//! Runtime entrypoints are offsets in the same genuine admitted application.
#![forbid(unsafe_code)]

use core::sync::atomic::Ordering;

use mac_abi::{ACCESS_DENIED, BAD_ABI, SERVICE_BUSY, Status};
use mac_formats::native::runtime;

impl super::Loaded
{
    pub(crate) fn runtime_entries(&self) -> Result<runtime::Entries, Status>
    {
        self._execution._source.runtime_entries()
    }
}

impl super::Read
{
    pub(crate) fn runtime_entries(&self) -> Result<runtime::Entries, Status>
    {
        if self.instance.module
           || !matches!(&self.source.role, super::Role::Application { .. })
           || self.owner().owner().kind() != mac_object_table::domain::DomainKind::Process
        {
            return Err(ACCESS_DENIED);
        }
        if !self.instance.initialized.load(Ordering::Acquire)
        {
            return Err(SERVICE_BUSY);
        }
        let mut resources =
            self.source
                .resources
                .iter()
                .filter(|resource| {
                    resource.kind == runtime::RESOURCE_KIND && resource.id == runtime::RESOURCE_ID
                });
        let Some(resource) = resources.next()
        else
        {
            serial!("USER_RUNTIME_MISSING app={}; rebuild the application with the current SDK",
                    self.binding.owner());
            return Err(BAD_ABI);
        };
        if resources.next().is_some()
        {
            serial!("USER_RUNTIME_REFUSED app={}: duplicate URTM resource", self.binding.owner());
            return Err(BAD_ABI);
        }
        let bytes = &resource.data;
        let descriptor = runtime::Descriptor::decode(bytes).map_err(|cause| {
                                                               serial!("USER_RUNTIME_REFUSED \
                                                                        app={}: {cause}",
                                                                       self.binding.owner());
                                                               BAD_ABI
                                                           })?;
        descriptor.resolve(self.instance.base, self.instance.limit, |entry| {
                      self.instance.executable.iter().any(|range| range.contains(&entry))
                  })
                  .map_err(|cause| {
                      serial!("USER_RUNTIME_ENTRY_REFUSED app={}: {cause}", self.binding.owner());
                      BAD_ABI
                  })
    }
}
