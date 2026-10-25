//! Transfer payloads and backing use ordinary owner-walk dispatch.
#![forbid(unsafe_code)]
use mac_object_table::{domain::ReleasedResource,
                       registry::{Release, Source, Specification}};

use crate::toolbox::handles::{CapacityPolicy, Kind, KindCode, cleanup::Failure};

pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"FTXS" } else { *b"FTXN" }),
                    label:    if storage { "Transfer backing" } else { "File transfers" },
                    provider: "System File Manager",
                    capacity: CapacityPolicy::HardBound { slots:  if storage
                                                          {
                                                              1
                                                          }
                                                          else
                                                          {
                                                              Kind::FileTransfers.capacity()
                                                          },
                                                          reason: if storage
                                                          {
                                                              "One transfer backing per domain"
                                                          }
                                                          else
                                                          {
                                                              "Existing 32-bit transfer projection \
                                                               namespace"
                                                          } },
                    source:   Source::Resident }
}
pub(crate) trait TransferHost
{
    fn release_file_transfer(&mut self, ticket: ReleasedResource, storage: bool)
                             -> Result<(), Failure>;
}
pub(crate) struct ReleaseTransfers;
pub(crate) struct ReleaseStorage;
impl<C: TransferHost> Release<C, Failure> for ReleaseTransfers
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_file_transfer(ticket, false)
    }
}
impl<C: TransferHost> Release<C, Failure> for ReleaseStorage
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_file_transfer(ticket, true)
    }
}
