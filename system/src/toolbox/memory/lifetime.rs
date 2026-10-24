//! Memory objects and their saved zone are independently registered lifetimes.
#![forbid(unsafe_code)]
use mac_object_table::{domain::ReleasedResource,
                       registry::{Release, Source, Specification}};

use crate::toolbox::handles::{CapacityPolicy, KindCode, cleanup::Failure};

pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"MMST" } else { *b"MMBL" }),
                    label:    if storage
                    {
                        "Memory Manager zone"
                    }
                    else
                    {
                        "Memory Manager handles"
                    },
                    provider: "System Memory Manager",
                    capacity: if storage
                    {
                        CapacityPolicy::HardBound { slots:  1,
                                                    reason: "One Memory Manager zone per domain" }
                    }
                    else
                    {
                        CapacityPolicy::HardBound { slots:  512,
                                                    reason: "Classic Memory Manager projection \
                                                             has 512 one-based slots" }
                    },
                    source:   Source::Resident }
}
pub(crate) trait MemoryHost
{
    fn release_memory(&mut self, ticket: ReleasedResource, storage: bool) -> Result<(), Failure>;
}
pub(crate) struct ReleaseBlocks;
pub(crate) struct ReleaseStorage;
impl<C: MemoryHost> Release<C, Failure> for ReleaseBlocks
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_memory(ticket, false)
    }
}
impl<C: MemoryHost> Release<C, Failure> for ReleaseStorage
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_memory(ticket, true)
    }
}
