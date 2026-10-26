//! Plain host entries retain their bytes/names. Catalog identity is backed by
//! an open object pin, so inode reuse cannot alias a deleted session object.
#![deny(unsafe_code)]
use super::*;
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Key
{
    export:   usize,
    physical: [u8; 24],
    identity: Option<wire::ObjectIdentity>
}
impl Key
{
    fn new(export: usize, file: &File, identity: Option<wire::ObjectIdentity>) -> Result<Self>
    {
        let physical = if let Some(identity) = identity
        {
            let mut key = [0; 24];
            #[cfg(windows)]
            key.copy_from_slice(&identity.as_bytes()[..24]);
            #[cfg(not(windows))]
            key[..16].copy_from_slice(&identity.as_bytes()[..16]);
            key
        }
        else
        {
            physical_key(file)?
        };
        Ok(Self { export,
                  physical,
                  identity })
    }
}
/// Physical identity from a pinned handle. Windows uses the complete ReFS /
/// NTFS identifier; the legacy 64-bit file index is not unique on ReFS.
pub fn physical_key(file: &File) -> Result<[u8; 24]>
{
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        let mut bytes = [0; 24];
        bytes[..8].copy_from_slice(&metadata.dev().to_le_bytes());
        bytes[8..16].copy_from_slice(&metadata.ino().to_le_bytes());
        Ok(bytes)
    }
    #[cfg(windows)]
    {
        windows_physical_key(file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Err(Failure(status::UNSUPPORTED))
    }
}
#[cfg(windows)]
#[allow(unsafe_code)]
fn windows_physical_key(file: &File) -> Result<[u8; 24]>
{
    use windows_sys::Win32::Storage::FileSystem::{FILE_ID_INFO, FileIdInfo,
                                                  GetFileInformationByHandleEx};
    let mut info = FILE_ID_INFO::default();
    // SAFETY: the owned handle remains valid, and the aligned output buffer
    // has the exact size required by FileIdInfo for this synchronous call.
    if unsafe {
        GetFileInformationByHandleEx(file.as_raw_handle(),
                                     FileIdInfo,
                                     (&mut info as *mut FILE_ID_INFO).cast(),
                                     std::mem::size_of::<FILE_ID_INFO>() as u32)
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    if info.FileId.Identifier == [0; 16] || info.FileId.Identifier == [255; 16]
    {
        return Err(Failure(status::UNSUPPORTED));
    }
    let mut bytes = [0; 24];
    bytes[..8].copy_from_slice(&info.VolumeSerialNumber.to_le_bytes());
    bytes[8..].copy_from_slice(&info.FileId.Identifier);
    Ok(bytes)
}
pub(super) struct Node
{
    #[cfg(windows)]
    pin: Arc<windows_pins::Pin>,
    #[cfg(not(windows))]
    pin: Arc<File>,
    key: Key
}
/// Exact source incarnation on supported Unix filesystems. Birth time is kept
/// at nanosecond resolution, independent of the editable guest creation date.
/// A live pin still supplies the stronger within-session no-reuse guarantee.
pub fn object_identity(file: &File) -> Result<Option<wire::ObjectIdentity>>
{
    object_identity_metadata(file, &file.metadata()?)
}
pub(super) fn object_identity_metadata(file: &File, metadata: &Metadata)
                                       -> Result<Option<wire::ObjectIdentity>>
{
    #[cfg(unix)]
    {
        let _ = file;
        use std::os::unix::fs::MetadataExt;
        let Ok(birth) = metadata.created()
        else
        {
            return Ok(None);
        };
        let (seconds, nanos) = match birth.duration_since(std::time::UNIX_EPOCH)
        {
            Ok(d) =>
            {
                (i64::try_from(d.as_secs()).map_err(|_| Failure(status::INVALID))?,
                 d.subsec_nanos())
            }
            Err(error) =>
            {
                let d = error.duration();
                let seconds = i64::try_from(d.as_secs()).map_err(|_| Failure(status::INVALID))?;
                if d.subsec_nanos() == 0
                {
                    (-seconds, 0)
                }
                else
                {
                    (-seconds.checked_add(1).ok_or(Failure(status::INVALID))?,
                     1_000_000_000 - d.subsec_nanos())
                }
            }
        };
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&metadata.dev().to_le_bytes());
        bytes[8..16].copy_from_slice(&metadata.ino().to_le_bytes());
        bytes[16..24].copy_from_slice(&seconds.to_le_bytes());
        bytes[24..28].copy_from_slice(&nanos.to_le_bytes());
        Ok(Some(wire::ObjectIdentity::from_bytes(bytes)?))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let physical = physical_key(file)?;
        let mut bytes = [0; 32];
        bytes[..24].copy_from_slice(&physical);
        bytes[24..].copy_from_slice(&metadata.creation_time().to_le_bytes());
        Ok(Some(wire::ObjectIdentity::from_bytes(bytes)?))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = metadata;
        Ok(None)
    }
}
pub fn date(time: std::time::SystemTime) -> Result<u64>
{
    use std::time::UNIX_EPOCH;
    let (s, us) = match time.duration_since(UNIX_EPOCH)
    {
        Ok(d) =>
        {
            (i64::try_from(d.as_secs()).map_err(|_| Failure(status::INVALID))?, d.subsec_micros())
        }
        Err(error) =>
        {
            let d = error.duration();
            let s = i64::try_from(d.as_secs()).map_err(|_| Failure(status::INVALID))?;
            if d.subsec_nanos() == 0
            {
                (-s, 0)
            }
            else
            {
                (-s.checked_add(1).ok_or(Failure(status::INVALID))?,
                 1_000_000 - d.subsec_nanos().div_ceil(1000))
            }
        }
    };
    mac_hfs_b::time::Timestamp::from_unix(s, us).map(|t| t.0).map_err(|_| Failure(status::INVALID))
}
impl Session
{
    pub(super) fn describe(&mut self, parent: u32, name: &str, reply: &mut Header)
                           -> Result<Vec<u8>>
    {
        let directory = self.directory(parent)?;
        #[cfg(windows)]
        directory.changes.observe_entry(name)?;
        let kind = directory.root.symlink_metadata(name)?.file_type();
        if !kind.is_file() && !kind.is_dir()
        {
            return Err(Failure(status::INVALID));
        }
        let pin = if kind.is_dir()
        {
            pin_directory(directory.root.open_dir(name)?)?.into_std_file()
        }
        else
        {
            directory.root.open(name)?.into_std()
        };
        let metadata = pin.metadata()?;
        if metadata.is_file() != kind.is_file() || metadata.is_dir() != kind.is_dir()
        {
            return Err(Failure(status::STALE));
        }
        #[cfg(windows)]
        if !directory.private && !name.starts_with(".system-risc-")
        {
            self.ensure_node_pin(directory.export, &pin, Some((&directory.changes, name)))?;
        }
        self.describe_pin(directory.export, pin, reply)
    }

    pub(super) fn describe_object(&mut self, object: u32, reply: &mut Header) -> Result<Vec<u8>>
    {
        let (export, pin) = if let Ok(directory) = self.directory(object)
        {
            (directory.export, pin_directory_file(&directory.root)?)
        }
        else if let Some(file) = self.files.get(object).filter(|f| f.owner == self.id)
        {
            match &file.value
            {
                FileObject::Read(snapshot) =>
                {
                    snapshot.validate()?;
                    (snapshot.export, snapshot.file.try_clone()?)
                }
                FileObject::Lock { export,
                                   file,
                                   .. } => (*export, file.try_clone()?)
            }
        }
        else if let Some(stage) = self.transactions.get(object).filter(|t| t.owner == self.id)
        {
            (stage.directory.export, stage.file.try_clone()?)
        }
        else
        {
            return Err(Failure(status::STALE));
        };
        self.describe_pin(export, pin, reply)
    }

    pub(super) fn ensure_node_pin(&mut self, export: usize, pin: &File,
                                  origin: Option<(&Arc<changes::Changes>, &str)>)
                                  -> Result<u32>
    {
        #[cfg(not(windows))]
        let _ = origin;
        let identity = object_identity(pin)?;
        let key = Key::new(export, pin, identity)?;
        let id = match self.node_index.binary_search_by_key(&key, |entry| entry.0)
        {
            Ok(at) =>
            {
                let id = self.node_index[at].1;
                let node = self.nodes.get(id).ok_or(Failure(status::STALE))?;
                #[cfg(not(windows))]
                if node.key != key
                   || Key::new(export, &node.pin, object_identity(&node.pin)?)? != key
                {
                    return Err(Failure(status::STALE));
                }
                #[cfg(windows)]
                {
                    let state = node.pin.lock()?;
                    if node.key != key
                       || Key::new(export, &state.file, object_identity(&state.file)?)? != key
                    {
                        return Err(Failure(status::STALE));
                    }
                    let upgrade =
                        !state.detached && self.exports[export].writable && origin.is_some();
                    drop(state);
                    if upgrade
                    {
                        windows_pins::acquire(&self.exports[export].root,
                                              origin,
                                              pin,
                                              identity.ok_or(Failure(status::UNSUPPORTED))?,
                                              true)?;
                    }
                }
                id
            }
            Err(at) =>
            {
                self.node_index.try_reserve(1).map_err(|_| Failure(status::LIMIT))?;
                let stored = {
                    #[cfg(windows)]
                    {
                        windows_pins::acquire(&self.exports[export].root,
                                              origin,
                                              pin,
                                              identity.ok_or(Failure(status::UNSUPPORTED))?,
                                              self.exports[export].writable)?
                    }
                    #[cfg(not(windows))]
                    {
                        Arc::new(pin.try_clone()?)
                    }
                };
                let id = self.nodes.insert(Node { pin: stored,
                                                   key })?;
                self.node_index.insert(at, (key, id));
                id
            }
        };
        Ok(id)
    }

    fn describe_pin(&mut self, export: usize, pin: File, reply: &mut Header) -> Result<Vec<u8>>
    {
        let id = self.ensure_node_pin(export, &pin, None)?;
        let metadata = pin.metadata()?;
        let identity = object_identity_metadata(&pin, &metadata)?;
        let revision = file_revision(&pin)?;
        reply.object = id;
        reply.value = if metadata.is_dir() { 2 } else { 1 };
        reply.offset = if metadata.is_file() { metadata.len() } else { 0 };
        let modified = metadata.modified()?;
        Ok(wire::Description { created: date(metadata.created().unwrap_or(modified))?,
                               modified: date(modified)?,
                               identity,
                               revision }.encode()
                                         .to_vec())
    }
}
