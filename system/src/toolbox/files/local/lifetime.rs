//! Native file operation payloads and backing use ordinary owner-walk dispatch.
#![forbid(unsafe_code)]
use mac_object_table::{domain::ReleasedResource,
                       registry::{Release, Source, Specification}};

use crate::toolbox::handles::{CapacityPolicy, KindCode, cleanup::Failure};

pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"FIOB" } else { *b"FIOP" }),
                    label:    if storage
                    {
                        "Native file operation backing"
                    }
                    else
                    {
                        "Native file operations"
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
pub(crate) trait OperationHost
{
    fn release_file_operation(&mut self, ticket: ReleasedResource, storage: bool)
                              -> Result<(), Failure>;
}
pub(crate) struct ReleaseOperations(pub bool);
impl<C: OperationHost> Release<C, Failure> for ReleaseOperations
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_file_operation(ticket, self.0)
    }
}
