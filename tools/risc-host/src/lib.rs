//! Host companion for explicitly selected VM directory exports. Native names,
//! Macintosh metadata and fork semantics remain in the guest filesystem driver.
#![forbid(unsafe_code)]

use std::{fs::{File, Metadata, TryLockError},
          io::{self, Read, Seek, SeekFrom, Write},
          path::Path,
          sync::Arc};

use cap_std::fs::{Dir, OpenOptions, ReadDir};
use mac_host_protocol::{self as wire, Header, Operation as Op, status};
use mac_object_table::{Kind, Table};
use sha2::{Digest, Sha256};
mod nodes;

pub type Result<T> = core::result::Result<T, Failure>;
#[derive(Debug)]
pub struct Failure(pub i32);
impl From<io::Error> for Failure
{
    fn from(error: io::Error) -> Self
    {
        Self(match error.kind()
             {
                 io::ErrorKind::NotFound => status::NOT_FOUND,
                 io::ErrorKind::AlreadyExists => status::CONFLICT,
                 io::ErrorKind::WouldBlock => status::BUSY,
                 io::ErrorKind::Unsupported => status::UNSUPPORTED,
                 _ => status::IO
             })
    }
}
impl From<wire::Error> for Failure
{
    fn from(_: wire::Error) -> Self
    {
        Self(status::INVALID)
    }
}
impl From<mac_object_table::Exhausted> for Failure
{
    fn from(_: mac_object_table::Exhausted) -> Self
    {
        Self(status::LIMIT)
    }
}
fn lock_error(error: TryLockError) -> Failure
{
    match error
    {
        TryLockError::WouldBlock => Failure(status::BUSY),
        TryLockError::Error(e) => e.into()
    }
}
fn sync(directory: &Dir) -> Result<()>
{
    Ok(directory.open(".")?.sync_all()?)
}
fn unique() -> Result<String>
{
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|_| Failure(status::UNAVAILABLE))?;
    Ok(format!(".system-risc-stage-{}",
               bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()))
}
fn fingerprint(metadata: &Metadata) -> Result<[u8; 32]>
{
    let mut digest = Sha256::new();
    digest.update(metadata.len().to_le_bytes());
    match metadata.modified()?.duration_since(std::time::UNIX_EPOCH)
    {
        Ok(time) =>
        {
            digest.update([0]);
            digest.update(time.as_secs().to_le_bytes());
            digest.update(time.subsec_nanos().to_le_bytes());
        }
        Err(error) =>
        {
            let time = error.duration();
            digest.update([1]);
            digest.update(time.as_secs().to_le_bytes());
            digest.update(time.subsec_nanos().to_le_bytes());
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        digest.update(metadata.dev().to_le_bytes());
        digest.update(metadata.ino().to_le_bytes());
        digest.update(metadata.ctime().to_le_bytes());
        digest.update(metadata.ctime_nsec().to_le_bytes());
    }
    #[cfg(not(unix))]
    {
        let time = metadata.created()?
                           .duration_since(std::time::UNIX_EPOCH)
                           .map_err(|_| Failure(status::UNSUPPORTED))?;
        digest.update(time.as_secs().to_le_bytes());
        digest.update(time.subsec_nanos().to_le_bytes());
    }
    Ok(digest.finalize().into())
}
fn regular(directory: &Dir, name: &str) -> Result<File>
{
    if !directory.symlink_metadata(name)?.is_file()
    {
        return Err(Failure(status::INVALID));
    }
    let file = directory.open(name)?.into_std();
    if !file.metadata()?.is_file()
    {
        return Err(Failure(status::INVALID));
    }
    Ok(file)
}

/// An export is opened once, by explicit host authority. All subsequent names
/// are checked components relative to this capability, including after renames.
pub struct Export
{
    name:     String,
    root:     Arc<Dir>,
    writable: bool
}
impl Export
{
    pub fn open(name: &str, path: &Path, writable: bool) -> Result<Self>
    {
        wire::component(name.as_bytes())?;
        if std::fs::symlink_metadata(path)?.file_type().is_symlink()
        {
            return Err(Failure(status::INVALID));
        }
        let root = Arc::new(Dir::open_ambient_dir(path, cap_std::ambient_authority())?);
        if writable
        {
            qualify_writes(&root)?;
        }
        Ok(Self { name: name.into(),
                  root,
                  writable })
    }
}
/// Qualify actual directory durability/publication, not just a QEMU byte channel.
fn move_noreplace(from: &Dir, old: &str, to: &Dir, new: &str) -> Result<()>
{
    #[cfg(target_os = "linux")]
    {
        rustix::fs::renameat_with(from, old, to, new, rustix::fs::RenameFlags::NOREPLACE).map_err(io::Error::from)?;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (from, old, to, new);
        Err(Failure(status::UNSUPPORTED))
    }
}
fn qualify_writes(root: &Dir) -> Result<()>
{
    #[cfg(not(target_os = "linux"))]
    {
        let _ = root;
        Err(Failure(status::UNSUPPORTED))
    }
    #[cfg(target_os = "linux")]
    {
        sync(root)?;
        let first = unique()?;
        let second = unique()?;
        let result = (|| {
            let mut file = root.open_with(&first, OpenOptions::new().write(true).create_new(true))?;
            file.write_all(b"System RISC publication probe")?;
            file.sync_all()?;
            root.hard_link(&first, root, &second)?;
            root.remove_file(&second)?;
            move_noreplace(root, &first, root, &second)?;
            root.rename(&second, root, &first)?;
            sync(root)
        })();
        let mut cleanup = Ok(());
        for name in [&first, &second]
        {
            match root.remove_file(name)
            {
                Ok(()) => (),
                Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                Err(e) => cleanup = Err(Failure::from(e))
            }
        }
        let flushed = sync(root);
        result?;
        cleanup?;
        flushed
    }
}

#[derive(Clone)]
struct Directory
{
    root:   Arc<Dir>,
    export: usize,
    owner:  u32
}
impl Directory
{
    fn revision(&self) -> Result<[u8; 32]>
    {
        fingerprint(&self.root.open(".")?.into_std().metadata()?)
    }
}
struct Enumeration
{
    directory: Directory,
    entries:   ReadDir,
    revision:  [u8; 32]
}
enum DirectoryObject
{
    Open(Directory),
    Enumerate(Enumeration)
}
struct Snapshot
{
    file:     File,
    revision: [u8; 32],
    length:   u64,
    export:   usize
}
impl Snapshot
{
    fn validate(&self) -> Result<()>
    {
        if fingerprint(&self.file.metadata()?)? != self.revision
        {
            return Err(Failure(status::STALE));
        }
        Ok(())
    }
}
enum FileObject
{
    Read(Snapshot),
    Lock
    {
        file:   File,
        export: usize,
        mode:   Option<u32>
    }
}
struct OwnedFile
{
    owner: u32,
    value: FileObject
}
struct Transaction
{
    owner:           u32,
    directory:       Directory,
    name:            String,
    file:            File,
    dirty:           bool,
    publication:     Option<String>,
    directory_stage: Option<String>
}
impl Drop for Transaction
{
    fn drop(&mut self)
    {
        let _ = self.directory.root.remove_file(&self.name);
        if let Some(name) = &self.directory_stage
        {
            let _ = self.directory.root.remove_dir_all(name);
        }
    }
}

/// One connection incarnation. All live IDs use shared generation-tagged
/// namespaces. Separate owner validation remains explicit in every lookup.
pub struct Session
{
    exports:      Arc<Vec<Export>>,
    sessions:     Table<()>,
    id:           u32,
    hello:        bool,
    directories:  Table<DirectoryObject>,
    files:        Table<OwnedFile>,
    transactions: Table<Transaction>,
    nodes:        Table<nodes::Node>
}
impl Session
{
    pub fn new(exports: Arc<Vec<Export>>) -> Result<Self>
    {
        if exports.len() > 16
        {
            return Err(Failure(status::LIMIT));
        }
        let mut sessions = Table::new(Kind::HostSessions);
        let id = sessions.insert(())?;
        Ok(Self { exports,
                  sessions,
                  id,
                  hello: false,
                  directories: Table::new(Kind::HostDirectories),
                  files: Table::new(Kind::HostFiles),
                  transactions: Table::new(Kind::HostTransactions),
                  nodes: Table::new(Kind::HostNodes) })
    }

    fn directory(&self, id: u32) -> Result<Directory>
    {
        match self.directories.get(id)
        {
            Some(DirectoryObject::Open(dir)) if dir.owner == self.id => Ok(dir.clone()),
            _ => Err(Failure(status::STALE))
        }
    }

    fn writable(&self, export: usize) -> Result<()>
    {
        if !self.exports.get(export).is_some_and(|e| e.writable)
        {
            return Err(Failure(status::READ_ONLY));
        }
        Ok(())
    }

    fn snapshot(&self, id: u32, export: usize) -> Result<&Snapshot>
    {
        match self.files.get(id)
        {
            Some(OwnedFile { owner,
                             value: FileObject::Read(snapshot) })
                if *owner == self.id && snapshot.export == export =>
            {
                snapshot.validate()?;
                Ok(snapshot)
            }
            _ => Err(Failure(status::STALE))
        }
    }

    fn transaction(&mut self, id: u32) -> Result<&mut Transaction>
    {
        self.transactions.get_mut(id).filter(|t| t.owner == self.id).ok_or(Failure(status::STALE))
    }

    /// Execute one already framed request. At most one bounded payload is read or
    /// written. Host disk work runs in the companion, outside the guest kernel.
    pub fn respond(&mut self, request: Header, payload: &[u8]) -> (Header, Vec<u8>)
    {
        let mut response = request.response();
        let result = self.execute(request, payload, &mut response);
        let bytes = match result
        {
            Ok(bytes) => bytes,
            Err(error) =>
            {
                response.status = error.0;
                response.object = 0;
                response.offset = 0;
                response.value = 0;
                Vec::new()
            }
        };
        response.payload = bytes.len() as u32;
        (response, bytes)
    }

    fn execute(&mut self, req: Header, payload: &[u8], reply: &mut Header) -> Result<Vec<u8>>
    {
        if req.reply
           || req.payload as usize != payload.len()
           || payload.len() > wire::MAX_PAYLOAD
           || req.status != 0
           || req.request == 0
        {
            return Err(Failure(status::INVALID));
        }
        if req.operation == Op::Hello
        {
            if self.hello
               || req.session != 0
               || req.object != 0
               || req.offset != 0
               || req.value != 0
               || !payload.is_empty()
            {
                return Err(Failure(status::INVALID));
            }
            self.hello = true;
            reply.session = self.id;
            reply.value = self.exports.len() as u32;
            return Ok(Vec::new());
        }
        if !self.hello || req.session != self.id || self.sessions.get(req.session).is_none()
        {
            return Err(Failure(status::STALE));
        }
        // The operation table fixes which fields carry arguments. Reject unused
        // bits rather than accidentally granting new behavior to malformed frames.
        let permits_payload = matches!(req.operation,
                                       Op::OpenDirectory
                                       | Op::OpenRead
                                       | Op::Write
                                       | Op::Commit
                                       | Op::MakeDirectory
                                       | Op::Rename
                                       | Op::Remove
                                       | Op::OpenLock);
        let permits_payload =
            permits_payload || matches!(req.operation, Op::Describe | Op::PublishDirectory);
        let permits_offset =
            matches!(req.operation, Op::Export | Op::Read | Op::Write | Op::SetLength | Op::Rename);
        let permits_value =
            matches!(req.operation, Op::Read | Op::Commit | Op::Rename | Op::Remove | Op::Lock);
        if !permits_payload && !payload.is_empty()
           || !permits_offset && req.offset != 0
           || !permits_value && req.value != 0
        {
            return Err(Failure(status::INVALID));
        }
        match req.operation
        {
            Op::Hello => unreachable!(),
            Op::Export =>
            {
                if req.object != 0
                {
                    return Err(Failure(status::INVALID));
                }
                let index = usize::try_from(req.offset).map_err(|_| Failure(status::NOT_FOUND))?;
                let export = self.exports.get(index).ok_or(Failure(status::NOT_FOUND))?;
                let directory = Directory { root:   export.root.clone(),
                                            export: index,
                                            owner:  self.id };
                let name = export.name.as_bytes().to_vec();
                reply.value = if export.writable { wire::ALL_CAPABILITIES } else { wire::READ };
                reply.object = self.directories.insert(DirectoryObject::Open(directory))?;
                return Ok(name);
            }
            Op::OpenDirectory =>
            {
                let parent = self.directory(req.object)?;
                let name = wire::component(payload)?;
                if !parent.root.symlink_metadata(name)?.is_dir()
                {
                    return Err(Failure(status::INVALID));
                }
                let child = Directory { root: Arc::new(parent.root.open_dir(name)?),
                                        ..parent };
                reply.object = self.directories.insert(DirectoryObject::Open(child))?;
            }
            Op::ListBegin =>
            {
                let directory = self.directory(req.object)?;
                let revision = directory.revision()?;
                let entries = directory.root.entries()?;
                reply.object = self.directories
                                   .insert(DirectoryObject::Enumerate(Enumeration { directory,
                                                                                    entries,
                                                                                    revision }))?;
            }
            Op::ListNext =>
            {
                let enumeration = match self.directories.get_mut(req.object)
                {
                    Some(DirectoryObject::Enumerate(e)) if e.directory.owner == self.id => e,
                    _ => return Err(Failure(status::STALE))
                };
                if enumeration.revision != enumeration.directory.revision()?
                {
                    return Err(Failure(status::STALE));
                }
                // Skip only this connection's live stages, never crash leftovers.
                for _ in 0..32
                {
                    let Some(entry) = enumeration.entries.next()
                    else
                    {
                        return Ok(Vec::new());
                    };
                    let entry = entry?;
                    let name = entry.file_name();
                    let name = name.to_str().ok_or(Failure(status::INVALID))?;
                    if self.transactions.iter().any(|(_, t)| t.name == name || t.directory_stage.as_deref() == Some(name))
                    {
                        continue;
                    }
                    wire::component(name.as_bytes())?;
                    let kind = entry.file_type()?;
                    reply.value = if kind.is_file()
                    {
                        1
                    }
                    else if kind.is_dir()
                    {
                        2
                    }
                    else
                    {
                        return Err(Failure(status::INVALID));
                    };
                    let bytes = name.as_bytes().to_vec();
                    if enumeration.revision != enumeration.directory.revision()?
                    {
                        return Err(Failure(status::STALE));
                    }
                    return Ok(bytes);
                }
                return Err(Failure(status::BUSY));
            }
            Op::OpenRead =>
            {
                let directory = self.directory(req.object)?;
                let file = regular(&directory.root, wire::component(payload)?)?;
                file.try_lock_shared().map_err(lock_error)?;
                let metadata = file.metadata()?;
                let length = metadata.len();
                let revision = fingerprint(&metadata)?;
                reply.object = self.files.insert(OwnedFile { owner: self.id, value: FileObject::Read(Snapshot { file, revision, length, export: directory.export }) })?;
                reply.offset = length;
                return Ok(revision.to_vec());
            }
            Op::Read =>
            {
                if req.value as usize > wire::MAX_PAYLOAD
                {
                    return Err(Failure(status::LIMIT));
                }
                let owned = self.files
                                .get_mut(req.object)
                                .filter(|f| f.owner == self.id)
                                .ok_or(Failure(status::STALE))?;
                let snapshot = match &mut owned.value
                {
                    FileObject::Read(f) => f,
                    _ => return Err(Failure(status::INVALID))
                };
                snapshot.validate()?;
                let length =
                    snapshot.length.saturating_sub(req.offset).min(req.value as u64) as usize;
                let mut bytes = vec![0; length];
                if length != 0
                {
                    snapshot.file.seek(SeekFrom::Start(req.offset))?;
                    snapshot.file.read_exact(&mut bytes)?;
                }
                snapshot.validate()?;
                return Ok(bytes);
            }
            Op::Close =>
            {
                if self.files.get(req.object).is_some_and(|f| f.owner == self.id)
                {
                    self.files.remove(req.object);
                }
                else if self.transactions.get(req.object).is_some_and(|t| t.owner == self.id)
                {
                    self.transactions.remove(req.object);
                }
                else
                {
                    self.directory_or_enumeration(req.object)?;
                    self.directories.remove(req.object);
                }
            }
            Op::BeginWrite =>
            {
                let directory = self.directory(req.object)?;
                self.writable(directory.export)?;
                let name = unique()?;
                let file =
                    directory.root
                             .open_with(&name,
                                        OpenOptions::new().read(true).write(true).create_new(true))?
                             .into_std();
                let transaction = Transaction { owner: self.id,
                                                directory,
                                                name,
                                                file,
                                                dirty: true,
                                                publication: None,
                                                directory_stage: None };
                reply.object = self.transactions.insert(transaction)?;
            }
            Op::Write =>
            {
                let transaction = self.transaction(req.object)?;
                if transaction.publication.is_some()
                {
                    return Err(Failure(status::CONFLICT));
                }
                req.offset.checked_add(payload.len() as u64).ok_or(Failure(status::INVALID))?;
                if !payload.is_empty()
                {
                    transaction.file.seek(SeekFrom::Start(req.offset))?;
                    transaction.file.write_all(payload)?;
                    transaction.dirty = true;
                }
                reply.value = payload.len() as u32;
            }
            Op::SetLength =>
            {
                let transaction = self.transaction(req.object)?;
                if transaction.publication.is_some()
                {
                    return Err(Failure(status::CONFLICT));
                }
                if transaction.file.metadata()?.len() != req.offset
                {
                    transaction.file.set_len(req.offset)?;
                    transaction.dirty = true;
                }
            }
            Op::Flush =>
            {
                let transaction = self.transaction(req.object)?;
                if transaction.dirty
                {
                    transaction.file.sync_all()?;
                    transaction.dirty = false;
                }
            }
            Op::Commit =>
            {
                self.commit(req.object, wire::component(payload)?, req.value)?;
            }
            Op::MakeDirectory =>
            {
                let directory = self.directory(req.object)?;
                self.writable(directory.export)?;
                directory.root.create_dir(wire::component(payload)?)?;
                sync(&directory.root)?;
            }
            Op::Rename =>
            {
                let from = self.directory(req.object)?;
                let to = self.directory(u32::try_from(req.offset).map_err(|_| {
                                                                     Failure(status::INVALID)
                                                                 })?)?;
                if from.export != to.export
                {
                    return Err(Failure(status::INVALID));
                }
                self.writable(from.export)?;
                let (old, new) = wire::rename_names(payload)?;
                self.check_source(&from, old, req.value)?;
                // Native host tokens are globally unique. Never replace an
                // existing destination while moving a logical catalog object.
                if to.root.symlink_metadata(new).is_ok()
                {
                    return Err(Failure(status::CONFLICT));
                }
                move_noreplace(&from.root, old, &to.root, new)?;
                self.check_moved(&to, new, req.value)?;
                sync(&from.root)?;
                if !Arc::ptr_eq(&from.root, &to.root)
                {
                    sync(&to.root)?;
                }
            }
            Op::Remove =>
            {
                let directory = self.directory(req.object)?;
                self.writable(directory.export)?;
                let name = wire::component(payload)?;
                let is_dir = self.check_source(&directory, name, req.value)?;
                if is_dir
                {
                    directory.root.remove_dir(name)?;
                }
                else
                {
                    directory.root.remove_file(name)?;
                }
                sync(&directory.root)?;
            }
            Op::SyncDirectory =>
            {
                let directory = self.directory(req.object)?;
                self.writable(directory.export)?;
                sync(&directory.root)?;
            }
            Op::OpenLock =>
            {
                let directory = self.directory(req.object)?;
                let file = regular(&directory.root, wire::component(payload)?)?;
                reply.object =
                    self.files.insert(OwnedFile { owner: self.id,
                                                   value: FileObject::Lock { file,
                                                                             export:
                                                                                 directory.export,
                                                                             mode: None } })?;
            }
            Op::Lock | Op::Unlock =>
            {
                let owned = self.files
                                .get_mut(req.object)
                                .filter(|f| f.owner == self.id)
                                .ok_or(Failure(status::STALE))?;
                match &mut owned.value
                {
                    FileObject::Lock { file,
                                       export,
                                       mode } =>
                    {
                        if req.operation == Op::Unlock
                        {
                            if mode.is_some()
                            {
                                file.unlock()?;
                                *mode = None;
                            }
                        }
                        else
                        {
                            if req.value > 1
                            {
                                return Err(Failure(status::INVALID));
                            }
                            if req.value == 1 && !self.exports[*export].writable
                            {
                                return Err(Failure(status::READ_ONLY));
                            }
                            if *mode != Some(req.value)
                            {
                                if mode.is_some()
                                {
                                    return Err(Failure(status::CONFLICT));
                                }
                                if req.value == 1
                                {
                                    file.try_lock()
                                }
                                else
                                {
                                    file.try_lock_shared()
                                }.map_err(lock_error)?;
                                *mode = Some(req.value);
                            }
                        }
                    }
                    _ => return Err(Failure(status::INVALID))
                }
            }
            Op::Stat =>
            {
                if let Some(OwnedFile { owner,
                                        value: FileObject::Read(snapshot) }) =
                    self.files.get(req.object)
                {
                    if *owner != self.id
                    {
                        return Err(Failure(status::STALE));
                    }
                    snapshot.validate()?;
                    reply.offset = snapshot.length;
                    reply.value = 1;
                    return Ok(snapshot.revision.to_vec());
                }
                let directory = self.directory(req.object)?;
                reply.value = 2;
                return Ok(directory.revision()?.to_vec());
            }
            Op::Describe => return self.describe(req.object, wire::component(payload)?, reply),
            Op::PublishDirectory =>
            {
                let (name, record) = wire::rename_names(payload)?;
                reply.object = self.publish_directory(req.object, name, record)?;
            }
        }
        Ok(Vec::new())
    }

    fn directory_or_enumeration(&self, id: u32) -> Result<()>
    {
        match self.directories.get(id)
        {
            Some(DirectoryObject::Open(d)) if d.owner == self.id => Ok(()),
            Some(DirectoryObject::Enumerate(e)) if e.directory.owner == self.id => Ok(()),
            _ => Err(Failure(status::STALE))
        }
    }

    fn check_source(&self, parent: &Directory, name: &str, expected: u32) -> Result<bool>
    {
        if let Ok(snapshot) = self.snapshot(expected, parent.export)
        {
            let current = regular(&parent.root, name)?;
            if fingerprint(&current.metadata()?)? != snapshot.revision
            {
                return Err(Failure(status::CONFLICT));
            }
            Ok(false)
        }
        else
        {
            let directory = self.directory(expected)?;
            if directory.export != parent.export || !parent.root.symlink_metadata(name)?.is_dir()
            {
                return Err(Failure(status::CONFLICT));
            }
            let current = parent.root.open_dir(name)?;
            if fingerprint(&current.open(".")?.into_std().metadata()?)? != directory.revision()?
            {
                return Err(Failure(status::CONFLICT));
            }
            Ok(true)
        }
    }

    fn check_moved(&self, directory: &Directory, name: &str, expected: u32) -> Result<()>
    {
        let old = if let Some(OwnedFile { owner,
                                          value: FileObject::Read(snapshot) }) =
            self.files.get(expected)
        {
            if *owner != self.id || snapshot.export != directory.export
            {
                return Err(Failure(status::STALE));
            }
            snapshot.file.metadata()?
        }
        else
        {
            let old = self.directory(expected)?;
            if old.export != directory.export
            {
                return Err(Failure(status::STALE));
            }
            old.root.open(".")?.into_std().metadata()?
        };
        let current = directory.root.open(name)?.into_std().metadata()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if old.dev() != current.dev() || old.ino() != current.ino()
            {
                return Err(Failure(status::CONFLICT));
            }
        }
        #[cfg(not(unix))]
        if fingerprint(&old)? != fingerprint(&current)?
        {
            return Err(Failure(status::CONFLICT));
        }
        Ok(())
    }

    fn commit(&mut self, id: u32, name: &str, expected: u32) -> Result<()>
    {
        if self.transaction(id)?.directory_stage.is_some()
        {
            return Err(Failure(status::INVALID));
        }
        let directory = self.transactions
                            .get(id)
                            .filter(|t| t.owner == self.id)
                            .ok_or(Failure(status::STALE))?
                            .directory
                            .clone();
        let published = self.transactions.get(id).unwrap().publication.as_deref();
        if let Some(prior) = published
        {
            if prior != name
            {
                return Err(Failure(status::CONFLICT));
            }
        }
        else
        {
            if expected != 0
            {
                self.check_source(&directory, name, expected)?;
            }
            let transaction = self.transaction(id)?;
            if transaction.dirty
            {
                transaction.file.sync_all()?;
                transaction.dirty = false;
            }
            if expected == 0
            {
                directory.root.hard_link(&transaction.name, &directory.root, name)?;
            }
            else
            {
                directory.root.rename(&transaction.name, &directory.root, name)?;
            }
            transaction.publication = Some(name.into());
        }
        let transaction = self.transaction(id)?;
        match directory.root.remove_file(&transaction.name)
        {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into())
        }
        sync(&directory.root)?;
        self.transactions.remove(id);
        Ok(())
    }

    /// Publish a directory containing the already flushed staged record. The
    /// target is never visible without its record; retries after a durability
    /// failure retain the original transaction and do not publish twice.
    fn publish_directory(&mut self, id: u32, name: &str, record: &str) -> Result<u32>
    {
        self.directories.reserve()?;
        let export = self.transaction(id)?.directory.export;
        let owner = self.id;
        let transaction = self.transaction(id)?;
        let parent = transaction.directory.root.clone();
        let directory = if let Some(published) = &transaction.publication
        {
            if published != name
            {
                return Err(Failure(status::CONFLICT));
            }
            parent.open_dir(name)?
        }
        else
        {
            if transaction.dirty
            {
                return Err(Failure(status::INVALID));
            }
            let stage = if let Some(stage) = &transaction.directory_stage
            {
                stage.clone()
            }
            else
            {
                let stage = unique()?;
                parent.create_dir(&stage)?;
                transaction.directory_stage = Some(stage.clone());
                stage
            };
            let directory = parent.open_dir(&stage)?;
            if parent.symlink_metadata(&transaction.name).is_ok()
            {
                move_noreplace(&parent, &transaction.name, &directory, record)?;
            }
            directory
        };
        let root = Arc::new(directory);
        let capability = self.directories
                             .insert(DirectoryObject::Open(Directory { root: root.clone(),
                                                                       export,
                                                                       owner }))?;
        let result = (|| {
            sync(&root)?;
            let transaction = self.transaction(id)?;
            if transaction.publication.is_none()
            {
                let stage = transaction.directory_stage.as_ref().ok_or(Failure(status::INVALID))?;
                move_noreplace(&parent, stage, &parent, name)?;
                transaction.publication = Some(name.into());
            }
            sync(&parent)
        })();
        if let Err(error) = result
        {
            self.directories.remove(capability);
            return Err(error);
        }
        self.transactions.remove(id);
        Ok(capability)
    }
}

/// Blocking host transport; the guest uses bounded virtqueue polling and never
/// waits for this I/O while holding a resident File Manager lock.
pub fn serve_connection(stream: &mut impl ReadWrite, exports: Arc<Vec<Export>>) -> io::Result<()>
{
    let mut session =
        Session::new(exports).map_err(|e| io::Error::other(format!("session: {}", e.0)))?;
    loop
    {
        let mut bytes = [0; wire::HEADER_BYTES];
        // EOF between packets is a clean disconnect; partial headers are errors.
        match stream.read(&mut bytes[..1])?
        {
            0 => return Ok(()),
            1 => (),
            _ => unreachable!()
        }
        stream.read_exact(&mut bytes[1..])?;
        let request =
            Header::decode(&bytes).map_err(|e| io::Error::other(format!("protocol: {e:?}")))?;
        let mut payload = vec![0; request.payload as usize];
        stream.read_exact(&mut payload)?;
        let (reply, payload) = session.respond(request, &payload);
        stream.write_all(&reply.encode()
                               .map_err(|e| io::Error::other(format!("reply: {e:?}")))?)?;
        stream.write_all(&payload)?;
        stream.flush()?;
    }
}
pub trait ReadWrite: Read + Write
{
}
impl<T: Read + Write> ReadWrite for T
{
}
