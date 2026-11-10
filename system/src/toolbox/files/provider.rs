//! Resident mount identities for filesystem-class providers. No device/sector
//! fiction: provider metadata is validated before its mount becomes visible.
#![forbid(unsafe_code)]
#![cfg_attr(not(systemrisc), allow(dead_code))]
use mac_filesystem_interface as fs;

use super::*;
use crate::toolbox::handles::InvocationGate;
mod forks;
pub(in crate::toolbox::files) mod path;
mod scan;
pub(super) mod writes;
pub(super) mod streams;
pub(super) use forks::Open;
#[cfg(systemrisc)]
pub(super) mod runtime;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source
{
    Export(u64),
    File(u64)
}
pub(super) struct Mounted
{
    pub provider:                           u64,
    pub source:                             Source,
    pub info:                               Option<VolumeInfo>,
    pub phase:                              Phase,
    reads:                                  usize,
    /// Which of the System's own background readers have an operation
    /// outstanding on this volume. Each holds at most one of `reads`. The
    /// System's bookkeeping loses nothing if the machine restarts under it,
    /// so it must never be why a person cannot restart.
    system_work:                            [bool; 3],
    pub(in crate::toolbox::files) revision: u64,
    pub(in crate::toolbox::files) changes:  bool
}

impl Mounted
{
    pub(super) fn power_status(&self) -> Result<(), Status>
    {
        match self.phase
        {
            Phase::Quarantined => Err(ROLLBACK_FAILED),
            Phase::Live
                if self.reads <= self.system_work.iter().filter(|busy| **busy).count() =>
            {
                Ok(())
            }
            _ => Err(SERVICE_BUSY)
        }
    }

    pub(super) fn end_read(&mut self) -> Result<(), Status>
    {
        self.reads = self.reads.checked_sub(1).ok_or(BAD_ABI)?;
        Ok(())
    }
}

/// Decode before exposing any provider bytes to an application. In addition to
/// wire validation, bind the reply to the requested volume, node or parent.
pub(in crate::toolbox::files) fn catalog(volume: u32, bytes: &[u8], node: Option<u32>,
                                         parent: Option<u32>)
                                         -> Result<CatalogInfo, Status>
{
    let metadata = fs::Metadata::decode(bytes).map_err(|_| BAD_ABI)?;
    catalog_metadata(volume, metadata, node, parent)
}
fn catalog_metadata(volume: u32, metadata: fs::Metadata<'_>, node: Option<u32>,
                    parent: Option<u32>)
                    -> Result<CatalogInfo, Status>
{
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
        let (kind, creator) = super::types::classify(metadata.name, info.file_type, info.creator);
        info.file_type = kind;
        info.creator = creator;
        info.finder_info[..4].copy_from_slice(&kind);
        info.finder_info[4..8].copy_from_slice(&creator);
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

/// The System's own background readers of provider volumes.
#[derive(Clone, Copy)]
pub(super) enum SystemReader
{
    /// The application index walking the catalogue.
    Walk         = 0,
    /// The application index reading what an application declares.
    Declarations = 1,
    /// File icons being read.
    Icons        = 2
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase
{
    Mounting,
    Live,
    Unmounting,
    Quarantined
}
fn volume_info(id: u32, descriptor: fs::Mount<'_>) -> VolumeInfo
{
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
                                read_only: u16::from(descriptor.capabilities & fs::WRITE == 0),
                                finder_info: root.finder_info,
                                ..Default::default() };
    info.name_length = utf8_name(root.name.as_bytes(), &mut info.name);
    info
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
            Phase::Live if self.groups.iter().any(|(_, group)| group.volume == id) =>
            {
                Err(SERVICE_BUSY)
            }
            Phase::Live if mounted.info.is_some_and(|info| info.read_only != 0) => Err(READ_ONLY),
            Phase::Live => Ok(())
        }
    }

    pub(crate) fn reserve_provider_mount(&mut self, provider: u64, source: Source)
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
        let mounted = super::Mounted::Provider(Mounted { provider,
                                                         source,
                                                         info: None,
                                                         phase: Phase::Mounting,
                                                         reads: 0,
                                                         system_work: [false; 3],
                                                         revision: 0,
                                                         changes: false });
        InvocationGate::with_system_allocation(|| self.volumes.insert(mounted)).map_err(|_| {
                                                                                   NO_MEMORY
                                                                               })
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
        let revision = super::scan::next_revision()?;
        let info = volume_info(id, descriptor);
        mounted.info = Some(info);
        mounted.changes = descriptor.capabilities & fs::CHANGES != 0;
        mounted.phase = Phase::Live;
        mounted.revision = revision;
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

    pub(crate) fn existing_provider_mount(&self, provider: u64, source: Source)
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

    pub(crate) fn provider_source(&mut self, volume: u32) -> Result<(u64, Source), Status>
    {
        let mount = self.provider_mount(volume)?;
        Ok((mount.provider, mount.source))
    }

    pub(crate) fn begin_provider_unmount(&mut self, id: u32) -> Result<u64, Status>
    {
        self.check_provider_unmount(id)?;
        let mounted = self.provider_mount(id)?;
        mounted.phase = Phase::Unmounting;
        Ok(mounted.provider)
    }

    /// Read-only admission for drag feedback; actual unmount revalidates it.
    pub(crate) fn check_provider_unmount(&mut self, id: u32) -> Result<(), Status>
    {
        #[cfg(systemrisc)]
        if self.transfers.iter().any(|(_, entry)| entry.uses(id))
        {
            return Err(SERVICE_BUSY);
        }
        if self.pins.objects().any(|pin| pin.volume == id)
           || self.handles.iter().any(|(_, file)| file.volume == id)
           || self.writes.iter().any(|(_, write)| write.volume == id)
           || self.streams.iter().any(|(_, stream)| stream.volume == id)
           || self.groups.iter().any(|(_, group)| group.volume == id)
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
        Ok(())
    }

    /// Resolve only resident identities here; provider callbacks run after the
    /// File Manager guard and firmware stack handoff have both been released.
    pub(super) fn read_provider(&self, id: u32) -> Result<Option<u64>, Status>
    {
        match self.volumes.get(id).ok_or(NOT_FOUND)?
        {
            super::Mounted::Block(_) =>
            {
                #[cfg(systemrisc)]
                if self.local_operation.is_some()
                {
                    return Err(SERVICE_BUSY);
                }
                Ok(None)
            }
            #[cfg(systemrisc)]
            super::Mounted::Operating(_) => Err(SERVICE_BUSY),
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

    pub(super) fn begin_provider_read(&mut self, id: u32) -> Result<u64, Status>
    {
        let provider = self.read_provider(id)?.ok_or(BAD_ARGUMENT)?;
        let mounted = self.provider_mount(id)?;
        mounted.reads = mounted.reads.checked_add(1).ok_or(NO_MEMORY)?;
        Ok(provider)
    }

    /// One of the System's background readers says whether it has an
    /// operation outstanding on this volume. Each has at most one, and says so
    /// only while it does.
    pub(super) fn system_work(&mut self, id: u32, reader: SystemReader, outstanding: bool)
    {
        if let Ok(mounted) = self.provider_mount(id)
        {
            mounted.system_work[reader as usize] = outstanding;
        }
    }

    pub(super) fn end_provider_read(&mut self, id: u32) -> Result<(), Status>
    {
        self.provider_mount(id)?.end_read()
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

