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
    OpenTypes
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
            Self::All =>
            {
                crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, operation)
            }
            Self::OpenTypes => operation()
        }
    }

    pub fn includes(self) -> fn([u8; 4], i16) -> bool
    {
        match self
        {
            Self::All => |_, _| true,
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
pub(crate) fn read(owner: u64, volume: u32, cnid: u32) -> Result<Box<Loaded>, Status>
{
    read_selected(owner, volume, cnid, Selection::All)
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
