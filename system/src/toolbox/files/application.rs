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
pub(crate) fn read(owner: u64, volume: u32, cnid: u32) -> Result<Box<Loaded>, Status>
{
    // Resume the owned operation even if another request quarantined the mount.
    // Rechecking its availability first could strand the flight and its lease.
    if pending()
    {
        return provider::runtime::request_application(owner, volume, cnid);
    }
    let provider = crate::kernel::with_files(|m| m.read_provider(volume)).map_err(|e| e.status())??;
    if provider.is_some()
    {
        provider::runtime::request_application(owner, volume, cnid)
    }
    else
    {
        let (catalog, resources) = crate::kernel::with_files(|m| {
                                       m.application_resources(volume, cnid)
                                   }).map_err(|e| e.status())??;
        Box::try_new(Loaded { catalog,
                              resources }).map_err(|_| NO_MEMORY)
    }
}
