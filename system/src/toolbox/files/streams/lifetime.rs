//! Streamed-write payloads and backing use ordinary owner-walk dispatch.
#![forbid(unsafe_code)]
use mac_object_table::{domain::ReleasedResource,
                       registry::{Release, Source, Specification}};

use crate::toolbox::handles::{CapacityPolicy, KindCode, cleanup::Failure};

pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"FSST" } else { *b"FSWR" }),
                    label:    if storage
                    {
                        "Streamed write backing"
                    }
                    else
                    {
                        "Streamed fork writes"
                    },
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
pub(crate) trait StreamHost
{
    fn release_file_stream(&mut self, ticket: ReleasedResource, storage: bool)
                           -> Result<(), Failure>;
}
pub(crate) struct ReleaseStreams;
pub(crate) struct ReleaseStorage;
impl<C: StreamHost> Release<C, Failure> for ReleaseStreams
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_file_stream(ticket, false)
    }
}
impl<C: StreamHost> Release<C, Failure> for ReleaseStorage
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_file_stream(ticket, true)
    }
}
