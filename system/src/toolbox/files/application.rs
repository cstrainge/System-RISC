//! Application loading owns its bytes before the Process Manager acquires UI state.
#![forbid(unsafe_code)]
use alloc::boxed::Box;

use super::*;
pub(crate) struct Loaded
{
    pub catalog:   CatalogInfo,
    pub resources: Vec<mac_formats::resource::Resource>
}
pub(crate) fn pending() -> bool
{
    provider::runtime::application_pending()
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Selection
{
    All,
    OpenTypes,
    Console { session: u64, streams: [u64; 3] }
}
impl Selection
{
    /// Full application packages become shared immutable System image sources.
    /// Open-type queries stay with their requesting subject. This scope affects
    /// new source backing only, never an existing buffer's refund authority.
    pub(crate) fn with_source_allocation<R>(self, operation: impl FnOnce() -> R) -> R
    {
        match self
        {
            Self::All | Self::Console { .. } =>
            {
                crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, operation)
            }
            Self::OpenTypes => operation()
        }
    }

    pub(super) fn file_type(self) -> [u8; 4]
    {
        if matches!(self, Self::Console { .. }) { *b"CLAP" } else { *b"APPL" }
    }

    pub fn includes(self) -> fn([u8; 4], i16) -> bool
    {
        match self
        {
            Self::All | Self::Console { .. } => |_, _| true,
            Self::OpenTypes => |kind, _| kind == *b"open"
        }
    }

    pub fn budget(self) -> mac_formats::resource::stream::Budget
    {
        let mut budget = mac_formats::resource::stream::Budget::default();
        if self == Self::OpenTypes
        {
            budget.payload_bytes = 64 * 1024;
        }
        budget
    }
}
pub(crate) fn read(owner: u64, volume: u32, cnid: u32, session: u64, streams: [u64; 3])
                   -> Result<Box<Loaded>, Status>
{
    if streams.contains(&0) && streams != [0; 3]
    {
        return Err(BAD_ARGUMENT);
    }
    #[cfg(target_arch = "riscv64")]
    if !pending()
    {
        crate::toolbox::processes::execution::authorize(owner, volume, cnid, streams)?;
    }
    let selection = if streams == [0; 3] { Selection::All } else { Selection::Console { session, streams } };
    read_selected(owner, volume, cnid, selection)
}
pub(crate) fn read_selected(owner: u64, volume: u32, cnid: u32, selection: Selection)
                            -> Result<Box<Loaded>, Status>
{
    // Resume the owned operation even if another request quarantined the mount.
    // Rechecking its availability first could strand the flight and its lease.
    if pending()
    {
        return provider::runtime::request_application(owner, volume, cnid, selection);
    }
    let provider = crate::kernel::with_files(|m| m.read_provider(volume)).map_err(|e| e.status())??;
    if provider.is_some()
    {
        provider::runtime::request_application(owner, volume, cnid, selection)
    }
    else
    {
        let (catalog, resources) = crate::kernel::with_files(|m| {
                                       m.validate_transfer_read(owner, volume, cnid)?;
                                       selection.with_source_allocation(|| {
                                                    m.application_resources_selected(volume, cnid,
                                                                                     selection)
                                                })
                                   }).map_err(|e| e.status())??;
        selection.with_source_allocation(|| {
                     Box::try_new(Loaded { catalog,
                                           resources })
                 })
                 .map_err(|_| NO_MEMORY)
    }
}

pub(crate) fn installed_terminal(volume: u32) -> Result<CatalogInfo, Status>
{
    crate::kernel::with_files(|files| {
        let entry = files.lookup(volume, 0, "/Applications/Utility/Terminal")?;
        if entry.directory || entry.finder_info[..8] != *b"APPLTRML"
        {
            return Err(ACCESS_DENIED);
        }
        Ok(metadata(volume, &entry))
    }).map_err(|error| error.status())?
}

pub(crate) fn catalog(owner: u64, volume: u32, cnid: u32) -> Result<CatalogInfo, Status>
{
    let mut call = FileCall { operation: CATALOG,
                              volume,
                              cnid,
                              ..Default::default() };
    let provider = crate::kernel::with_files(|m| m.read_provider(volume)).map_err(|e| e.status())??;
    let status = if provider.is_some()
    {
        provider::runtime::request(owner, &mut call, "", &mut [])
    }
    else
    {
        crate::kernel::with_files(|m|m.request_owned(owner,&mut call,"",&mut [])).map_err(|e|e.status())?.err().unwrap_or(OK)
    };
    if status == OK { Ok(call.entry) } else { Err(status) }
}

/// A bounded, positional probe of the original executable's fork. The
/// caller owns the buffer and no open-file handle escapes the operation.
#[cfg(target_arch = "riscv64")]
pub(crate) fn probe_fork(owner: u64, volume: u32, cnid: u32, resource: bool, offset: u64,
                         output: &mut [u8])
                         -> Result<usize, Status>
{
    crate::kernel::with_files(|files| {
        files.validate_transfer_read(owner, volume, cnid)?;
        if files.read_provider(volume)?.is_some()
        {
            return Err(NOT_SUPPORTED);
        }
        files.with_volume(volume, |disk| {
                 let entry = required(io(disk.entry(cnid))?)?;
                 if entry.directory
                 {
                     return Err(BAD_ARGUMENT);
                 }
                 let mut fork =
                     if resource { entry.resources } else { entry.data }.ok_or(NOT_FOUND)?;
                 if offset >= fork.length
                 {
                     return Ok(0);
                 }
                 let count = (fork.length - offset).min(output.len() as u64) as usize;
                 io(disk.read(&mut fork, offset, &mut output[..count]))
             })
    }).map_err(|error| error.status())?
}
