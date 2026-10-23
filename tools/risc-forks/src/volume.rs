use std::{collections::{BTreeMap, BTreeSet},
          fs::File,
          io::Write,
          path::{Path, PathBuf}};

use cap_std::fs::{Dir, OpenOptions};
use mac_fork_file::{self as format, CHUNK_BYTES, DIRECTORY_RECORD, Opened, ROOT_RECORD, ReadAt,
                    Writer};

use crate::{Fork, Kind, LOCK_RECORD, Metadata, Result, error,
            io::{FileIo, Slice}};
const STAGE_PREFIX: &str = ".system-risc-stage-";
const MAX_ENTRIES: usize = 100_000;
const MAX_DEPTH: usize = 128;
#[derive(Clone)]
pub struct Entry
{
    pub metadata:         Metadata,
    pub parent:           u32,
    pub path:             PathBuf,
    pub layout:           format::Layout,
    pub(super) key:       String,
    pub(super) plain:     bool,
    pub(super) pinned:    std::sync::Arc<FileIo>,
    pub(super) directory: Option<std::sync::Arc<FileIo>>
}
impl Entry
{
    fn validate(&self) -> Result<()>
    {
        self.pinned.validate()?;
        if let Some(directory) = &self.directory
        {
            directory.validate()?;
        }
        Ok(())
    }
}

pub struct Volume
{
    pub(super) dir:        Dir,
    pub(super) projection: std::sync::Mutex<super::projection::Projection>
}
pub(super) fn sync(dir: &Dir) -> Result<()>
{
    // A capability directory may use an O_PATH descriptor. Open a real read
    // descriptor relative to that capability before requesting durability.
    dir.open(".")?.sync_all()?;
    Ok(())
}
pub(super) struct Stage<'a>
{
    pub(super) dir:       &'a Dir,
    pub(super) name:      String,
    pub(super) published: bool
}
impl Drop for Stage<'_>
{
    fn drop(&mut self)
    {
        if !self.published
        {
            let _ = self.dir.remove_file(&self.name);
        }
    }
}
pub(super) fn stage_name() -> Result<String>
{
    let mut id = [0; 16];
    getrandom::fill(&mut id).map_err(|e| error(format!("entropy unavailable: {e}")))?;
    Ok(format!("{STAGE_PREFIX}{}", id.iter().map(|b| format!("{b:02x}")).collect::<String>()))
}
fn write_file(dir: &Dir, name: &str, meta: &Metadata, data: &mut impl ReadAt,
              resources: &mut impl ReadAt, replace: bool)
              -> Result<()>
{
    data.validate()?;
    resources.validate()?;
    let mut stage = Stage { dir,
                            name: stage_name()?,
                            published: false };
    let file = dir.open_with(&stage.name,
                             OpenOptions::new().read(true).write(true).create_new(true))?
                  .into_std();
    let mut writer = Writer::begin(FileIo::new(file)?, meta, data.len(), resources.len())?;
    let mut buffer = [0; CHUNK_BYTES];
    for (kind, source) in
        [(Fork::Data, data as &mut dyn ReadAt), (Fork::Resource, resources as &mut dyn ReadAt)]
    {
        let mut at = 0;
        while at < source.len()
        {
            let count = (source.len() - at).min(CHUNK_BYTES as u64) as usize;
            source.read_exact(at, &mut buffer[..count])?;
            writer.write_chunk(kind, &buffer[..count])?;
            at += count as u64;
        }
    }
    data.validate()?;
    resources.validate()?;
    writer.finish()?;
    if replace
    {
        dir.rename(&stage.name, dir, name)?;
    }
    else
    {
        dir.hard_link(&stage.name, dir, name)?;
        dir.remove_file(&stage.name)?;
    }
    stage.published = true;
    // A publication followed by a flush error is an error, never reported as a
    // durable commit. Root counters advance before publication and never reuse IDs.
    sync(dir)?;
    Ok(())
}
pub(super) fn read_entry(dir: &Dir, path: &Path, parent: u32) -> Result<Entry>
{
    let kind = dir.symlink_metadata(path)?.file_type();
    if kind.is_symlink() || !kind.is_file()
    {
        return Err(error("container must be a regular file"));
    }
    let file = dir.open(path)?.into_std();
    let mut entry = read_pinned(FileIo::snapshot(file)?, path, parent)?;
    if entry.metadata.kind != Kind::File
    {
        let folder = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        entry.directory =
            Some(std::sync::Arc::new(FileIo::snapshot(dir.open(folder)?.into_std())?));
    }
    Ok(entry)
}
pub(super) fn read_pinned(mut source: FileIo, path: &Path, parent: u32) -> Result<Entry>
{
    let opened = Opened::open(&mut source)?;
    let metadata = opened.metadata().clone();
    let layout = opened.layout();
    drop(opened);
    source.validate()?;
    let key = mac_hfs_b::names::comparison(&metadata.name)?;
    Ok(Entry { metadata,
               layout,
               parent,
               plain: false,
               path: path.to_path_buf(),
               key,
               pinned: std::sync::Arc::new(source),
               directory: None })
}

impl Volume
{
    pub fn initialize(path: &Path, name: &str) -> Result<Self>
    {
        if !path.exists()
        {
            std::fs::create_dir_all(path)?;
        }
        if std::fs::symlink_metadata(path)?.file_type().is_symlink()
        {
            return Err(error("export root cannot be a symlink"));
        }
        let dir = Dir::open_ambient_dir(path, cap_std::ambient_authority())?;
        if dir.entries()?.next().is_some()
        {
            return Err(error("init requires an empty directory; import files explicitly"));
        }
        sync(&dir)?;
        let mut identity = [0; 16];
        getrandom::fill(&mut identity).map_err(|e| error(format!("entropy unavailable: {e}")))?;
        let root = Metadata::new(Kind::Root, 2, identity, name, crate::now()?)?;
        let lock = dir.open_with(LOCK_RECORD,
                                 OpenOptions::new().read(true).write(true).create_new(true))?
                      .into_std();
        lock.lock()?;
        lock.sync_all()?;
        write_file(&dir, ROOT_RECORD, &root, &mut Slice(&[]), &mut Slice(&[]), false)?;
        drop(lock);
        Ok(Self { dir,
                  projection: std::sync::Mutex::new(super::projection::Projection::new()) })
    }

    pub fn open(path: &Path) -> Result<Self>
    {
        if std::fs::symlink_metadata(path)?.file_type().is_symlink()
        {
            return Err(error("export root cannot be a symlink"));
        }
        let dir = Dir::open_ambient_dir(path, cap_std::ambient_authority())?;
        let result = Self { dir,
                            projection:
                                std::sync::Mutex::new(super::projection::Projection::new()) };
        let _lock = result.lock(false)?;
        let root = result.root()?;
        if root.metadata.kind != Kind::Root
        {
            return Err(error("invalid export root record"));
        }
        Ok(result)
    }

    fn lock(&self, write: bool) -> Result<File>
    {
        let file =
            self.dir.open_with(LOCK_RECORD, OpenOptions::new().read(true).write(write))?.into_std();
        if write
        {
            file.lock()?;
        }
        else
        {
            file.lock_shared()?;
        }
        Ok(file)
    }

    pub fn root(&self) -> Result<Entry>
    {
        read_entry(&self.dir, Path::new(ROOT_RECORD), 2)
    }

    fn folder(&self, entry: &Entry) -> Result<Dir>
    {
        match entry.metadata.kind
        {
            Kind::Root => Ok(self.dir.try_clone()?),
            Kind::Directory =>
            {
                let path = if entry.plain
                {
                    entry.path.as_path()
                }
                else
                {
                    entry.path.parent().ok_or_else(|| error("invalid directory metadata path"))?
                };
                Ok(self.dir.open_dir(path)?)
            }
            Kind::File => Err(error("not a folder"))
        }
    }

    fn children_unlocked(&self, parent: &Entry) -> Result<Vec<Entry>>
    {
        let dir = self.folder(parent)?;
        let mut entries = Vec::new();

        for entry in dir.entries()?
        {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| error("non-UTF8 host component"))?;
            if parent.metadata.kind == Kind::Root && matches!(name, LOCK_RECORD | ROOT_RECORD)
               || parent.metadata.kind == Kind::Directory && name == DIRECTORY_RECORD
            {
                continue;
            }
            if name == format::annotation::DIRECTORY
            {
                if !entry.file_type()?.is_dir()
                {
                    return Err(error("metadata bookkeeping must be a directory"));
                }
                continue;
            }
            if name.starts_with(STAGE_PREFIX) || name.starts_with(".system-risc-delete-")
            {
                let recovered = risc_host::recover_removals_in(&dir).map_err(|e| {
                                    error(format!("cannot recover interrupted removal: host \
                                                   status {}",
                                                  e.0))
                                })?;
                if recovered
                {
                    // Recovery is per change, not a second scan on ordinary
                    // reads. Refresh the parent's physical snapshot before retry.
                    let refreshed = if parent.metadata.kind == Kind::Root
                    {
                        self.root()?
                    }
                    else
                    {
                        let physical = if parent.plain
                        {
                            parent.path.as_path()
                        }
                        else
                        {
                            parent.path
                                  .parent()
                                  .ok_or_else(|| error("invalid directory metadata path"))?
                        };
                        self.observe(physical,
                                     parent.parent,
                                     parent.metadata.volume,
                                     Kind::Directory,
                                     &parent.metadata.name)?
                    };
                    return self.children_unlocked(&refreshed);
                }
                return Err(error("unfinished host write transaction; inspect the private stage \
                                  before removing it"));
            }
            if entries.len() >= MAX_ENTRIES
            {
                return Err(error("catalog entry budget exceeded"));
            }
            let file_type = entry.file_type()?;
            if file_type.is_symlink() || (!file_type.is_file() && !file_type.is_dir())
            {
                return Err(error("host entry must be a regular file or directory"));
            }
            let kind = if file_type.is_dir() { Kind::Directory } else { Kind::File };
            let base = if parent.plain
            {
                parent.path.as_path()
            }
            else
            {
                parent.path.parent().unwrap_or(Path::new(""))
            };
            let path = base.join(name);
            let observed = self.observe(&path, parent.metadata.catalog_id,
                                        parent.metadata.volume, kind, name)?;
            entries.push(observed);
        }
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        if entries.windows(2).any(|pair| pair[0].key == pair[1].key)
        {
            return Err(error("canonical caseless filename collision"));
        }
        Ok(entries)
    }

    pub fn children(&self, path: &str) -> Result<Vec<Entry>>
    {
        let _lock = self.lock(false)?;
        let parent = self.resolve_unlocked(path)?;
        self.children_unlocked(&parent)
    }

    fn resolve_unlocked(&self, path: &str) -> Result<Entry>
    {
        let mut entry = self.root()?;
        for component in path.split('/').filter(|s| !s.is_empty())
        {
            if component == "." || component == ".."
            {
                return Err(error("dot path components are not allowed"));
            }
            let wanted = mac_hfs_b::names::comparison(&mac_hfs_b::names::normalize(component)?)?;
            entry = self.children_unlocked(&entry)?
                        .into_iter()
                        .find(|e| e.key == wanted)
                        .ok_or_else(|| error("guest path not found"))?;
        }
        Ok(entry)
    }

    pub fn resolve(&self, path: &str) -> Result<Entry>
    {
        let _lock = self.lock(false)?;
        self.resolve_unlocked(path)
    }

    fn reserve(&self, kind: Kind, name: &str) -> Result<(Metadata, Metadata)>
    {
        let mut root = self.root()?.metadata;
        let id = root.next_catalog_id;
        root.next_catalog_id = id.checked_add(1)
                                 .filter(|id| *id < 0x8000_0000)
                                 .ok_or_else(|| error("catalog identities exhausted"))?;
        root.revision =
            root.revision.checked_add(1).ok_or_else(|| error("root revision exhausted"))?;
        root.modified = crate::now()?;
        let metadata = Metadata::new(kind, id, root.volume, name, root.modified)?;
        Ok((root, metadata))
    }

    pub(crate) fn creation(&self) -> Result<Creation<'_>>
    {
        let lock = self.lock(true)?;
        sync(&self.dir)?;
        let root = self.root()?;
        Ok(Creation { volume: self,
                      _lock: lock,
                      root,
                      children: BTreeMap::new() })
    }

    pub fn create(&self, parent: &str, name: &str, kind: Kind, data: &mut impl ReadAt,
                  resources: &mut impl ReadAt, template: Option<&Metadata>)
                  -> Result<Entry>
    {
        self.creation()?.create(parent, name, kind, data, resources, template)
    }

    pub fn check(&self) -> Result<usize>
    {
        let _lock = self.lock(false)?;
        let root = self.root()?;
        let mut seen = BTreeSet::new();
        let mut identities = BTreeSet::new();
        let mut count = 0;
        self.visit(&root, 0, &mut |entry| {
                if !seen.insert(entry.metadata.catalog_id)
                   || !identities.insert(entry.metadata.identity)
                {
                    return Err(error("duplicate catalog/container identity; import copied \
                                      containers explicitly"));
                }
                if entry.metadata.catalog_id < 0x8000_0000
                   && entry.metadata.catalog_id >= root.metadata.next_catalog_id
                   && entry.metadata.kind != Kind::Root
                {
                    return Err(error("root identity counter is behind catalog"));
                }
                entry.validate()?;
                let source = FileIo::snapshot(entry.pinned.file.try_clone()?)?;
                if entry.plain
                {
                    source.validate()?;
                }
                else
                {
                    Opened::open(source)?.verify()?;
                }
                count += 1;
                if count > MAX_ENTRIES
                {
                    return Err(error("catalog entry budget exceeded"));
                }
                Ok(())
            })?;
        Ok(count)
    }

    fn visit(&self, entry: &Entry, depth: usize, run: &mut impl FnMut(&Entry) -> Result<()>)
             -> Result<()>
    {
        if depth > MAX_DEPTH
        {
            return Err(error("folder depth limit exceeded"));
        }
        run(entry)?;
        if entry.metadata.kind != Kind::File
        {
            for child in self.children_unlocked(entry)?
            {
                self.visit(&child, depth + 1, run)?;
            }
        }
        Ok(())
    }

    pub fn export(&self, path: &str, destination: &Path) -> Result<()>
    {
        let _lock = self.lock(false)?;
        let entry = self.resolve_unlocked(path)?;
        self.export_entry(&entry, destination, 0)
    }

    fn export_entry(&self, entry: &Entry, destination: &Path, depth: usize) -> Result<()>
    {
        if depth > MAX_DEPTH
        {
            return Err(error("folder depth limit exceeded"));
        }
        entry.validate()?;
        std::fs::create_dir(destination)?;
        if entry.metadata.kind == Kind::File
        {
            entry.pinned.validate()?;
            let mut source = FileIo::snapshot(entry.pinned.file.try_clone()?)?;
            let mut opened = if entry.plain
            {
                None
            }
            else
            {
                Some(Opened::open(FileIo::snapshot(entry.pinned.file.try_clone()?)?)?)
            };
            let mut buffer = [0; CHUNK_BYTES];
            for (kind, name) in [(Fork::Data, "data"), (Fork::Resource, "resources")]
            {
                let mut target = File::create(destination.join(name))?;
                let mut at = 0;
                loop
                {
                    let n = if let Some(opened) = opened.as_mut()
                    {
                        opened.read(kind, at, &mut buffer)?
                    }
                    else if kind == Fork::Resource
                    {
                        0
                    }
                    else
                    {
                        let n = source.len().saturating_sub(at).min(CHUNK_BYTES as u64) as usize;
                        source.validate()?;
                        source.read_exact(at, &mut buffer[..n])?;
                        n
                    };
                    if n == 0
                    {
                        break;
                    }
                    target.write_all(&buffer[..n])?;
                    at += n as u64;
                }
                target.sync_all()?;
            }
        }
        else
        {
            let children = destination.join("children");
            std::fs::create_dir(&children)?;
            for child in self.children_unlocked(entry)?
            {
                self.export_entry(&child, &children.join(&child.metadata.name), depth + 1)?;
            }
        }
        entry.validate()?;
        // Metadata is the completion record; a failed export lacks this marker.
        let mut file = File::create(destination.join("metadata.json"))?;
        file.write_all(&serde_json::to_vec_pretty(&crate::json(&entry.metadata, entry.layout))?)?;
        file.sync_all()?;
        Ok(())
    }
}

/// An import holds the cooperating writer lock throughout its operation. Read a
/// directory once, then update its lookup index per publication; repeated sibling
/// imports do not rescan or reparse the entire growing directory.
pub(crate) struct Creation<'a>
{
    volume:   &'a Volume,
    _lock:    File,
    root:     Entry,
    children: BTreeMap<u32, BTreeMap<String, Entry>>
}
impl Creation<'_>
{
    fn children(&mut self, parent: &Entry) -> Result<&mut BTreeMap<String, Entry>>
    {
        if let std::collections::btree_map::Entry::Vacant(slot) =
            self.children.entry(parent.metadata.catalog_id)
        {
            let entries = self.volume.children_unlocked(parent)?;
            slot.insert(entries.into_iter().map(|entry| (entry.key.clone(), entry)).collect());
        }
        Ok(self.children.get_mut(&parent.metadata.catalog_id).unwrap())
    }

    fn resolve(&mut self, path: &str) -> Result<Entry>
    {
        let mut entry = self.root.clone();
        for component in path.split('/').filter(|s| !s.is_empty())
        {
            if component == "." || component == ".."
            {
                return Err(error("dot path components are not allowed"));
            }
            let key = mac_hfs_b::names::comparison(&mac_hfs_b::names::normalize(component)?)?;
            entry = self.children(&entry)?
                        .get(&key)
                        .cloned()
                        .ok_or_else(|| error("guest path not found"))?;
        }
        Ok(entry)
    }

    pub(crate) fn create(&mut self, parent: &str, name: &str, kind: Kind, data: &mut impl ReadAt,
                         resources: &mut impl ReadAt, template: Option<&Metadata>)
                         -> Result<Entry>
    {
        if kind == Kind::Root
           || kind == Kind::Directory && (!data.is_empty() || !resources.is_empty())
        {
            return Err(error("invalid new entry kind or folder forks"));
        }
        let parent = self.resolve(parent)?;
        let prepared = mac_hfs_b::names::normalize(name)?;
        format::host_component(&prepared)?;
        let key = mac_hfs_b::names::comparison(&prepared)?;
        if self.children(&parent)?.len() >= MAX_ENTRIES
        {
            return Err(error("catalog entry budget exceeded"));
        }
        if self.children(&parent)?.contains_key(&key)
        {
            return Err(error("filename already exists"));
        }
        let (root, mut meta) = self.volume.reserve(kind, &prepared)?;
        if let Some(template) = template
        {
            meta.created = template.created;
            meta.modified = template.modified;
            meta.backup = template.backup;
            meta.finder_info = template.finder_info;
            meta.attributes = template.attributes;
            meta.comment = template.comment.clone();
        }
        let dir = self.volume.folder(&parent)?;
        let name = prepared;
        if dir.symlink_metadata(&name).is_ok()
        {
            return Err(error("host identity collision"));
        }
        format::Header::encode(&meta, data.len(), resources.len())?;
        write_file(&self.volume.dir, ROOT_RECORD, &root, &mut Slice(&[]), &mut Slice(&[]), true)?;
        if kind == Kind::Directory
        {
            if !data.is_empty() || !resources.is_empty()
            {
                return Err(error("folder cannot have forks"));
            }
            dir.create_dir(&name)?;
            let child = dir.open_dir(&name)?;
            if let Err(error) = write_file(&child, DIRECTORY_RECORD, &meta, data, resources, false)
            {
                let _ = dir.remove_dir_all(&name);
                return Err(error);
            }
            sync(&dir)?;
        }
        else if kind == Kind::File
        {
            write_file(&dir, &name, &meta, data, resources, false)?;
        }
        else
        {
            return Err(error("cannot create a second root"));
        }
        let base = if parent.plain
        {
            parent.path.as_path()
        }
        else
        {
            parent.path.parent().unwrap_or(Path::new(""))
        };
        let path = base.join(&name);
        self.volume.bind_created(&path, &meta)?;
        let entry =
            self.volume.observe(&path, parent.metadata.catalog_id, meta.volume, kind, &name)?;
        self.children(&parent)?.insert(key, entry.clone());
        Ok(entry)
    }
}
