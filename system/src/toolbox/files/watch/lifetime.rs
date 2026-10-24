//! Directory-watch payloads and backing use ordinary owner-walk dispatch.
#![forbid(unsafe_code)]
use mac_object_table::{domain::ReleasedResource,
                       registry::{Release, Source, Specification}};

use crate::toolbox::handles::{CapacityPolicy, Kind, KindCode, cleanup::Failure};

pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"FWST" } else { *b"FWCH" }),
                    label:    if storage { "Directory watch backing" } else { "Directory watches" },
                    provider: "System File Manager",
                    capacity: CapacityPolicy::HardBound { slots:  if storage
                                                          {
                                                              1
                                                          }
                                                          else
                                                          {
                                                              Kind::FileWatches.capacity()
                                                          },
                                                          reason: if storage
                                                          {
                                                              "One directory-watch backing per \
                                                               domain"
                                                          }
                                                          else
                                                          {
                                                              "Existing 32-bit directory-watch \
                                                               projection namespace"
                                                          } },
                    source:   Source::Resident }
}
pub(crate) trait WatchHost
{
    fn release_file_watch(&mut self, ticket: ReleasedResource, storage: bool)
                          -> Result<(), Failure>;
}
pub(crate) struct ReleaseWatches;
pub(crate) struct ReleaseStorage;
impl<C: WatchHost> Release<C, Failure> for ReleaseWatches
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_file_watch(ticket, false)
    }
}
impl<C: WatchHost> Release<C, Failure> for ReleaseStorage
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_file_watch(ticket, true)
    }
}
