//! Filesystem-neutral volume dispatch; HFS and HFS-B keep their own algorithms.
//! Names here are UTF-8 and dates are precise 1904-epoch microseconds. Classic
//! format conversions happen at this backend boundary, never in applications.
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
use alloc::vec::Vec;

pub use mac_block::{BlockDevice, Error, Partition, Result, WritableBlockDevice};
pub use mac_hfs::{MutationError, MutationResult};
use mac_hfs_b::{Budget, ForkKind, time::Timestamp};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format
{
    Hfs,
    HfsB
}
impl Format
{
    pub fn prepare_name(self, name: &str) -> Result<(Vec<u8>, Vec<u8>)>
    {
        match self
        {
            Self::Hfs =>
            {
                let key = mac_formats::text::encode(name).map_err(|_| INVALID_REQUEST)?;
                if key.is_empty()
                   || key.len() > 31
                   || key.contains(&0)
                   || key.contains(&b':')
                   || name == "."
                   || name == ".."
                {
                    return Err(INVALID_REQUEST);
                }
                let mut display = Vec::new();
                display.try_reserve_exact(name.len())
                       .map_err(|_| Error::AllocationFailed("name allocation failed"))?;
                display.extend_from_slice(name.as_bytes());
                Ok((display, key))
            }
            Self::HfsB =>
            {
                let name = mac_hfs_b::names::normalize(name).map_err(native_error)?;
                let key = mac_hfs_b::names::comparison(&name).map_err(native_error)?;
                Ok((name.into_bytes(), key.into_bytes()))
            }
        }
    }

    pub fn keys_equal(self, a: &[u8], b: &[u8]) -> bool
    {
        match self
        {
            Self::Hfs => mac_hfs::names_equal(a, b),
            Self::HfsB => a == b
        }
    }

    pub const fn version(self) -> u16
    {
        match self
        {
            Self::Hfs => 0,
            Self::HfsB => mac_hfs_b::layout::VERSION
        }
    }
}
pub const INVALID_REQUEST: Error = Error::Malformed("invalid filesystem request or name");
fn native_error(e: mac_hfs_b::Error) -> Error
{
    match e
    {
        mac_hfs_b::Error::Invalid => INVALID_REQUEST,
        mac_hfs_b::Error::Incompatible =>
        {
            Error::Malformed("incompatible HFS-B format; recreate volume")
        }
        mac_hfs_b::Error::Dirty =>
        {
            Error::Malformed("unclean HFS-B volume; check/recreate before mounting")
        }
        _ => Error::Malformed("HFS-B filesystem operation failed")
    }
}
pub fn partitions(disk: &mut impl BlockDevice) -> Result<Vec<Partition>>
{
    let mut b = [0; 512];
    if disk.sector_count() < 3
    {
        return Err(Error::Malformed("disk is too small"));
    }
    disk.read_sector(2, &mut b)?;
    if &b[..2] == b"HB" || &b[..2] == b"BD"
    {
        return Ok(alloc::vec![Partition { start:   0,
                                          sectors: disk.sector_count() }]);
    }
    let mut out = Vec::new();
    for entry in mac_block::apple_partitions(disk)?
    {
        let kind = entry.kind.split(|&b| b == 0).next().unwrap_or_default();
        if kind == b"Apple_HFS" || kind == b"System_RISC_HFS_B"
        {
            out.try_reserve(1)
               .map_err(|_| Error::AllocationFailed("partition allocation failed"))?;
            out.push(entry.range);
        }
    }
    Ok(out)
}
#[derive(Clone, Debug)]
enum ForkStorage
{
    Hfs(mac_hfs::Fork),
    HfsB
    {
        id:   u32,
        kind: ForkKind
    }
}
#[derive(Clone, Debug)]
pub struct Fork
{
    pub length: u64,
    storage:    ForkStorage
}
#[derive(Clone, Debug)]
pub struct CatalogEntry
{
    pub id:                 u32,
    pub parent:             u32,
    pub name:               Vec<u8>,
    pub directory:          bool,
    pub flags:              u16,
    pub finder_info:        [u8; 32],
    pub created:            u64,
    pub modified:           u64,
    pub backup:             Option<u64>,
    pub child_count:        u32,
    pub data:               Option<Fork>,
    pub resources:          Option<Fork>,
    pub data_allocated:     u64,
    pub resource_allocated: u64
}
impl From<mac_hfs::CatalogEntry> for CatalogEntry
{
    fn from(e: mac_hfs::CatalogEntry) -> Self
    {
        Self { id:                 e.id,
               parent:             e.parent,
               name:               mac_formats::text::decode(&e.name).into_bytes(),
               directory:          e.directory,
               flags:              e.flags,
               finder_info:        e.finder_info,
               created:            u64::from(e.created) * 1_000_000,
               modified:           u64::from(e.modified) * 1_000_000,
               backup:             (e.backup != 0).then_some(u64::from(e.backup) * 1_000_000),
               child_count:        u32::from(e.child_count),
               data:               e.data.map(|f| {
                                             Fork { length:  f.length,
                                                    storage: ForkStorage::Hfs(f) }
                                         }),
               resources:          e.resources.map(|f| {
                                                  Fork { length:  f.length,
                                                         storage: ForkStorage::Hfs(f) }
                                              }),
               data_allocated:     e.data_allocated,
               resource_allocated: e.resource_allocated }
    }
}
impl From<mac_hfs_b::Entry> for CatalogEntry
{
    fn from(e: mac_hfs_b::Entry) -> Self
    {
        Self { id:                 e.id,
               parent:             e.parent,
               name:               e.name.into_bytes(),
               directory:          e.directory,
               flags:              e.flags,
               finder_info:        e.finder_info,
               created:            e.created.0,
               modified:           e.modified.0,
               backup:             e.backup.map(|t| t.0),
               child_count:        e.children,
               data:
                   (!e.directory).then_some(Fork { length:  e.data.length,
                                     storage: ForkStorage::HfsB { id:   e.id,
                                                                  kind: ForkKind::Data } }),
               resources:
                   (!e.directory).then_some(Fork { length:  e.resources.length,
                                     storage: ForkStorage::HfsB { id:   e.id,
                                                                  kind: ForkKind::Resource } }),
               data_allocated:     e.data.blocks * 4096,
               resource_allocated: e.resources.blocks * 4096 }
    }
}
enum Backend<'a, D: BlockDevice>
{
    Hfs(mac_hfs::Volume<'a, D>),
    HfsB(mac_hfs_b::Volume<'a, D>)
}
pub struct Volume<'a, D: BlockDevice>
{
    backend:              Backend<'a, D>,
    pub format:           Format,
    pub blessed_id:       u32,
    pub name:             Vec<u8>,
    pub created:          u64,
    pub modified:         u64,
    pub allocation_bytes: u64,
    pub free_bytes:       u64,
    pub attributes:       u16,
    pub finder_info:      [u8; 32]
}
impl<'a, D: BlockDevice> Volume<'a, D>
{
    pub fn mount(disk: &'a mut D, partition: Partition) -> Result<Self>
    {
        partition.validate(disk)?;
        let mut signature = [0; 512];
        disk.read_sector(partition.start + 2, &mut signature)?;
        if &signature[..2] == b"HB"
        {
            // The resident document writer publishes up to 4 MiB atomically.
            // Payload pages stream directly to fresh blocks; only bounded
            // metadata is staged. This is a per-operation limit, not a fork cap.
            let v = mac_hfs_b::Volume::mount(disk,
                                             partition,
                                             Budget { io_bytes: 4 * 1024 * 1024,
                                                      ..Budget::default() }).map_err(native_error)?;
            let h = v.header();
            Ok(Self { format:           Format::HfsB,
                      blessed_id:       h.blessed,
                      name:             h.name.as_bytes().to_vec(),
                      created:          h.created,
                      modified:         h.modified,
                      allocation_bytes: u64::from(h.blocks) * 4096,
                      free_bytes:       u64::from(h.free) * 4096,
                      attributes:       0,
                      finder_info:      h.finder_info,
                      backend:          Backend::HfsB(v) })
        }
        else
        {
            let v = mac_hfs::Volume::mount(disk, partition)?;
            Ok(Self { format:           Format::Hfs,
                      blessed_id:       v.blessed_id,
                      name:             mac_formats::text::decode(&v.name).into_bytes(),
                      created:          u64::from(v.created) * 1_000_000,
                      modified:         u64::from(v.modified) * 1_000_000,
                      allocation_bytes: v.allocation_bytes,
                      free_bytes:       v.free_bytes,
                      attributes:       v.attributes,
                      finder_info:      v.finder_info,
                      backend:          Backend::Hfs(v) })
        }
    }

    pub fn entry(&mut self, id: u32) -> Result<Option<CatalogEntry>>
    {
        match &mut self.backend
        {
            Backend::Hfs(v) => Ok(v.entry(id)?.map(Into::into)),
            Backend::HfsB(v) =>
            {
                match v.entry(id)
                {
                    Ok(e) => Ok(Some(e.into())),
                    Err(mac_hfs_b::Error::NotFound) => Ok(None),
                    Err(e) => Err(native_error(e))
                }
            }
        }
    }

    pub fn child(&mut self, parent: u32, index: u32) -> Result<Option<CatalogEntry>>
    {
        match &mut self.backend
        {
            Backend::Hfs(v) => Ok(v.child(parent, index)?.map(Into::into)),
            Backend::HfsB(v) => Ok(v.child(parent, index).map_err(native_error)?.map(Into::into))
        }
    }

    pub fn lookup(&mut self, parent: u32, name: &[u8]) -> Result<Option<CatalogEntry>>
    {
        let name = core::str::from_utf8(name).map_err(|_| Error::Malformed("invalid UTF-8 name"))?;
        match &mut self.backend
        {
            Backend::Hfs(v) =>
            {
                Ok(v.lookup(parent,
                            &mac_formats::text::encode(name).map_err(|_| INVALID_REQUEST)?)?
                    .map(Into::into))
            }
            Backend::HfsB(v) =>
            {
                match v.lookup(parent, name)
                {
                    Ok(e) => Ok(Some(e.into())),
                    Err(mac_hfs_b::Error::NotFound) => Ok(None),
                    Err(e) => Err(native_error(e))
                }
            }
        }
    }

    pub fn allocation_info(&mut self, id: u32) -> Result<Option<(u64, u64)>>
    {
        Ok(self.entry(id)?.map(|e| (e.data_allocated, e.resource_allocated)))
    }

    pub fn read(&mut self, fork: &mut Fork, offset: u64, output: &mut [u8]) -> Result<usize>
    {
        match (&mut self.backend, &mut fork.storage)
        {
            (Backend::Hfs(v), ForkStorage::Hfs(f)) => v.read(f, offset, output),
            (Backend::HfsB(v),
             ForkStorage::HfsB { id,
                                 kind }) =>
            {
                v.read_fork(*id, *kind, offset, output).map_err(native_error)
            }
            _ => Err(Error::Malformed("foreign filesystem fork descriptor"))
        }
    }

    pub fn blessed_folder(&mut self) -> Result<CatalogEntry>
    {
        let entry = self.entry(self.blessed_id)?
                        .ok_or(Error::Malformed("blessed System Folder CNID is missing"))?;
        if !entry.directory || entry.id == 2
        {
            return Err(Error::Malformed("invalid blessed System Folder"));
        }
        Ok(entry)
    }

    pub fn find_file(&mut self, parent: u32, name: &[u8]) -> Result<File>
    {
        let mut e = self.lookup(parent, name)?
                        .ok_or(Error::Malformed("startup file is missing from blessed folder"))?;
        if e.directory
        {
            return Err(Error::Malformed("startup file is a directory"));
        }
        Ok(File { id:        e.id,
                  file_type: e.finder_info[..4].try_into().unwrap(),
                  creator:   e.finder_info[4..8].try_into().unwrap(),
                  data:      e.data.take().ok_or(Error::Malformed("missing data fork"))?,
                  resources: e.resources.take().ok_or(Error::Malformed("missing resource fork"))? })
    }

    fn refresh(&mut self)
    {
        match &self.backend
        {
            Backend::Hfs(v) =>
            {
                self.modified = u64::from(v.modified) * 1_000_000;
                self.free_bytes = v.free_bytes;
            }
            Backend::HfsB(v) =>
            {
                self.modified = v.header().modified;
                self.free_bytes = u64::from(v.header().free) * 4096;
                self.blessed_id = v.header().blessed;
            }
        }
    }
}
pub struct File
{
    pub id:        u32,
    pub file_type: [u8; 4],
    pub creator:   [u8; 4],
    pub data:      Fork,
    pub resources: Fork
}
fn legacy_time(when: u64) -> MutationResult<u32>
{
    u32::try_from(when / 1_000_000).map_err(|_| MutationError::Invalid)
}
fn legacy_name(name: &[u8]) -> MutationResult<Vec<u8>>
{
    mac_formats::text::encode(core::str::from_utf8(name).map_err(|_|MutationError::Invalid)?).map_err(|_|MutationError::Invalid)
}
fn mutation_error(e: mac_hfs_b::Error) -> MutationError
{
    use mac_hfs_b::Error as E;
    match e
    {
        E::Invalid => MutationError::Invalid,
        E::NotFound => MutationError::NotFound,
        E::Exists => MutationError::Exists,
        E::Protected => MutationError::Protected,
        E::ReadOnly => MutationError::ReadOnly,
        E::Full => MutationError::Full,
        E::Budget | E::NoMemory => MutationError::NoMemory,
        E::RollbackFailed | E::Dirty => MutationError::RollbackFailed,
        E::Io | E::Corrupt | E::Stale => MutationError::Io,
        E::Incompatible => MutationError::Unsupported
    }
}
mod mutation;
mod trash;
pub mod scan;
pub struct DataSave<'a>
{
    pub parent:    u32,
    pub name:      &'a [u8],
    pub expected:  u32,
    pub file_type: [u8; 4],
    pub creator:   [u8; 4],
    pub when:      u64
}
