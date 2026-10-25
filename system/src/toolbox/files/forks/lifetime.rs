//! Open-fork payloads and backing use ordinary owner-walk dispatch.
#![forbid(unsafe_code)]
use mac_object_table::{domain::ReleasedResource,
                       registry::{Release, Source, Specification}};

use crate::toolbox::handles::{CapacityPolicy, Kind, KindCode, cleanup::Failure};

pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"FFST" } else { *b"FFRK" }),
                    label:    if storage { "Open fork backing" } else { "Open file forks" },
                    provider: "System File Manager",
                    capacity: CapacityPolicy::HardBound { slots:  if storage
                                                          {
                                                              1
                                                          }
                                                          else
                                                          {
                                                              Kind::Files.capacity()
                                                          },
                                                          reason: if storage
                                                          {
                                                              "One open-fork backing per domain"
                                                          }
                                                          else
                                                          {
                                                              "Existing 32-bit open-fork \
                                                               projection namespace"
                                                          } },
                    source:   Source::Resident }
}
pub(crate) trait ForkHost
{
    fn release_file_fork(&mut self, ticket: ReleasedResource, storage: bool)
                         -> Result<(), Failure>;
}
pub(crate) struct ReleaseForks;
pub(crate) struct ReleaseStorage;
impl<C: ForkHost> Release<C, Failure> for ReleaseForks
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_file_fork(ticket, false)
    }
}
impl<C: ForkHost> Release<C, Failure> for ReleaseStorage
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_file_fork(ticket, true)
    }
}
