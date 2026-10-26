//! Owned cancellation for direct roots abandoned by failed module entry.
//! Nested requests belong to their caller's protocol; queued roots belong to
//! their queue. Neither is stolen by this coordinator.
#![forbid(unsafe_code)]
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use mac_abi::Status;
use mac_object_table::store::CompletionKey;

use super::{Error, Request, Result, Subject};

pub(super) const IDLE: u8 = 0;
pub(super) const READY: u8 = 1;
pub(super) const RUNNING: u8 = 2;
pub(super) const UNCERTAIN: u8 = 3;
pub(super) const ACKNOWLEDGED: u8 = 4;
pub(super) struct Control
{
    pub(super) phase: AtomicU8,
    waiting_reported: AtomicBool
}
pub(super) struct Reservation
{
    pub(super) state:  Arc<Control>,
    pub(super) output: Option<Vec<u8>>
}
impl Reservation
{
    pub(super) fn new(length: usize) -> Result<Self>
    {
        let mut output = Vec::new();
        output.try_reserve_exact(length).map_err(|_| Error::Exhausted)?;
        Ok(Self { state: Arc::try_new(Control { phase: AtomicU8::new(IDLE),
                    waiting_reported: AtomicBool::new(false) }).map_err(|_| Error::Exhausted)?,
                  output: Some(output) })
    }
}
/// Original receipt, subject and reply reservation travel together. Dropping
/// this guard is not cancellation acknowledgement: its record remains pinned.
pub(crate) struct Plan
{
    pub(super) request: Request,
    pub(super) subject: Subject,
    pub(super) receipt: CompletionKey,
    pub(super) state:   Arc<Control>,
    pub(super) active:  bool,
    pub(crate) output:  Vec<u8>
}
impl Plan
{
    pub(crate) fn request(&self) -> Request
    {
        self.request
    }

    pub(crate) fn subject(&self) -> &Subject
    {
        &self.subject
    }

    pub(crate) fn ticket(&self) -> u64
    {
        self.receipt.id()
    }

    pub(crate) fn note_waiting(&self) -> bool
    {
        !self.state.waiting_reported.swap(true, Ordering::AcqRel)
    }
}
impl Drop for Plan
{
    fn drop(&mut self)
    {
        if self.active
        {
            let _ = self.state
                        .phase
                        .compare_exchange(RUNNING, UNCERTAIN, Ordering::AcqRel, Ordering::Acquire);
        }
    }
}
pub(crate) struct Refusal
{
    pub(crate) cause: Error,
    pub(crate) plan:  Plan
}
impl core::fmt::Debug for Refusal
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        f.debug_struct("CancellationRefusal")
         .field("cause", &self.cause)
         .field("ticket", &self.plan.ticket())
         .finish_non_exhaustive()
    }
}
impl core::fmt::Display for Refusal
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        write!(f,
               "cancellation of request {} refused: {}; original plan remains owned",
               self.plan.ticket(),
               self.cause)
    }
}
pub(super) struct Reply
{
    pub(super) status: Status,
    pub(super) bytes:  Vec<u8>
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Acknowledged
{
    Released,
    /// A successful cancellation may carry an already committed operation's
    /// result. Keep that opaque receipt and provider pin for reconciliation.
    ReplyRetained
    {
        bytes:  usize,
        status: Status
    }
}
