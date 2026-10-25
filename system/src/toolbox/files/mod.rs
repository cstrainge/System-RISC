//! Resident File Manager with catalog/Trash writes on live firmware block devices.
//! No file contents or catalog listings are fabricated or cached in applications.
#[cfg(any(target_arch = "riscv64", feature = "test-support"))]
mod resource;
#[cfg(target_arch = "riscv64")]
pub(crate) mod application;
use alloc::{string::String, vec::Vec};

mod objects;
pub(crate) mod pins;
pub(crate) mod forks;
mod scan;
mod properties;
pub(crate) mod watch;
mod cache;
#[cfg(any(target_arch = "riscv64", feature = "test-support"))]
mod icon_cache;
pub(crate) mod streams;
pub(crate) mod groups;
#[cfg(any(target_arch = "riscv64", feature = "test-support"))]
mod stream_native;
mod provider;
#[cfg(feature = "test-support")]
pub(crate) use provider::Source as TestProviderSource;
#[cfg(target_arch = "riscv64")]
pub(crate) use provider::runtime::transfer::lifetime as transfer_lifetime;
pub(crate) mod types;
#[cfg(target_arch = "riscv64")]
mod backing;
use mac_abi::{files::*, *};
use mac_filesystem::{BlockDevice, CatalogEntry, Fork, MutationError, Partition, Volume,
                     WritableBlockDevice};
pub use mac_formats::macbinary::MacFile;
#[cfg(target_arch = "riscv64")]
pub(crate) use provider::runtime::{abandon_thread as abandon_provider_thread,
                                   mount_startup_exports, pump as pump_provider_operations,
                                   request_resource};

/// Owned transfer compatibility calls use the same trap replay boundary as
/// driver jobs, with IDs classified by the central namespace registry.
#[cfg(target_arch = "riscv64")]
pub(crate) fn transfer_io_pending(thread: u64, handle: u32) -> Result<bool, Status>
{
    provider::runtime::transfer::bridge::pending(thread, handle)
}
#[cfg(target_arch = "riscv64")]
pub(crate) fn abandon_transfer_waiter(thread: u64, handle: u32)
{
    provider::runtime::transfer::bridge::abandon_waiter(thread, handle)
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn current_io_pending() -> bool
{
    provider::runtime::current_io_pending()
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn abandon_document_io(owner: u64, thread: u64, volume: u32, cnid: u32,
                                  application: bool)
                                  -> bool
{
    provider::runtime::abandon_document_io(owner, thread, volume, cnid, application)
}

/// Resolve a resident coordinator's trusted subject before any firmware stack
/// handoff. Request payloads never supply this identity or authority.
#[cfg(target_arch = "riscv64")]
pub(super) fn creation_authority(owner: u64) -> Result<crate::toolbox::handles::OwnerRef, Status>
{
    if owner == 0
    {
        return crate::toolbox::handles::system_owner();
    }
    let current = crate::context::current_owner()?;
    if current.owner().incarnation() == owner
    {
        return Ok(current);
    }
    if crate::kernel::with_files(|files| {
           provider::runtime::transfer::owns_coordinator(files, owner)
       }).map_err(|error| error.status())?
    {
        return crate::toolbox::handles::system_owner();
    }
    crate::kernel::with_desktop(|desktop| desktop.processes.ledger(owner)).map_err(|error| {
                                                                              error.status()
                                                                          })?
}

pub(crate) struct Caller
{
    pub owner: OwnerRef,
    pub kinds: Option<(KindLease, KindLease)>
}

pub const MAX_DISKS: u32 = 32;
pub const MAX_VOLUMES: usize = crate::toolbox::handles::Kind::Volumes.capacity();
pub const MAX_OPEN_FILES: usize = crate::toolbox::handles::Kind::Files.capacity();
use crate::toolbox::handles::{Kind, KindLease, OwnerRef, Table};
pub(crate) mod write;
mod moves;
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
            return Err(mac_hfs::Error::Malformed("sector outside device"));
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
            Err(mac_hfs::Error::Malformed("firmware sector read failed"))
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
            return Err(mac_hfs::Error::Malformed("device is read-only"));
        }
        if sector >= self.info.sector_count
        {
            return Err(mac_hfs::Error::Malformed("sector outside device"));
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
            Err(mac_hfs::Error::Malformed("firmware sector write failed"))
        }
    }

    fn flush(&mut self) -> mac_hfs::Result<()>
    {
        let callback = self.storage
                           .flush_disk
                           .ok_or(mac_hfs::Error::Malformed("device has no flush callback"))?;
        let status = unsafe { callback(self.storage.context, self.index) };
        if status == OK
        {
            Ok(())
        }
        else
        {
            self.cache.clear();
            Err(mac_hfs::Error::Malformed("firmware disk flush failed"))
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
    position:       core::sync::atomic::AtomicU64,
    authority:      crate::toolbox::handles::OwnerRef,
    #[cfg_attr(not(any(target_arch = "riscv64", feature = "test-support")), allow(dead_code))]
    resource_index: Option<mac_formats::resource::index::Index>
}
impl OpenFile
{
    fn owner_stopping(&self) -> bool
    {
        !self.authority.accepting()
    }
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
        matches!(self, Self::Provider(fork) if fork.orphaned())
    }
}
pub struct Manager
{
    #[cfg(any(target_arch = "riscv64", feature = "test-support"))]
    file_icons:           icon_cache::Cache,
    executables:          pins::Pins,
    document_pins:        Vec<(u32, u32, u32)>,
    volumes:              Table<Mounted>,
    handles:              objects::Objects<OpenFile>,
    writes:               objects::Objects<write::Pending>,
    streams:              objects::Objects<streams::Stream>,
    groups:               objects::Objects<groups::Group>,
    #[cfg(target_arch = "riscv64")]
    transfers:            objects::Objects<provider::runtime::transfer::Entry>,
    watches:              watch::Watches,
    pub rejected_volumes: u32
}
impl Default for Manager
{
    fn default() -> Self
    {
        Self { document_pins:                                                             Vec::new(),
               executables:
                   pins::Pins::new(),
               volumes:
                   Table::new(Kind::Volumes),
               handles:
                   objects::Objects::new(),
               writes:
                   objects::Objects::new(),
               streams:
                   objects::Objects::new(),
               groups:
                   objects::Objects::new(),
               #[cfg(target_arch = "riscv64")]
               transfers:
                   objects::Objects::new(),
               watches:
                   watch::Watches::new(),
               rejected_volumes:                                                          0,
               #[cfg(any(target_arch = "riscv64", feature = "test-support"))]
               file_icons:
                   Default::default() }
    }
}
impl Manager
{
    pub(crate) fn prepare_forks(&mut self,
                                kinds: (crate::toolbox::handles::KindLease,
                                 crate::toolbox::handles::KindLease))
                                -> Result<(), Status>
    {
        self.handles.prepare(kinds)
    }

    pub(crate) fn prepare_writes(&mut self,
                                 kinds: (crate::toolbox::handles::KindLease,
                                  crate::toolbox::handles::KindLease))
                                 -> Result<(), Status>
    {
        self.writes.prepare(kinds)
    }

    pub(crate) fn prepare_streams(&mut self,
                                  kinds: (crate::toolbox::handles::KindLease,
                                   crate::toolbox::handles::KindLease))
                                  -> Result<(), Status>
    {
        self.streams.prepare(kinds)
    }

    pub(crate) fn prepare_groups(&mut self,
                                 kinds: (crate::toolbox::handles::KindLease,
                                  crate::toolbox::handles::KindLease))
                                 -> Result<(), Status>
    {
        self.groups.prepare(kinds)
    }

    /// Power triggers run in the same non-preemptible service invocation as
    /// this barrier. Refuse unfinished publication/rollback before flushing;
    /// provider commits already acknowledge their own durability barriers.
    pub(crate) fn prepare_power(&mut self) -> Result<(), Status>
    {
        if self.handles.cleanup_pending()
           || self.writes.iter().next().is_some()
           || self.streams.iter().next().is_some()
           || self.groups.iter().next().is_some()
        {
            return Err(SERVICE_BUSY);
        }
        #[cfg(target_arch = "riscv64")]
        if self.transfers.iter().next().is_some() || !provider::runtime::power_ready()
        {
            return Err(SERVICE_BUSY);
        }
        for volume in self.volumes.values_mut()
        {
            match volume
            {
                Mounted::Block(mounted) =>
                {
                    if mounted.poisoned
                    {
                        return Err(ROLLBACK_FAILED);
                    }
                    if mounted.info.read_only == 0
                    {
                        mounted.device.flush().map_err(|_| IO_ERROR)?;
                    }
                }
                Mounted::Provider(mounted) => mounted.power_status()?
            }
        }
        Ok(())
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
        if let Ok(name) = core::str::from_utf8(&entry.name)
        {
            let (kind, creator) = types::classify(name, result.file_type, result.creator);
            result.file_type = kind;
            result.creator = creator;
            result.finder_info[..4].copy_from_slice(&kind);
            result.finder_info[4..8].copy_from_slice(&creator);
        }
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
    /// Transfer leases serialize mutations, not reads, across every involved
    /// mount. This makes rollback dates and source snapshots attributable to
    /// the transaction instead of overwriting an unrelated app's later edit.
    fn validate_transfer_mutation(&self, owner: u64, volume: u32) -> Result<(), Status>
    {
        #[cfg(target_arch = "riscv64")]
        if self.transfers
               .iter()
               .any(|(handle, entry)| entry.uses(volume) && !entry.permits(handle, owner))
        {
            return Err(SERVICE_BUSY);
        }
        let _ = (owner, volume);
        Ok(())
    }

    pub(crate) fn pin_document(&mut self, owner: u64, batch: u32, volume: u32, id: u32)
                               -> Result<(), Status>
    {
        self.read_provider(volume)?;
        self.validate_transfer_read(owner, volume, id)?;
        if self.writes.iter().any(|(_, write)| write.volume == volume && write.expected_cnid == id)
           || self.streams
                  .iter()
                  .any(|(_, stream)| stream.volume == volume && stream.expected == id)
        {
            return Err(SERVICE_BUSY);
        }
        if !self.document_pins.contains(&(batch, volume, id))
        {
            self.document_pins.try_reserve(1).map_err(|_| NO_MEMORY)?;
            self.document_pins.push((batch, volume, id));
        }
        Ok(())
    }

    pub(crate) fn unpin_documents(&mut self, batch: u32)
    {
        self.document_pins.retain(|(id, _, _)| *id != batch);
    }

    fn file_pinned(&self, volume: u32, id: u32) -> bool
    {
        self.executables.objects().any(|pin| pin.volume == volume && pin.id == id)
        || self.document_pins.iter().any(|(_, mount, file)| *mount == volume && *file == id)
    }

    fn validate_transfer_read(&self, owner: u64, volume: u32, id: u32) -> Result<(), Status>
    {
        #[cfg(target_arch = "riscv64")]
        if self.transfers
               .iter()
               .any(|(handle, entry)| entry.protects_read(handle, owner, volume, id))
        {
            return Err(SERVICE_BUSY);
        }
        let _ = (owner, volume, id);
        Ok(())
    }

    /// The caller supplies its directory-minted ledger before storage entry.
    pub(crate) fn pin_executable(&mut self, authority: &crate::toolbox::handles::OwnerRef,
                                 volume: u32, id: u32)
                                 -> Result<(), Status>
    {
        let owner = authority.owner().incarnation();
        self.validate_transfer_read(owner, volume, id)?;
        if self.writes.iter().any(|(_, write)| write.volume == volume && write.expected_cnid == id)
           || self.streams
                  .iter()
                  .any(|(_, stream)| stream.volume == volume && stream.expected == id)
        {
            return Err(SERVICE_BUSY);
        }
        #[cfg(target_arch = "riscv64")]
        self.prepare_executable_pins(crate::toolbox::handles::system_owner()?,
                                     crate::toolbox::handles::file_pin_kinds()?)?;
        self.executables.insert(authority, volume, id)
    }

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
                let mut recovery_failed = false;
                if info.read_only == 0
                   && storage.write_sector.is_some()
                   && storage.flush_disk.is_some()
                   && volume.attributes & 0x8080 == 0
                {
                    // Exclusive startup: no application can own a stream yet.
                    // Every step reclaims at most 64 KiB and commits a valid
                    // reachable catalog, so another interruption is retryable.
                    let when = volume.modified;
                    loop
                    {
                        match volume.recover_stream_step(when)
                        {
                            Ok(true) => (),
                            Ok(false) => break,
                            Err(_) =>
                            {
                                recovery_failed = true;
                                break;
                            }
                        }
                    }
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
                                                      || recovery_failed
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
                                             poisoned: recovery_failed,
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

    fn validate_file_authority(&self, owner: u64, authority: &crate::toolbox::handles::OwnerRef)
                               -> Result<(), Status>
    {
        #[cfg(target_arch = "riscv64")]
        {
            if owner != 0
               && owner != authority.owner().incarnation()
               && !(authority.owner().kind() == mac_object_table::domain::DomainKind::System
                    && provider::runtime::transfer::owns_coordinator(self, owner))
            {
                return Err(ACCESS_DENIED);
            }
        }
        #[cfg(not(target_arch = "riscv64"))]
        let _ = (owner, authority);
        Ok(())
    }

    fn open_for(&mut self, owner: u64, authority: &crate::toolbox::handles::OwnerRef, volume: u32,
                id: u32, kind: u32)
                -> Result<(u32, u64), Status>
    {
        self.validate_file_authority(owner, authority)?;
        #[cfg(target_arch = "riscv64")]
        self.prepare_forks(crate::toolbox::handles::file_fork_kinds()?)?;
        self.validate_transfer_read(owner, volume, id)?;
        if self.streams.iter().any(|(_, mark)| mark.volume == volume && mark.expected == id)
        {
            return Err(SERVICE_BUSY);
        }
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
        let handle = self.handles.insert(authority,
                                          OpenFile { owner,
                                                     id,
                                                     volume,
                                                     fork: FileFork::Block(fork),
                                                     position:
                                                         core::sync::atomic::AtomicU64::new(0),
                                                     authority: authority.clone(),
                                                     resource_index: None })?;
        Ok((handle, length))
    }

    fn open(&mut self, owner: u64, volume: u32, id: u32, kind: u32) -> Result<(u32, u64), Status>
    {
        #[cfg(test)]
        let authority = self.handles.fixture_owner(owner);
        #[cfg(all(not(test), target_arch = "riscv64"))]
        let authority = if owner == 0
        {
            crate::toolbox::handles::system_owner()?
        }
        else
        {
            return Err(ACCESS_DENIED);
        };
        #[cfg(all(not(test), not(target_arch = "riscv64")))]
        {
            let _ = (owner, volume, id, kind);
            return Err(ACCESS_DENIED);
        }
        #[cfg(any(test, target_arch = "riscv64"))]
        self.open_for(owner, &authority, volume, id, kind)
    }

    fn mutation(&mut self, call: &mut FileCall, name: &str, output: &mut [u8])
                -> Result<(), Status>
    {
        let when = call.position;
        if call.operation == REMOVE && self.file_pinned(call.volume, call.cnid)
        {
            return Err(SERVICE_BUSY);
        }
        if matches!(call.operation, TRASH_DIRECTORY | SET_LOCKED | SET_STATIONERY) && call.index > 1
        {
            return Err(BAD_ARGUMENT);
        }
        if call.operation == SET_ICON_LOCATION
           && (call.fork > 1
               || call.fork == 0 && call.index != 0
               || call.fork == 1 && call.index == u32::MAX)
        {
            return Err(BAD_ARGUMENT);
        }
        if call.operation == MOVE_INTO && (call.cnid <= ROOT_ID || call.index < ROOT_ID)
        {
            return Err(BAD_ARGUMENT);
        }
        if call.operation == REMOVE && (call.cnid <= ROOT_ID || call.index != 0 || call.fork != 0)
        {
            return Err(BAD_ARGUMENT);
        }
        if call.operation == SET_FOLDER_STATE
        {
            let info = call.entry.finder_info;
            let bounds = Rect::from_be_bytes(info[..8].try_into().unwrap());
            if call.cnid < ROOT_ID
               || call.index != 0
               || call.fork != 0
               || bounds.right <= bounds.left
               || bounds.bottom <= bounds.top
               || info[8..14].iter().chain(info[20..].iter()).any(|b| *b != 0)
            {
                return Err(BAD_ARGUMENT);
            }
        }
        if call.operation == SET_PROPERTIES
        {
            properties::validate(call, name)?;
        }
        let name = name.as_bytes();
        let mut protected = Vec::new();
        if !matches!(call.operation, SET_ICON_LOCATION | SET_FOLDER_STATE)
        {
            protected.try_reserve_exact(MAX_OPEN_FILES
                                        + self.writes.len()
                                        + self.streams.len()
                                        + self.executables.len()
                                        + self.document_pins.len())
                     .map_err(|_| NO_MEMORY)?;
            if matches!(call.operation, EMPTY_TRASH | REMOVE)
            {
                protected.extend(self.document_pins
                                     .iter()
                                     .filter(|(_, volume, _)| *volume == call.volume)
                                     .map(|(_, _, id)| *id));
                protected.extend(self.executables
                                     .objects()
                                     .filter(|pin| pin.volume == call.volume)
                                     .map(|pin| pin.id));
            }
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
            for (_, pending) in self.streams.iter()
            {
                if pending.volume == call.volume && pending.expected != 0
                {
                    protected.push(pending.expected);
                }
            }
        }
        let mut changes_catalog = scan::writes_catalog(call.operation, call.index);
        let old_parent = if changes_catalog
        {
            match call.operation
            {
                NEW_FOLDER => Some(if call.cnid == 0 { ROOT_ID } else { call.cnid }),
                TRASH_DIRECTORY => Some(ROOT_ID),
                EMPTY_TRASH | SET_ICON_LOCATION | SET_FOLDER_STATE => None,
                _ => Some(self.entry(call.volume, call.cnid)?.parent)
            }
        }
        else
        {
            None
        };
        let mounted = self.mounted(call.volume)?;
        if mounted.poisoned
        {
            return Err(ROLLBACK_FAILED);
        }
        if matches!(call.operation,
                    SET_ICON_LOCATION | SET_FOLDER_STATE | MOVE_INTO | SET_PROPERTIES | REMOVE)
           && mounted.info.read_only != 0
        {
            return Err(READ_ONLY);
        }
        let previous_revision = mounted.catalog_revision;
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
            SET_PROPERTIES => {
                let value = &call.entry;
                volume.set_properties(call.cnid, mac_filesystem::Properties {
                    flags: value.flags, finder_info: value.finder_info,
                    created: mac_hfs_b::time::Timestamp(value.created),
                    modified: mac_hfs_b::time::Timestamp(value.modified),
                    backup: (value.backup_present != 0).then_some(mac_hfs_b::time::Timestamp(value.backup)),
                    comment: core::str::from_utf8(name).map_err(|_| BAD_ARGUMENT)?,
                }, when, &protected).map(|(entry, changed)| {
                    if !changed { changes_catalog = false; mounted.catalog_revision = previous_revision; }
                    Some(entry)
                })
            }
            REMOVE => volume.remove(call.cnid, when, &protected).map(|()| None),
            RENAME => volume.rename(call.cnid, name, when, &protected).map(Some),
            MOVE_INTO =>
            {
                volume.move_into(call.cnid, call.index, when, &protected).map(|(entry, changed)|
                {
                    if !changed
                    {
                        changes_catalog = false;
                        mounted.catalog_revision = previous_revision;
                    }
                    Some(entry)
                })
            }
            SET_ICON_LOCATION =>
            {
                let location = (call.fork == 1)
                    .then_some((call.index as u16 as i16, (call.index >> 16) as u16 as i16));
                volume.set_icon_location(call.cnid, location, when).map(|(entry, changed)|
                {
                    if !changed
                    {
                        changes_catalog = false;
                        mounted.catalog_revision = previous_revision;
                    }
                    Some(entry)
                })
            }
            SET_FOLDER_STATE => {
                volume.set_folder_state(call.cnid, call.entry.finder_info, when).map(|(entry, changed)| {
                    if !changed { changes_catalog = false; mounted.catalog_revision = previous_revision; }
                    Some(entry)
                })
            }
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
                if changes_catalog
                {
                    if let Some(parent) = old_parent
                    {
                        self.notify_directory(call.volume, parent)?;
                    }
                    if call.entry.id != 0 && Some(call.entry.parent) != old_parent
                    {
                        self.notify_directory(call.volume, call.entry.parent)?;
                    }
                    if call.operation == EMPTY_TRASH
                    {
                        self.notify_directory(call.volume, 0)?;
                    }
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
                    MutationError::NotEmpty => DIRECTORY_NOT_EMPTY,
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

    /// `owner` is trusted execution context captured before any I/O handoff. It
    /// is deliberately absent from FileCall and cannot be selected by an app.
    pub fn request_owned(&mut self, owner: u64, call: &mut FileCall, name: &str, output: &mut [u8])
                         -> Result<(), Status>
    {
        call.transferred = 0;
        if matches!(call.operation, WATCH_DIRECTORY | POLL_WATCH | DISPOSE_WATCH)
        {
            // Watch calls carry genuine owner authority through the checked
            // trap boundary. A scalar alone cannot publish a ledger object.
            return Err(ACCESS_DENIED);
        }
        if matches!(call.operation,
                    NEW_FOLDER
                    | TRASH_DIRECTORY
                    | MOVE_TO_TRASH
                    | PUT_AWAY
                    | EMPTY_TRASH
                    | TRASH_USAGE
                    | RENAME
                    | REMOVE
                    | SET_PROPERTIES
                    | SET_LOCKED
                    | SET_STATIONERY
                    | SET_ICON_LOCATION
                    | SET_FOLDER_STATE
                    | MOVE_INTO
                    | COMMENT
                    | SET_COMMENT)
        {
            if scan::writes_catalog(call.operation, call.index)
            {
                self.validate_transfer_mutation(owner, call.volume)?;
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
                let (volume, position, mut fork) =
                    (file.volume,
                     file.position.load(core::sync::atomic::Ordering::Relaxed),
                     fork.clone());
                let length = self.with_volume(volume, |v| io(v.read(&mut fork, position, output)))?;
                let file = self.slot(owner, call.handle)?;
                let fork_length = fork.length;
                file.fork = FileFork::Block(fork);
                file.position.fetch_add(length as u64, core::sync::atomic::Ordering::Relaxed);
                call.transferred = length;
                call.position = file.position.load(core::sync::atomic::Ordering::Relaxed);
                call.length = fork_length;
            }
            SEEK =>
            {
                let file = self.slot(owner, call.handle)?;
                let length = file.fork.length()?;
                let base = match call.whence
                {
                    SEEK_START => 0,
                    SEEK_CURRENT => file.position.load(core::sync::atomic::Ordering::Relaxed),
                    SEEK_END => length,
                    _ => return Err(BAD_ARGUMENT)
                };
                let position = base.checked_add_signed(call.offset).ok_or(BAD_ARGUMENT)?;
                if position > length
                {
                    return Err(BAD_ARGUMENT);
                }
                file.position.store(position, core::sync::atomic::Ordering::Relaxed);
                call.position = position;
                call.length = length;
            }
            TELL =>
            {
                let file = self.slot(owner, call.handle)?;
                call.position = file.position.load(core::sync::atomic::Ordering::Relaxed);
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
