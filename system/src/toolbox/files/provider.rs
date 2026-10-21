//! Resident mount identities for filesystem-class providers. No device/sector
//! fiction: provider metadata is validated before its mount becomes visible.
#![forbid(unsafe_code)]
#![cfg_attr(not(target_arch = "riscv64"), allow(dead_code))]
use mac_filesystem_interface as fs;

use super::*;
mod forks;
pub(super) use forks::Open;
#[cfg(target_arch = "riscv64")]
pub(super) mod runtime;

pub(super) struct Mounted
{
    pub provider: u32,
    pub source:   u64,
    pub info:     Option<VolumeInfo>,
    pub phase:    Phase,
    reads:        usize
}

/// Decode before exposing any provider bytes to an application. In addition to
/// wire validation, bind the reply to the requested volume, node or parent.
fn catalog(volume: u32, bytes: &[u8], node: Option<u32>, parent: Option<u32>)
           -> Result<CatalogInfo, Status>
{
    let metadata = fs::Metadata::decode(bytes).map_err(|_| BAD_ABI)?;
    if node.is_some_and(|id| metadata.id != id) || parent.is_some_and(|id| metadata.parent != id)
    {
        return Err(BAD_ABI);
    }
    let mut info = CatalogInfo { volume,
                                 id: metadata.id,
                                 parent: metadata.parent,
                                 kind: if metadata.directory { DIRECTORY } else { FILE },
                                 data_length: metadata.data_length,
                                 resource_length: metadata.resource_length,
                                 flags: metadata.attributes,
                                 child_count: metadata.child_count,
                                 created: metadata.created,
                                 modified: metadata.modified,
                                 backup: metadata.backup.unwrap_or(0),
                                 backup_present: u32::from(metadata.backup.is_some()),
                                 finder_info: metadata.finder_info,
                                 ..Default::default() };
    if !metadata.directory
    {
        info.file_type.copy_from_slice(&metadata.finder_info[..4]);
        info.creator.copy_from_slice(&metadata.finder_info[4..8]);
    }
    info.name_length = utf8_name(metadata.name.as_bytes(), &mut info.name);
    Ok(info)
}
fn scan_entry(volume: u32, parent: u32, bytes: &[u8]) -> Result<Option<CatalogInfo>, Status>
{
    let header = bytes.get(..4).ok_or(BAD_ABI)?;
    match u32::from_le_bytes(header.try_into().map_err(|_| BAD_ABI)?)
    {
        fs::SCAN_PENDING if bytes.len() == 4 => Ok(None),
        fs::SCAN_DONE if bytes.len() == 4 => Err(NOT_FOUND),
        fs::SCAN_ENTRY => catalog(volume, &bytes[4..], None, Some(parent)).map(Some),
        _ => Err(BAD_ABI)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase
{
    Mounting,
    Live,
    Unmounting,
    Quarantined
}
impl Manager
{
    pub(super) fn validate_provider_mutation(&self, id: u32) -> Result<(), Status>
    {
        let Some(super::Mounted::Provider(mounted)) = self.volumes.get(id)
        else
        {
            return Ok(());
        };
        match mounted.phase
        {
            Phase::Quarantined => Err(ROLLBACK_FAILED),
            Phase::Mounting | Phase::Unmounting => Err(SERVICE_BUSY),
            Phase::Live if mounted.info.is_some_and(|info| info.read_only != 0) => Err(READ_ONLY),
            Phase::Live => Ok(())
        }
    }

    pub(crate) fn reserve_provider_mount(&mut self, provider: u32, source: u64)
                                         -> Result<u32, Status>
    {
        if provider == 0
        {
            return Err(BAD_ARGUMENT);
        }
        if self.volumes.iter().any(|(_, mounted)| matches!(mounted, super::Mounted::Provider(m) if m.provider == provider && m.source == source))
        {
            return Err(SERVICE_BUSY);
        }
        self.volumes
            .insert(super::Mounted::Provider(Mounted { provider,
                                                       source,
                                                       info: None,
                                                       phase: Phase::Mounting,
                                                       reads: 0 }))
            .map_err(|_| NO_MEMORY)
    }

    pub(crate) fn publish_provider_mount(&mut self, id: u32, bytes: &[u8])
                                         -> Result<VolumeInfo, Status>
    {
        let descriptor = fs::Mount::decode(bytes).map_err(|_| BAD_ABI)?;
        let mounted = self.provider_mount(id)?;
        if mounted.info.is_some() || mounted.phase != Phase::Mounting
        {
            return Err(SERVICE_BUSY);
        }
        let root = descriptor.root;
        let (total_bytes, free_bytes) = descriptor.capacity.unwrap_or((0, 0));
        let mut info = VolumeInfo { id,
                                    disk: u32::MAX,
                                    capacity_known: u16::from(descriptor.capacity.is_some()),
                                    total_bytes,
                                    free_bytes,
                                    created: root.created,
                                    modified: root.modified,
                                    filesystem: u32::from_be_bytes(descriptor.filesystem),
                                    filesystem_version: descriptor.filesystem_version,
                                    read_only: u16::from(descriptor.capabilities & fs::WRITE
                                                         == 0),
                                    finder_info: root.finder_info,
                                    ..Default::default() };
        info.name_length = utf8_name(root.name.as_bytes(), &mut info.name);
        mounted.info = Some(info);
        mounted.phase = Phase::Live;
        Ok(info)
    }

    pub(super) fn provider_mount(&mut self, id: u32) -> Result<&mut Mounted, Status>
    {
        match self.volumes.get_mut(id).ok_or(NOT_FOUND)?
        {
            super::Mounted::Provider(mounted) => Ok(mounted),
            _ => Err(BAD_ARGUMENT)
        }
    }

    pub(crate) fn retire_provider_mount(&mut self, id: u32) -> Result<(), Status>
    {
        let mounted = self.provider_mount(id)?;
        if mounted.phase == Phase::Live || mounted.reads != 0
        {
            return Err(SERVICE_BUSY);
        }
        // The caller must have an Unmount acknowledgement (or a never-sent
        // reservation). Never release this generation while the provider owns it.
        self.volumes.remove(id).ok_or(NOT_FOUND)?;
        Ok(())
    }

    pub(crate) fn existing_provider_mount(&self, provider: u32, source: u64)
                                          -> Result<Option<VolumeInfo>, Status>
    {
        for (_, mounted) in self.volumes.iter()
        {
            if let super::Mounted::Provider(m) = mounted
               && m.provider == provider
               && m.source == source
            {
                return if m.phase == Phase::Live { Ok(m.info) } else { Err(SERVICE_BUSY) };
            }
        }
        Ok(None)
    }

    pub(crate) fn begin_provider_unmount(&mut self, id: u32) -> Result<u32, Status>
    {
        if self.handles.iter().any(|(_, file)| file.volume == id)
           || self.writes.iter().any(|(_, write)| write.volume == id)
        {
            return Err(SERVICE_BUSY);
        }
        let mounted = self.provider_mount(id)?;
        if mounted.reads != 0
        {
            return Err(SERVICE_BUSY);
        }
        if !matches!(mounted.phase, Phase::Live | Phase::Quarantined)
        {
            return Err(SERVICE_BUSY);
        }
        mounted.phase = Phase::Unmounting;
        Ok(mounted.provider)
    }

    /// Resolve only resident identities here; provider callbacks run after the
    /// File Manager guard and firmware stack handoff have both been released.
    pub(super) fn read_provider(&self, id: u32) -> Result<Option<u32>, Status>
    {
        match self.volumes.get(id).ok_or(NOT_FOUND)?
        {
            super::Mounted::Block(_) => Ok(None),
            super::Mounted::Provider(mounted) =>
            {
                match mounted.phase
                {
                    Phase::Live => Ok(Some(mounted.provider)),
                    Phase::Quarantined => Err(ROLLBACK_FAILED),
                    _ => Err(SERVICE_BUSY)
                }
            }
        }
    }

    pub(super) fn begin_provider_read(&mut self, id: u32) -> Result<u32, Status>
    {
        let provider = self.read_provider(id)?.ok_or(BAD_ARGUMENT)?;
        let mounted = self.provider_mount(id)?;
        mounted.reads = mounted.reads.checked_add(1).ok_or(NO_MEMORY)?;
        Ok(provider)
    }

    pub(super) fn end_provider_read(&mut self, id: u32) -> Result<(), Status>
    {
        let mounted = self.provider_mount(id)?;
        mounted.reads = mounted.reads.checked_sub(1).ok_or(BAD_ABI)?;
        Ok(())
    }

    pub(crate) fn quarantine_provider_mount(&mut self, id: u32) -> Result<(), Status>
    {
        let mounted = self.provider_mount(id)?;
        mounted.phase = Phase::Quarantined;
        if let Some(info) = &mut mounted.info
        {
            info.read_only = 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    fn descriptor() -> Vec<u8>
    {
        let root = fs::Metadata { id:              2,
                                  parent:          0,
                                  directory:       true,
                                  data_length:     0,
                                  resource_length: 0,
                                  created:         3_875_000_000_000_001,
                                  modified:        3_875_000_000_000_002,
                                  backup:          None,
                                  child_count:     0,
                                  attributes:      0,
                                  finder_info:     [0; 32],
                                  revision:        1,
                                  name:            "External",
                                  comment:         "" };
        let mount = fs::Mount { capabilities: fs::READ | fs::WRITE,
                                filesystem: *b"HOST",
                                filesystem_version: 1,
                                capacity: None,
                                root };
        let mut bytes = alloc::vec![0;1024];
        let len = mount.encode(&mut bytes).unwrap();
        bytes.truncate(len);
        bytes
    }
    fn count(manager: &mut Manager) -> u32
    {
        let mut call = FileCall { operation: VOLUME_COUNT,
                                  ..Default::default() };
        manager.request(&mut call, "", &mut []).unwrap();
        call.index
    }
    fn open_reply(data: u64, resource: u64) -> [u8; 16]
    {
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&data.to_le_bytes());
        bytes[8..].copy_from_slice(&resource.to_le_bytes());
        bytes
    }
    #[test]
    fn provider_fork_marks_validate_owner_busy_state_lengths_and_acknowledged_reuse()
    {
        let mut manager = Manager::default();
        let volume = manager.reserve_provider_mount(77, 0).unwrap();
        manager.publish_provider_mount(volume, &descriptor()).unwrap();
        let (_, handle) = manager.reserve_provider_file(7, volume, 16, DATA_FORK).unwrap();
        assert_eq!(manager.begin_provider_unmount(volume), Err(SERVICE_BUSY));
        assert_eq!(manager.publish_provider_file(handle, &[0; 15]), Err(BAD_ABI));
        assert_eq!(manager.publish_provider_file(handle, &open_reply(131089, 70003)), Ok(131089));
        assert_eq!(manager.file_provider(9, handle), Err(ACCESS_DENIED));
        assert_eq!(manager.begin_provider_file_read(9, handle, 4), Err(ACCESS_DENIED));
        let (_, args) = manager.begin_provider_file_read(7, handle, usize::MAX).unwrap();
        assert_eq!((args.volume, args.token, args.flags, args.offset, args.length),
                   (volume, handle, DATA_FORK, 0, fs::MAX_TRANSFER as u64));
        assert_eq!(manager.begin_provider_file_close(7, handle), Err(SERVICE_BUSY));
        assert_eq!(manager.retire_provider_file(handle), Err(SERVICE_BUSY));
        assert_eq!(manager.finish_provider_file_read(handle,
                                                     0,
                                                     args.length,
                                                     Some(fs::MAX_TRANSFER + 1)),
                   Err(BAD_ABI));
        let (_, args) = manager.begin_provider_file_read(7, handle, 4).unwrap();
        assert_eq!(args.offset, 0, "invalid provider length advanced the mark");
        assert_eq!(manager.finish_provider_file_read(handle, 0, 4, Some(3)), Ok((3, 131089)));
        let mut call = FileCall { operation: SEEK,
                                  handle,
                                  whence: SEEK_END,
                                  offset: -1,
                                  ..Default::default() };
        manager.request_owned(7, &mut call, "", &mut []).unwrap();
        assert_eq!(call.position, 131088);
        assert_eq!(manager.begin_provider_file_close(9, handle), Err(ACCESS_DENIED));
        manager.begin_provider_file_close(7, handle).unwrap();
        assert_eq!(manager.begin_provider_unmount(volume), Err(SERVICE_BUSY));
        manager.retire_provider_file(handle).unwrap(); // simulated actual Close ACK
        let (_, next) = manager.reserve_provider_file(7, volume, 16, RESOURCE_FORK).unwrap();
        assert_eq!(next & 0xffff, handle & 0xffff);
        assert_ne!(next, handle);
        assert_eq!(manager.file_provider(7, handle), Err(NOT_FOUND));
        assert_eq!(manager.publish_provider_file(next, &open_reply(131089, 70003)), Ok(70003));
        manager.begin_provider_file_close(7, next).unwrap();
        manager.quarantine_provider_file(next).unwrap();
        assert_eq!(manager.retire_provider_file(next), Err(SERVICE_BUSY));
        assert_eq!(manager.begin_provider_unmount(volume), Err(SERVICE_BUSY));
        manager.begin_provider_file_close(7, next).unwrap();
        manager.retire_provider_file(next).unwrap();
        manager.begin_provider_unmount(volume).unwrap();
    }
    #[test]
    fn app_exit_retains_provider_marks_until_pending_reads_and_cleanup_acknowledge()
    {
        let mut manager = Manager::default();
        let volume = manager.reserve_provider_mount(77, 0).unwrap();
        manager.publish_provider_mount(volume, &descriptor()).unwrap();
        let (_, handle) = manager.reserve_provider_file(7, volume, 16, DATA_FORK).unwrap();
        manager.publish_provider_file(handle, &open_reply(9, 0)).unwrap();
        let (_, other) = manager.reserve_provider_file(9, volume, 16, DATA_FORK).unwrap();
        manager.publish_provider_file(other, &open_reply(9, 0)).unwrap();
        manager.begin_provider_file_read(7, handle, 4).unwrap();
        assert_eq!(manager.close_all(7), 1);
        assert_eq!(manager.close_all(7), 0);
        assert_eq!(manager.next_orphaned_file().unwrap(), None);
        assert_eq!(manager.begin_provider_unmount(volume), Err(SERVICE_BUSY));
        // A cancelled waiter must not advance the shared mark with undelivered
        // bytes. The actual result still releases the read before Close runs.
        assert_eq!(manager.finish_provider_file_read(handle, 0, 4, None), Ok((0, 9)));
        assert_eq!(manager.next_orphaned_file().unwrap(), Some((handle, 77, volume)));
        assert_eq!(manager.next_orphaned_file().unwrap(), None);
        assert_eq!(manager.file_provider(9, other), Ok(Some(77)));
        manager.retire_provider_file(handle).unwrap();
        assert_eq!(manager.file_provider(7, handle), Err(NOT_FOUND));
        manager.begin_provider_file_close(9, other).unwrap();
        manager.retire_provider_file(other).unwrap();
        manager.begin_provider_unmount(volume).unwrap();
    }
    #[test]
    fn provider_reads_pin_mount_generation_until_actual_completion()
    {
        let mut manager = Manager::default();
        let id = manager.reserve_provider_mount(77, 0).unwrap();
        assert_eq!(manager.begin_provider_read(id), Err(SERVICE_BUSY));
        manager.publish_provider_mount(id, &descriptor()).unwrap();
        assert_eq!(manager.begin_provider_read(id), Ok(77));
        assert_eq!(manager.begin_provider_read(id), Ok(77));
        assert_eq!(manager.begin_provider_unmount(id), Err(SERVICE_BUSY));
        manager.end_provider_read(id).unwrap();
        assert_eq!(manager.begin_provider_unmount(id), Err(SERVICE_BUSY));
        manager.end_provider_read(id).unwrap();
        assert_eq!(manager.end_provider_read(id), Err(BAD_ABI));
        manager.begin_provider_read(id).unwrap();
        manager.quarantine_provider_mount(id).unwrap();
        assert_eq!(manager.retire_provider_mount(id), Err(SERVICE_BUSY));
        manager.end_provider_read(id).unwrap();
        manager.begin_provider_unmount(id).unwrap();
        assert_eq!(manager.begin_provider_read(id), Err(SERVICE_BUSY));
        manager.retire_provider_mount(id).unwrap();
        let next = manager.reserve_provider_mount(77, 0).unwrap();
        manager.publish_provider_mount(next, &descriptor()).unwrap();
        assert_eq!(next & 0xffff, id & 0xffff);
        assert_eq!(manager.begin_provider_read(id), Err(NOT_FOUND));
        assert_eq!(manager.end_provider_read(id), Err(NOT_FOUND));
        manager.begin_provider_unmount(next).unwrap();
    }
    #[test]
    fn provider_catalog_and_scan_replies_are_bound_to_requested_identity()
    {
        let bytes = descriptor();
        let mount = fs::Mount::decode(&bytes).unwrap();
        let mut record = mount.root;
        record.id = 16;
        record.parent = ROOT_ID;
        record.directory = false;
        record.data_length = 128;
        record.resource_length = 64;
        record.name = "Café";
        record.backup = Some(0);
        record.finder_info[..8].copy_from_slice(b"TEXTTEST");
        let mut bytes = alloc::vec![0; 4 + fs::METADATA_BYTES + 510];
        let count = record.encode(&mut bytes[4..]).unwrap();
        bytes.truncate(4 + count);
        bytes[..4].copy_from_slice(&fs::SCAN_ENTRY.to_le_bytes());
        let info = catalog(123, &bytes[4..], Some(16), None).unwrap();
        assert_eq!((info.volume, info.id, info.parent), (123, 16, ROOT_ID));
        assert_eq!((info.data_length, info.resource_length), (128, 64));
        assert_eq!((info.file_type, info.creator), (*b"TEXT", *b"TEST"));
        assert_eq!((info.backup, info.backup_present), (0, 1));
        assert_eq!(info.name(), "Café");
        assert_eq!(catalog(123, &bytes[4..], Some(17), None), Err(BAD_ABI));
        assert_eq!(scan_entry(123, ROOT_ID, &bytes), Ok(Some(info)));
        assert_eq!(scan_entry(123, 17, &bytes), Err(BAD_ABI));
        assert_eq!(scan_entry(123, ROOT_ID, &bytes[..bytes.len() - 1]), Err(BAD_ABI));
        for (header, result) in
            [(fs::SCAN_PENDING, Ok(None)), (fs::SCAN_DONE, Err(NOT_FOUND)), (3, Err(BAD_ABI))]
        {
            assert_eq!(scan_entry(123, ROOT_ID, &header.to_le_bytes()), result);
            assert_eq!(scan_entry(123, ROOT_ID, &[header as u8, 0, 0, 0, 0]), Err(BAD_ABI));
        }
    }
    #[test]
    fn mount_publication_is_validated_and_ack_retirement_preserves_generations()
    {
        let mut manager = Manager::default();
        let first = manager.reserve_provider_mount(77, 0).unwrap();
        assert_eq!(count(&mut manager), 0);
        assert_eq!(manager.reserve_provider_mount(77, 0), Err(SERVICE_BUSY));
        assert_eq!(manager.publish_provider_mount(first, b"bad"), Err(BAD_ABI));
        assert_eq!(count(&mut manager), 0);
        let bytes = descriptor();
        let info = manager.publish_provider_mount(first, &bytes).unwrap();
        assert_eq!(info.name(), "External");
        assert_eq!(info.filesystem, u32::from_be_bytes(*b"HOST"));
        assert_eq!((info.capacity_known, info.total_bytes, info.free_bytes), (0, 0, 0));
        assert_eq!(count(&mut manager), 1);
        assert_eq!(manager.retire_provider_mount(first), Err(SERVICE_BUSY));
        assert_eq!(manager.existing_provider_mount(77, 0).unwrap(), Some(info));
        assert_eq!(manager.begin_provider_unmount(first).unwrap(), 77);
        manager.quarantine_provider_mount(first).unwrap();
        assert_eq!(manager.existing_provider_mount(77, 0), Err(SERVICE_BUSY));
        assert_eq!(manager.begin_provider_unmount(first).unwrap(), 77);
        manager.retire_provider_mount(first).unwrap();
        let next = manager.reserve_provider_mount(77, 0).unwrap();
        assert_eq!(next & 0xffff, first & 0xffff);
        assert_ne!(next, first);
        assert_eq!(manager.publish_provider_mount(first, &bytes), Err(NOT_FOUND));
        let foreign = Manager::default().reserve_provider_mount(77, 0).unwrap();
        assert_eq!(manager.publish_provider_mount(foreign, &bytes), Err(NOT_FOUND));
        manager.publish_provider_mount(next, &bytes).unwrap();
        assert_eq!(count(&mut manager), 1);
    }
}
