//! Icon-move payloads and backing use ordinary owner-walk dispatch.
#![forbid(unsafe_code)]
use mac_object_table::{domain::ReleasedResource,
                       registry::{Release, Source, Specification}};

use crate::toolbox::handles::{CapacityPolicy, KindCode, cleanup::Failure};

pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"FIGS" } else { *b"FIGR" }),
                    label:    if storage { "Icon group backing" } else { "Icon move groups" },
                    provider: "System File Manager",
                    capacity: if storage
                    {
                        CapacityPolicy::HardBound { slots:  1,
                                                    reason: "One native file-object backing per \
                                                             domain" }
                    }
                    else
                    {
                        CapacityPolicy::GrowableUntilMemory
                    },
                    source:   Source::Resident }
}
pub(crate) trait GroupHost
{
    fn release_file_group(&mut self, ticket: ReleasedResource, storage: bool)
                          -> Result<(), Failure>;
}
pub(crate) struct ReleaseGroups;
pub(crate) struct ReleaseStorage;
impl<C: GroupHost> Release<C, Failure> for ReleaseGroups
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_file_group(ticket, false)
    }
}
impl<C: GroupHost> Release<C, Failure> for ReleaseStorage
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_file_group(ticket, true)
    }
}
