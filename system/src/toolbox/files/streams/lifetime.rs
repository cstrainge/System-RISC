//! Streamed-write payloads and backing use ordinary owner-walk dispatch.
#![forbid(unsafe_code)]
use mac_object_table::{domain::ReleasedResource,
                       registry::{Release, Source, Specification}};

use crate::toolbox::handles::{CapacityPolicy, Kind, KindCode, cleanup::Failure};

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
                    capacity: CapacityPolicy::HardBound { slots:  if storage
                                                          {
                                                              1
                                                          }
                                                          else
                                                          {
                                                              Kind::FileStreams.capacity()
                                                          },
                                                          reason: if storage
                                                          {
                                                              "One streamed-write backing per \
                                                               domain"
                                                          }
                                                          else
                                                          {
                                                              "Existing 32-bit streamed-write \
                                                               projection namespace"
                                                          } },
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
