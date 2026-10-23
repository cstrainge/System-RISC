//! The offline catalog reads the same optional annotations as the guest. Plain
//! entries keep their host names and bytes, including during lossless export.
use std::{collections::BTreeMap, fs::File, path::Path};

use mac_fork_file::{self as format, Kind, Metadata, ReadAt};
use mac_object_table::{Kind as ObjectKind, Table};

use crate::{Entry, Result, Volume, error, io::FileIo};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Key
{
    device:      u64,
    inode:       u64,
    incarnation: Option<[u8; 32]>
}
pub(super) struct Projection
{
    pins: Table<File>,
    ids:  BTreeMap<Key, u32>
}
impl Projection
{
    pub(super) fn new() -> Self
    {
        Self { pins: Table::new(ObjectKind::HostNodes),
               ids:  BTreeMap::new() }
    }

    fn id(&mut self, file: File, stored: Option<u32>) -> Result<u32>
    {
        let metadata = file.metadata()?;
        let incarnation = risc_host::object_identity(&metadata).map_err(|e| {
                                                                   error(format!("cannot qualify \
                                                                                  host identity: \
                                                                                  {e:?}"))
                                                               })?
                                                               .map(|v| *v.as_bytes());
        #[cfg(unix)]
        let key = {
            use std::os::unix::fs::MetadataExt;
            Key { device: metadata.dev(),
                  inode: metadata.ino(),
                  incarnation }
        };
        #[cfg(not(unix))]
        let key = return Err(error("host does not support pinned catalog identity"));
        if let Some(&id) = self.ids.get(&key)
        {
            if stored.is_some_and(|value| value != id)
            {
                return Err(error("annotation changed catalog identity during this operation"));
            }
            return Ok(id);
        }
        // An acknowledged atomic replacement keeps its logical catalog ID.
        // A prior resolve may still pin the now-unlinked inode in this tool;
        // retire only that dead binding. Two linked objects sharing an ID are
        // still corruption, never a reason to silently adopt one of them.
        if let Some(id) = stored
           && let Some(old) =
               self.ids.iter().find_map(|(key, value)| (*value == id).then_some(*key))
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let token = self.pins
                                .iter()
                                .find_map(|(token, file)| {
                                    let metadata = file.metadata().ok()?;
                                    (metadata.dev() == old.device
                                     && metadata.ino() == old.inode
                                     && metadata.nlink() == 0)
                                                              .then_some(token)
                                })
                                .ok_or_else(|| error("duplicate annotated catalog identity"))?;
                self.pins.remove(token);
                self.ids.remove(&old);
            }
            #[cfg(not(unix))]
            return Err(error("duplicate annotated catalog identity"));
        }
        let token =
            self.pins
                .insert(file)
                .map_err(|_| error("plain host catalog pin budget exceeded (1024 objects)"))?;
        let id = stored.unwrap_or(token | 0x8000_0000);
        if self.ids.values().any(|value| *value == id)
        {
            self.pins.remove(token);
            return Err(error("duplicate annotated catalog identity"));
        }
        self.ids.insert(key, id);
        Ok(id)
    }
}
impl Volume
{
    fn annotation(&self, file: &File, volume: [u8; 16])
                  -> Result<Option<[u8; format::annotation::BYTES]>>
    {
        let Some(identity) =
            risc_host::object_identity(&file.metadata()?).map_err(|e| {
                                                             error(format!("cannot read host \
                                                                            incarnation: {e:?}"))
                                                         })?
        else
        {
            return Ok(None);
        };
        let key = format::annotation::key(identity.as_bytes());
        let key = std::str::from_utf8(&key)?;
        let path = Path::new(format::annotation::DIRECTORY).join(key);
        let metadata = match self.dir.symlink_metadata(&path)
        {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into())
        };
        if !metadata.is_file() || metadata.len() != format::annotation::BYTES as u64
        {
            return Err(error("invalid host annotation: expected a 512-byte regular file"));
        }
        let mut snapshot = FileIo::snapshot(self.dir.open(&path)?.into_std())?;
        let mut bytes = [0; format::annotation::BYTES];
        snapshot.read_exact(0, &mut bytes)?;
        snapshot.validate()?;
        let record = format::annotation::Annotation::decode(&bytes)?;
        if record.object != *identity.as_bytes() || record.volume != volume
        {
            return Err(error("host annotation identity/volume mismatch"));
        }
        Ok(Some(bytes))
    }

    /// Caller holds the export writer lease. The binding is durable before
    /// a newly created entry is returned to the caller.
    pub(super) fn bind_created(&self, path: &Path, metadata: &Metadata) -> Result<()>
    {
        use std::io::Write;

        use cap_std::fs::OpenOptions;
        let physical = self.dir.open(path)?.into_std();
        let object =
            risc_host::object_identity(&physical.metadata()?).map_err(|e| {
                                                                 error(format!("cannot qualify \
                                                                                created object: \
                                                                                {e:?}"))
                                                             })?
                                                             .ok_or_else(|| {
                                                                 error("host cannot persist \
                                                                        catalog identity on this \
                                                                        filesystem")
                                                             })?;
        let directory = format::annotation::DIRECTORY;
        match self.dir.create_dir(directory)
        {
            Ok(()) => super::volume::sync(&self.dir)?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into())
        }
        let dir = self.dir.open_dir(directory)?;
        let name = format::annotation::key(object.as_bytes());
        let name = std::str::from_utf8(&name)?;
        let record = format::annotation::Annotation { volume:     metadata.volume,
                                                      object:     *object.as_bytes(),
                                                      catalog_id: metadata.catalog_id,
                                                      revision:   0,
                                                      patch:      Default::default() };
        let mut bytes = [0; format::annotation::BYTES];
        record.encode(&mut bytes)?;
        let mut stage = super::volume::Stage { dir:       &dir,
                                               name:      super::volume::stage_name()?,
                                               published: false };
        let mut file = dir.open_with(&stage.name, OpenOptions::new().write(true).create_new(true))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        dir.hard_link(&stage.name, &dir, name)?;
        dir.remove_file(&stage.name)?;
        stage.published = true;
        super::volume::sync(&dir)?;
        Ok(())
    }

    pub(super) fn observe(&self, path: &Path, parent: u32, volume: [u8; 16], kind: Kind,
                          name: &str)
                          -> Result<Entry>
    {
        let file = self.dir.open(path)?.into_std();
        let source = file.metadata()?;
        let mut snapshot = FileIo::snapshot(file.try_clone()?)?;
        if kind == Kind::File && !source.is_file() || kind == Kind::Directory && !source.is_dir()
        {
            return Err(error("host entry changed kind while opening"));
        }
        let annotation = self.annotation(&file, volume)?;
        let annotation =
            annotation.as_ref().map(format::annotation::Annotation::decode).transpose()?;
        let stored = if kind == Kind::Directory
        {
            let record = path.join(format::DIRECTORY_RECORD);
            match self.dir.symlink_metadata(&record)
            {
                Ok(_) => Some(super::volume::read_entry(&self.dir, &record, parent)?),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into())
            }
        }
        else
        {
            let mut prefix = [0; 8];
            if snapshot.len() >= prefix.len() as u64
            {
                snapshot.read_exact(0, &mut prefix)?;
            }
            if snapshot.len() >= prefix.len() as u64 && &prefix == format::MAGIC
            {
                Some(super::volume::read_pinned(FileIo::snapshot(file.try_clone()?)?,
                                                path,
                                                parent)?)
            }
            else
            {
                None
            }
        };
        let id =
            self.projection
                .lock()
                .map_err(|_| error("host catalog projection lock was poisoned; reopen the export"))?
                .id(file.try_clone()?, annotation.map(|a| a.catalog_id))?;
        let mut entry = if let Some(mut entry) = stored
        {
            if entry.metadata.kind != kind
            {
                return Err(error("host entry/container kind mismatch"));
            }
            // The header belongs to the source incarnation. Only qualified
            // annotations bind a physical object to this export's catalog.
            entry.metadata.volume = volume;
            entry.metadata.catalog_id = id;
            entry.metadata.identity = format::catalog_identity(volume, id)?;
            entry
        }
        else
        {
            let modified = source.modified()?;
            let date = |time| {
                risc_host::mac_date(time).map_err(|e| {
                                             error(format!("host timestamp is out of range: {e:?}"))
                                         })
            };
            let mut metadata =
                Metadata::new(kind, id, volume, name, date(source.created().unwrap_or(modified))?)?;
            metadata.modified = date(modified)?;
            let mut layout =
                format::Layout::new(&metadata,
                                    if kind == Kind::File { source.len() } else { 0 },
                                    0)?;
            // Plain data starts at zero, with no container/chunk framing.
            layout.data.offset = 0;
            Entry { metadata,
                    parent,
                    path: path.into(),
                    layout,
                    key: String::new(),
                    plain: true,
                    pinned: std::sync::Arc::new(FileIo::snapshot(file.try_clone()?)?),
                    directory: None }
        };
        if let Some(annotation) = annotation
        {
            annotation.apply(&mut entry.metadata)?;
        }
        entry.metadata.name = mac_hfs_b::names::normalize(name)?;
        entry.key = mac_hfs_b::names::comparison(&entry.metadata.name)?;
        snapshot.validate()?;
        Ok(entry)
    }
}
