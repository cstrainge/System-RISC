//! Data-write payloads and backing use ordinary owner-walk dispatch.
#![forbid(unsafe_code)]
use mac_object_table::{domain::ReleasedResource,
                       registry::{Release, Source, Specification}};

use crate::toolbox::handles::{CapacityPolicy, KindCode, cleanup::Failure};

pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"FDST" } else { *b"FDWR" }),
                    label:    if storage { "Data write backing" } else { "Data file writes" },
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
pub(crate) trait WriteHost
{
    fn release_file_write(&mut self, ticket: ReleasedResource, storage: bool)
                          -> Result<(), Failure>;
}
pub(crate) struct ReleaseWrites;
pub(crate) struct ReleaseStorage;
impl<C: WriteHost> Release<C, Failure> for ReleaseWrites
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_file_write(ticket, false)
    }
}
impl<C: WriteHost> Release<C, Failure> for ReleaseStorage
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_file_write(ticket, true)
    }
}
