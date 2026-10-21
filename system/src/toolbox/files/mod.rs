//! Resident File Manager with catalog/Trash writes on live firmware block devices.
//! No file contents or catalog listings are fabricated or cached in applications.
#[cfg(any(target_arch = "riscv64", feature = "test-support"))]
mod resource;
use alloc::{string::String, vec::Vec};

mod scan;
mod cache;
mod provider;
use mac_abi::{files::*, *};
use mac_filesystem::{BlockDevice, CatalogEntry, Fork, MutationError, Partition, Volume,
                     WritableBlockDevice};
pub use mac_formats::macbinary::MacFile;
#[cfg(target_arch = "riscv64")]
pub(crate) use provider::runtime::{abandon_thread as abandon_provider_thread,
                                   pump as pump_provider_operations};

pub const MAX_DISKS: u32 = 32;
pub const MAX_VOLUMES: usize = crate::toolbox::handles::Kind::Volumes.capacity();
pub const MAX_OPEN_FILES: usize = crate::toolbox::handles::Kind::Files.capacity();
use crate::toolbox::handles::{Kind, Table};
mod write;
const MAX_PATH: usize = 4096;

/// Capabilities borrowed from firmware. Only construction crosses the unsafe ABI.
#[derive(Clone, Copy)]
pub struct BootStorage
{
    context:                 *mut (),
    disk_info:               DiskInfoFn,
    read_sector:             ReadSectorFn,
    write_sector:            Option<WriteSectorFn>,
    flush_disk:              Option<FlushDiskFn>,
    startup_disk:            u32,
    startup_partition_start: u64
}
impl BootStorage
{
    /// # Safety
    /// Firmware context and callbacks must remain live for the manager's lifetime.
    /// Calls must be serialized with every other user of the same storage context.
    pub unsafe fn from_boot_info(info: &BootInfo) -> Option<Self>
    {
        Some(Self { context:                 info.storage_context,
                    disk_info:               info.disk_info?,
                    read_sector:             info.read_sector?,
                    write_sector:            info.write_sector,
                    flush_disk:              info.flush_disk,
                    startup_disk:            info.startup_disk,
                    startup_partition_start: info.startup_partition_start })
    }

    fn info(&self, index: u32) -> Result<DiskInfo, Status>
    {
        let mut result = DiskInfo::default();
        let status = unsafe { (self.disk_info)(self.context, index, &mut result) };
        if status == OK { Ok(result) } else { Err(status) }
    }
}
struct Device
{
    storage: BootStorage,
    index:   u32,
    info:    DiskInfo,
    cache:   cache::Cache
}
impl BlockDevice for Device
{
    fn sector_count(&self) -> u64
    {
        self.info.sector_count
    }

    fn read_sector(&mut self, sector: u64, output: &mut [u8; 512]) -> mac_hfs::Result<()>
    {
        if sector >= self.info.sector_count
        {
            return Err(mac_hfs::Error("sector outside device"));
        }
        if self.cache.get(sector, output)
        {
            return Ok(());
        }
        let status = unsafe {
            (self.storage.read_sector)(self.storage.context,
                                       self.index,
                                       sector,
                                       output.as_mut_ptr())
        };
        if status == OK
        {
            self.cache.put(sector, output);
            Ok(())
        }
        else
        {
            self.cache.clear();
            Err(mac_hfs::Error("firmware sector read failed"))
        }
    }
}
impl WritableBlockDevice for Device
{
    fn is_read_only(&self) -> bool
    {
        self.info.read_only != 0
        || self.storage.write_sector.is_none()
        || self.storage.flush_disk.is_none()
    }

    fn write_sector(&mut self, sector: u64, input: &[u8; 512]) -> mac_hfs::Result<()>
    {
        if self.is_read_only()
        {
            return Err(mac_hfs::Error("device is read-only"));
        }
        if sector >= self.info.sector_count
        {
            return Err(mac_hfs::Error("sector outside device"));
        }
        let status = unsafe {
            (self.storage.write_sector.unwrap())(self.storage.context,
                                                 self.index,
                                                 sector,
                                                 input.as_ptr())
        };
        if status == OK
        {
            self.cache.put(sector, input);
            Ok(())
        }
        else
        {
            // A reported failure may have written part of this sector. Never
            // hide it behind cached bytes during recovery or subsequent reads.
            self.cache.clear();
            Err(mac_hfs::Error("firmware sector write failed"))
        }
    }

    fn flush(&mut self) -> mac_hfs::Result<()>
    {
        let callback =
            self.storage.flush_disk.ok_or(mac_hfs::Error("device has no flush callback"))?;
        let status = unsafe { callback(self.storage.context, self.index) };
        if status == OK
        {
            Ok(())
        }
        else
        {
            self.cache.clear();
            Err(mac_hfs::Error("firmware disk flush failed"))
        }
    }
}
struct BlockMounted
{
    device:           Device,
    partition:        Partition,
    info:             VolumeInfo,
    poisoned:         bool,
    scanner:          Option<mac_filesystem::scan::CatalogScanner>,
    catalog_revision: u64
}
// At most 128 slots, each already sized for a block mount before this enum.
// Retain inline storage rather than adding a heap allocation/indirection to
// every existing mounted disk solely to shrink provider slots by 224 bytes.
#[allow(clippy::large_enum_variant)]
enum Mounted
{
    Block(BlockMounted),
    #[cfg_attr(not(target_arch = "riscv64"), allow(dead_code))]
    Provider(provider::Mounted)
}
impl Mounted
{
    fn info(&self) -> Option<VolumeInfo>
    {
        match self
        {
            Self::Block(mounted) => Some(mounted.info),
            Self::Provider(mounted) => mounted.info
        }
    }
}
struct OpenFile
{
    owner:          u64,
    id:             u32,
    volume:         u32,
    fork:           FileFork,
    position:       u64,
    #[cfg_attr(not(any(target_arch = "riscv64", feature = "test-support")), allow(dead_code))]
    resource_index: Option<mac_formats::resource::index::Index>
}
enum FileFork
{
    Block(Fork),
    Provider(provider::Open)
}
impl FileFork
{
    fn length(&self) -> Result<u64, Status>
    {
        match self
        {
            Self::Block(fork) => Ok(fork.length),
            Self::Provider(fork) => fork.ready_length()
        }
    }

    fn orphaned(&self) -> bool
    {
        matches!(self, Self::Provider(fork) if fork.orphaned)
    }
}
pub struct Manager
{
    volumes:              Table<Mounted>,
    handles:              Table<OpenFile>,
    writes:               Table<write::Pending>,
    pub rejected_volumes: u32
}
impl Default for Manager
{
    fn default() -> Self
    {
        Self { volumes:          Table::new(Kind::Volumes),
               handles:          Table::new(Kind::Files),
               writes:           Table::new(Kind::FileWrites),
               rejected_volumes: 0 }
    }
}
fn io<T>(result: mac_hfs::Result<T>) -> Result<T, Status>
{
    result.map_err(|e| {
                       if e == mac_filesystem::INVALID_REQUEST
                       {
                           BAD_ARGUMENT
                       }
                       else
                       {
                           NOT_SUPPORTED
                       }
                   })
}
fn utf8_name(bytes: &[u8], output: &mut [u8]) -> u32
{
    let name = core::str::from_utf8(bytes).unwrap_or("");
    // Backend names are validated UTF-8; native names fit 255 bytes after NFC.
    let mut n = name.len().min(output.len());
    while !name.is_char_boundary(n)
    {
        n -= 1;
    }
    output[..n].copy_from_slice(&name.as_bytes()[..n]);
    n as u32
}
fn metadata(volume: u32, entry: &CatalogEntry) -> CatalogInfo
{
    let mut result =
        CatalogInfo { volume,
                      id: entry.id,
                      parent: entry.parent,
                      kind: if entry.directory { DIRECTORY } else { FILE },
                      data_length: entry.data.as_ref().map_or(0, |f| f.length),
                      resource_length: entry.resources.as_ref().map_or(0, |f| f.length),
                      flags: entry.flags,
                      child_count: entry.child_count,
                      created: entry.created,
                      modified: entry.modified,
                      backup: entry.backup.unwrap_or(0),
                      backup_present: u32::from(entry.backup.is_some()),
                      finder_info: entry.finder_info,
                      ..Default::default() };
    if !entry.directory
    {
        result.file_type.copy_from_slice(&entry.finder_info[..4]);
        result.creator.copy_from_slice(&entry.finder_info[4..8]);
    }
    result.name_length = utf8_name(&entry.name, &mut result.name);
    result
}
fn required(entry: Option<CatalogEntry>) -> Result<CatalogEntry, Status>
{
    entry.ok_or(NOT_FOUND)
}
impl Manager
{
    /// Discover all valid classic HFS partitions on each firmware device.
    /// Non-HFS devices and malformed volumes are skipped; capacity overflow fails.
    pub fn new(storage: BootStorage) -> Result<Self, Status>
    {
        let mut result = Self::default();
        for index in 0..=MAX_DISKS
        {
            let info = match storage.info(index)
            {
                Ok(info) => info,
                Err(NOT_FOUND) => return Ok(result),
                Err(status) => return Err(status)
            };
            if index == MAX_DISKS
            {
                return Err(NO_MEMORY);
            }
            let mut device = Device { storage,
                                      index,
                                      info,
                                      cache: Default::default() };
            let partitions = match mac_filesystem::partitions(&mut device)
            {
                Ok(p) => p,
                Err(_) =>
                {
                    result.rejected_volumes += 1;
                    continue;
                }
            };
            for partition in partitions
            {
                let mut volume = match Volume::mount(&mut device, partition)
                {
                    Ok(v) => v,
                    Err(_) =>
                    {
                        result.rejected_volumes += 1;
                        continue;
                    }
                };
                if result.volumes.len() == MAX_VOLUMES
                {
                    return Err(NO_MEMORY);
                }
                let mut summary =
                    VolumeInfo { id: 0,
                                 disk: index,
                                 partition_start: partition.start,
                                 total_bytes: volume.allocation_bytes,
                                 free_bytes: volume.free_bytes,
                                 capacity_known: 1,
                                 created: volume.created,
                                 modified: volume.modified,
                                 filesystem: match volume.format
                                 {
                                     mac_filesystem::Format::Hfs => FILESYSTEM_HFS,
                                     mac_filesystem::Format::HfsB => FILESYSTEM_HFS_B
                                 },
                                 filesystem_version: u32::from(volume.format.version()),
                                 blessed_folder: volume.blessed_id,
                                 attributes: volume.attributes,
                                 read_only: u16::from(info.read_only != 0
                                                      || storage.write_sector.is_none()
                                                      || storage.flush_disk.is_none()
                                                      || volume.attributes & 0x8080 != 0),
                                 startup: u32::from(index == storage.startup_disk
                                                    && partition.start
                                                       == storage.startup_partition_start),
                                 finder_info: volume.finder_info,
                                 icon_kind: info.icon_kind,
                                 icon_id: info.icon_id,
                                 ..Default::default() };
                summary.name_length = utf8_name(&volume.name, &mut summary.name);
                let mounted = BlockMounted { device: Device { storage,
                                                              index,
                                                              info,
                                                              cache: Default::default() },
                                             partition,
                                             info: summary,
                                             poisoned: false,
                                             scanner: volume.catalog_scanner().ok(),
                                             catalog_revision: scan::next_revision()? };
                result.volumes
                      .insert_with(|id| {
                          let mut mounted = mounted;
                          mounted.info.id = id;
                          Mounted::Block(mounted)
                      })
                      .map_err(|_| NO_MEMORY)?;
            }
        }
        Err(NO_MEMORY)
    }

    fn mounted(&mut self, id: u32) -> Result<&mut BlockMounted, Status>
    {
        match self.volumes.get_mut(id).ok_or(NOT_FOUND)?
        {
            Mounted::Block(mounted) => Ok(mounted),
            Mounted::Provider(_) => Err(NOT_SUPPORTED)
        }
    }

    fn with_volume<T>(&mut self, id: u32,
                      f: impl FnOnce(&mut Volume<'_, Device>) -> Result<T, Status>)
                      -> Result<T, Status>
    {
        let mounted = self.mounted(id)?;
        if mounted.poisoned
        {
            return Err(ROLLBACK_FAILED);
        }
        let mut volume = io(Volume::mount(&mut mounted.device, mounted.partition))?;
        f(&mut volume)
    }

    fn entry(&mut self, volume: u32, id: u32) -> Result<CatalogEntry, Status>
    {
        self.with_volume(volume, |v| required(io(v.entry(if id == 0 { ROOT_ID } else { id }))?))
    }

    fn lookup(&mut self, volume: u32, start: u32, path: &str) -> Result<CatalogEntry, Status>
    {
        if path.len() > MAX_PATH || path.contains('\0')
        {
            return Err(BAD_ARGUMENT);
        }
        self.with_volume(volume, |v| {
                let mut entry = required(io(v.entry(if path.starts_with('/') || start == 0
                                                    {
                                                        ROOT_ID
                                                    }
                                                    else
                                                    {
                                                        start
                                                    }))?)?;
                let components = path.split('/').filter(|s| !s.is_empty());
                for component in components
                {
                    if !entry.directory
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    if component == "."
                    {
                        continue;
                    }
                    if component == ".."
                    {
                        if entry.id != ROOT_ID
                        {
                            entry = required(io(v.entry(entry.parent))?)?;
                        }
                        continue;
                    }
                    let name = component.as_bytes();
                    entry = required(io(v.lookup(entry.id, name))?)?;
                }
                if path.ends_with('/') && !entry.directory
                {
                    return Err(BAD_ARGUMENT);
                }
                Ok(entry)
            })
    }

    fn path(&mut self, volume: u32, id: u32) -> Result<String, Status>
    {
        self.with_volume(volume, |v| {
                let mut components = Vec::new();
                let mut current = id;
                let mut seen = Vec::new();
                let mut length = 1;
                for _ in 0..256
                {
                    if seen.contains(&current)
                    {
                        return Err(NOT_SUPPORTED);
                    }
                    seen.push(current);
                    let entry = required(io(v.entry(current))?)?;
                    if entry.id == ROOT_ID
                    {
                        components.reverse();
                        return Ok(alloc::format!("/{}", components.join("/")));
                    }
                    let name = String::from_utf8(entry.name).map_err(|_| IO_ERROR)?;
                    // Native path convention has slash separators and cannot encode slash names.
                    if name.contains('/')
                    {
                        return Err(NOT_SUPPORTED);
                    }
                    length += name.len() + 1;
                    if length > MAX_PATH
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    components.push(name);
                    current = entry.parent;
                }
                Err(NOT_SUPPORTED)
            })
    }

    fn slot(&mut self, owner: u64, handle: u32) -> Result<&mut OpenFile, Status>
    {
        let file = self.handles.get_mut(handle).ok_or(NOT_FOUND)?;
        if file.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        Ok(file)
    }

    fn open(&mut self, owner: u64, volume: u32, id: u32, kind: u32) -> Result<(u32, u64), Status>
    {
        let mut entry = self.entry(volume, id)?;
        if entry.directory
        {
            return Err(BAD_ARGUMENT);
        }
        let fork = match kind
                   {
                       DATA_FORK => entry.data.take(),
                       RESOURCE_FORK => entry.resources.take(),
                       _ => return Err(BAD_ARGUMENT)
                   }.ok_or(NOT_SUPPORTED)?;
        let length = fork.length;
        let handle = self.handles
                         .insert(OpenFile { owner,
                                            id,
                                            volume,
                                            fork: FileFork::Block(fork),
                                            position: 0,
                                            resource_index: None })
                         .map_err(|_| NO_MEMORY)?;
        Ok((handle, length))
    }

    fn mutation(&mut self, call: &mut FileCall, name: &str, output: &mut [u8])
                -> Result<(), Status>
    {
        let when = call.position;
        if matches!(call.operation, TRASH_DIRECTORY | SET_LOCKED | SET_STATIONERY) && call.index > 1
        {
            return Err(BAD_ARGUMENT);
        }
        let name = name.as_bytes();
        let mut protected = Vec::new();
        protected.try_reserve_exact(MAX_OPEN_FILES).map_err(|_| NO_MEMORY)?;
        for (_, file) in self.handles.iter()
        {
            if file.volume == call.volume
            {
                protected.push(file.id);
            }
        }
        for (_, pending) in self.writes.iter()
        {
            if pending.volume == call.volume && pending.expected_cnid != 0
            {
                protected.push(pending.expected_cnid);
            }
        }
        let mounted = self.mounted(call.volume)?;
        if mounted.poisoned
        {
            return Err(ROLLBACK_FAILED);
        }
        let changes_catalog = scan::writes_catalog(call.operation, call.index);
        if changes_catalog
        {
            mounted.catalog_revision = scan::next_revision()?;
        }
        let mut volume = io(Volume::mount(&mut mounted.device, mounted.partition))?;
        let result = match call.operation
        {
            NEW_FOLDER =>
            {
                volume.create_folder(if call.cnid == 0 { ROOT_ID } else { call.cnid }, name, when)
                      .map(Some)
            }
            TRASH_DIRECTORY =>
            {
                volume.trash_directory(call.index != 0,
                                       when)
                      .map(Some)
            }
            COMMENT => volume.comment(call.cnid).and_then(|bytes| {
                let comment = core::str::from_utf8(&bytes).map_err(|_|MutationError::Io)?;
                call.length = comment.len() as u64;
                if output.len() < comment.len() { return Err(MutationError::NoMemory); }
                output[..comment.len()].copy_from_slice(comment.as_bytes());
                call.transferred = comment.len();
                Ok(None)
            }),
            SET_COMMENT => volume.set_comment(call.cnid, name, when, &protected).map(|()| None),
            RENAME => volume.rename(call.cnid, name, when, &protected).map(Some),
            SET_LOCKED => volume.set_locked(call.cnid, call.index != 0, when, &protected).map(Some),
            SET_STATIONERY =>
            {
                volume.set_stationery(call.cnid, call.index != 0, when, &protected).map(Some)
            }
            MOVE_TO_TRASH => volume.move_to_trash(call.cnid, when, &protected).map(Some),
            PUT_AWAY => volume.put_away(call.cnid, when, &protected).map(Some),
            TRASH_USAGE =>
            {
                volume.trash_usage().map(|(count, bytes)| {
                                        call.index = count;
                                        call.length = bytes;
                                        None
                                    })
            }
            EMPTY_TRASH =>
            {
                volume.empty_trash(when, &protected).map(|count| {
                                                        call.index = count;
                                                        None
                                                    })
            }
            _ => return Err(NOT_SUPPORTED)
        };
        if changes_catalog
        {
            mounted.scanner = volume.catalog_scanner().ok();
        }
        match result
        {
            Ok(entry) =>
            {
                mounted.info.free_bytes = volume.free_bytes;
                mounted.info.modified = volume.modified;
                if let Some(entry) = entry
                {
                    call.entry = metadata(call.volume, &entry);
                }
                Ok(())
            }
            Err(error) =>
            {
                if error == MutationError::RollbackFailed
                {
                    mounted.poisoned = true;
                    mounted.info.read_only = 1;
                }
                Err(match error
                {
                    MutationError::Invalid => BAD_ARGUMENT,
                    MutationError::ReadOnly => READ_ONLY,
                    MutationError::NotFound => NOT_FOUND,
                    MutationError::Exists => ALREADY_EXISTS,
                    MutationError::Protected => ACCESS_DENIED,
                    MutationError::Busy => SERVICE_BUSY,
                    MutationError::Full => DISK_FULL,
                    MutationError::NoMemory => NO_MEMORY,
                    MutationError::Unsupported => NOT_SUPPORTED,
                    MutationError::Io => IO_ERROR,
                    MutationError::RollbackFailed => ROLLBACK_FAILED
                })
            }
        }
    }

    /// Trusted kernel/bootstrap requests use a separate owner namespace. Native
    /// application dispatch must supply the scheduler's app identity explicitly.
    pub fn request(&mut self, call: &mut FileCall, name: &str, output: &mut [u8])
                   -> Result<(), Status>
    {
        self.request_owned(0, call, name, output)
    }

    /// Close every fork owned by an exiting application, preserving all other
    /// marks. Slot generations survive cleanup so old IDs cannot alias reuse.
    pub fn close_all(&mut self, owner: u64) -> usize
    {
        let mut count = 0;
        loop
        {
            let id =
                self.handles.iter().find_map(|(id, file)| {
                                       (file.owner == owner && !file.fork.orphaned()).then_some(id)
                                   });
            let Some(id) = id
            else
            {
                break;
            };
            if let Some(OpenFile { fork: FileFork::Provider(fork),
                                   .. }) = self.handles.get_mut(id)
            {
                fork.abandon();
            }
            else
            {
                self.handles.remove(id);
            }
            count += 1;
        }
        loop
        {
            let id =
                self.writes
                    .iter()
                    .find_map(|(id, file)| (file.owner == owner && !file.orphaned()).then_some(id));
            let Some(id) = id
            else
            {
                break;
            };
            if !self.abandon_write(id)
            {
                self.writes.remove(id);
            }
            count += 1;
        }
        count
    }

    /// `owner` is trusted execution context captured before any I/O handoff. It
    /// is deliberately absent from FileCall and cannot be selected by an app.
    pub fn request_owned(&mut self, owner: u64, call: &mut FileCall, name: &str, output: &mut [u8])
                         -> Result<(), Status>
    {
        call.transferred = 0;
        if matches!(call.operation,
                    NEW_FOLDER
                    | TRASH_DIRECTORY
                    | MOVE_TO_TRASH
                    | PUT_AWAY
                    | EMPTY_TRASH
                    | TRASH_USAGE
                    | RENAME
                    | SET_LOCKED
                    | SET_STATIONERY
                    | COMMENT
                    | SET_COMMENT)
        {
            if scan::writes_catalog(call.operation, call.index)
            {
                self.validate_provider_mutation(call.volume)?;
            }
            return self.mutation(call, name, output);
        }
        match call.operation
        {
            VOLUME_COUNT =>
            {
                call.index =
                    self.volumes.iter().filter_map(|(_, mounted)| mounted.info()).count() as u32
            }
            VOLUME_INDEX =>
            {
                call.volume_info = self.volumes
                                       .iter()
                                       .filter_map(|(_, mounted)| mounted.info())
                                       .nth(call.index as usize)
                                       .ok_or(NOT_FOUND)?
            }
            VOLUME_INFO =>
            {
                call.volume_info =
                    self.volumes.get(call.volume).and_then(Mounted::info).ok_or(NOT_FOUND)?
            }
            CATALOG => call.entry = metadata(call.volume, &self.entry(call.volume, call.cnid)?),
            ALLOCATION_INFO =>
            {
                let (data, resource) = self.with_volume(call.volume, |volume| {
                                           io(volume.allocation_info(call.cnid))?.ok_or(NOT_FOUND)
                                       })?;
                call.position = data;
                call.length = resource;
            }
            DIRECTORY_ENTRY =>
            {
                if !self.entry(call.volume, call.cnid)?.directory
                {
                    return Err(BAD_ARGUMENT);
                }
                let id = if call.cnid == 0 { ROOT_ID } else { call.cnid };
                let entry =
                    self.with_volume(call.volume, |v| required(io(v.child(id, call.index))?))?;
                call.entry = metadata(call.volume, &entry);
            }
            LOOKUP =>
            {
                call.entry = metadata(call.volume, &self.lookup(call.volume, call.cnid, name)?)
            }
            PARENT =>
            {
                let entry = self.entry(call.volume, call.cnid)?;
                call.entry = metadata(call.volume,
                                      &self.entry(call.volume,
                                                  if entry.id == ROOT_ID
                                                  {
                                                      ROOT_ID
                                                  }
                                                  else
                                                  {
                                                      entry.parent
                                                  })?);
            }
            PATH =>
            {
                let path =
                    self.path(call.volume, if call.cnid == 0 { ROOT_ID } else { call.cnid })?;
                call.length = path.len() as u64;
                if output.len() < path.len()
                {
                    return Err(NO_MEMORY);
                }
                output[..path.len()].copy_from_slice(path.as_bytes());
                call.transferred = path.len();
            }
            OPEN =>
            {
                let (handle, length) = self.open(owner, call.volume, call.cnid, call.fork)?;
                call.handle = handle;
                call.length = length;
                call.position = 0;
            }
            READ =>
            {
                // Clone only the bounded extent descriptor; handles keep independent marks.
                let file = self.slot(owner, call.handle)?;
                let FileFork::Block(fork) = &file.fork
                else
                {
                    return Err(NOT_SUPPORTED);
                };
                let (volume, position, mut fork) = (file.volume, file.position, fork.clone());
                let length = self.with_volume(volume, |v| io(v.read(&mut fork, position, output)))?;
                let file = self.slot(owner, call.handle)?;
                let fork_length = fork.length;
                file.fork = FileFork::Block(fork);
                file.position += length as u64;
                call.transferred = length;
                call.position = file.position;
                call.length = fork_length;
            }
            SEEK =>
            {
                let file = self.slot(owner, call.handle)?;
                let length = file.fork.length()?;
                let base = match call.whence
                {
                    SEEK_START => 0,
                    SEEK_CURRENT => file.position,
                    SEEK_END => length,
                    _ => return Err(BAD_ARGUMENT)
                };
                let position = base.checked_add_signed(call.offset).ok_or(BAD_ARGUMENT)?;
                if position > length
                {
                    return Err(BAD_ARGUMENT);
                }
                file.position = position;
                call.position = position;
                call.length = length;
            }
            TELL =>
            {
                let file = self.slot(owner, call.handle)?;
                call.position = file.position;
                call.length = file.fork.length()?;
            }
            CLOSE =>
            {
                if matches!(self.slot(owner, call.handle)?.fork, FileFork::Provider(_))
                {
                    return Err(NOT_SUPPORTED);
                }
                self.handles.remove(call.handle).ok_or(NOT_FOUND)?;
            }
            _ => return Err(NOT_SUPPORTED)
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;

#[cfg(any(target_arch = "riscv64", feature = "test-support"))]
pub(crate) mod native;
