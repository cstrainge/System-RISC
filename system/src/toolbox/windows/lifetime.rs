//! Window and backing resources participate in the same owner walk as controls.
#![forbid(unsafe_code)]
use mac_object_table::{domain::ReleasedResource,
                       registry::{Release, Source, Specification}};

use crate::toolbox::handles::{CapacityPolicy, KindCode, cleanup::Failure};
pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"WNST" } else { *b"WIND" }),
                    label:    if storage { "Window Manager state" } else { "Windows" },
                    provider: "System Window Manager",
                    capacity: if storage
                    {
                        CapacityPolicy::HardBound { slots:  1,
                                                    reason: "One Window Manager backing per domain" }
                    }
                    else
                    {
                        CapacityPolicy::GrowableUntilMemory
                    },
                    source:   Source::Resident }
}
pub(crate) trait WindowHost
{
    fn release_window(&mut self, ticket: ReleasedResource) -> Result<(), Failure>;
    fn release_window_storage(&mut self, ticket: ReleasedResource) -> Result<(), Failure>;
}
pub(crate) struct ReleaseWindows;
pub(crate) struct ReleaseStorage;
impl<C: WindowHost> Release<C, Failure> for ReleaseWindows
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_window(ticket)
    }
}
impl<C: WindowHost> Release<C, Failure> for ReleaseStorage
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_window_storage(ticket)
    }
}
