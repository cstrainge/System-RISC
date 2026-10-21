//! Resident mount identities for filesystem-class providers. No device/sector
//! fiction: provider metadata is validated before its mount becomes visible.
#![forbid(unsafe_code)]
#![cfg_attr(not(target_arch = "riscv64"), allow(dead_code))]
use mac_filesystem_interface as fs;

use super::*;
#[cfg(target_arch = "riscv64")]
pub(super) mod runtime;

pub(super) struct Mounted
{
    pub provider: u32,
    pub source:   u64,
    pub info:     Option<VolumeInfo>,
    pub phase:    Phase
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
                                                       phase: Phase::Mounting }))
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
        if self.provider_mount(id)?.phase == Phase::Live
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
        if !matches!(mounted.phase, Phase::Live | Phase::Quarantined)
        {
            return Err(SERVICE_BUSY);
        }
        mounted.phase = Phase::Unmounting;
        Ok(mounted.provider)
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
