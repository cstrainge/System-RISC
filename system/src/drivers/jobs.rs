//! Owned deferred requests. An invocation borrows its originating queue and
//! returns its work through Drop. No app pointer or registry guard crosses a
//! callback. Every live job has a shared generation ID.
#![forbid(unsafe_code)]
use alloc::{boxed::Box, collections::VecDeque, sync::Arc, vec::Vec};

use mac_abi::{ACCESS_DENIED, BAD_ABI, BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, SERVICE_BUSY, Status};
use mac_module::ClassId;

const MAX_PAYLOAD: usize = 1024 * 1024;
const MAX_OWNED_BYTES: usize = 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner
{
    Kernel,
    Thread(u64),
    Module(u64)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase
{
    Start,
    Poll,
    Cancel
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome
{
    Complete(usize),
    Pending,
    /// Admission was busy; no provider callback ran. Keep the owned request.
    Contended,
    Failed(Status)
}
pub struct Call<'a>
{
    pub(crate) subject: &'a subjects::Subject,
    pub owner:          Owner,
    pub provider:       u64,
    pub class:          ClassId,
    pub operation:      u32,
    pub ticket:         &'a mut u64,
    pub phase:          Phase,
    pub input:          &'a [u8],
    pub output:         &'a mut [u8]
}
pub trait Broker
{
    fn invoke(&mut self, call: Call<'_>) -> Outcome;
}
#[derive(Debug)]
pub struct Work
{
    provider:     u64,
    class:        ClassId,
    operation:    u32,
    input:        Vec<u8>,
    output:       Vec<u8>,
    ticket:       u64,
    forced_error: Option<Status>
}
impl Work
{
    pub fn new(provider: u64, class: ClassId, operation: u32, input: Vec<u8>, output_bytes: usize)
               -> Result<Self, Status>
    {
        if provider == 0
           || class.0 == [0; 4]
           || input.len() > MAX_PAYLOAD
           || output_bytes > MAX_PAYLOAD
        {
            return Err(BAD_ARGUMENT);
        }
        let mut output = Vec::new();
        output.try_reserve_exact(output_bytes).map_err(|_| NO_MEMORY)?;
        output.resize(output_bytes, 0);
        Ok(Self { provider,
                  class,
                  operation,
                  input,
                  output,
                  ticket: 0,
                  forced_error: None })
    }

    fn footprint(&self) -> Option<usize>
    {
        self.input.capacity().checked_add(self.output.capacity())
    }
}
#[derive(Debug)]
pub struct SubmissionError
{
    pub status: Status,
    pub work:   Work
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure
{
    Service(Status),
    Cancelled
}
#[derive(Debug)]
pub struct Completed
{
    pub result:                 Result<Vec<u8>, Failure>,
    pub cancellation_requested: bool
}
/// Result ownership and scheduler wakeup are distinct. A resident service may
/// own a mutation's result while an application thread merely waits for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompletionNotice
{
    pub owner:  Owner,
    pub waiter: Option<u64>,
    pub job:    u64
}
enum State
{
    Admitting,
    Ready(Box<Node>),
    Running,
    Finished(Completed),
    Quarantined,
    Released
}
struct Record
{
    receipt:   Option<mac_object_table::store::CompletionKey>,
    owner:     Owner,
    waiter:    Option<u64>,
    state:     State,
    cancel:    bool,
    discard:   bool,
    footprint: usize
}
struct Node
{
    id:       u64,
    receipt:  Option<mac_object_table::store::CompletionKey>,
    owner:    Owner,
    work:     Option<Work>,
    cancel:   bool,
    outcome:  Option<Outcome>,
    next:     Option<Box<Node>>,
    failure:  Option<ReturnCause>,
    reported: bool,
    // Release buffers before the final operation use and its cleanup ACK.
    _subject: subjects::Subject
}
struct Core
{
    records:     records::Records,
    ready:       VecDeque<mac_object_table::store::CompletionKey>,
    owned_bytes: usize,
    notices:     VecDeque<CompletionNotice>,
    quarantined: Option<Box<Node>>,
    unreported:  usize,
    #[cfg(test)]
    fixture:     Option<tests::Fixture>
}

mod state;
pub(crate) mod subjects;
pub(crate) mod records;
mod dispatch;
mod queue;
pub use dispatch::{Dispatch, ReturnCause, ReturnFailure};
pub use queue::Queue;
#[cfg(test)]
mod tests;
