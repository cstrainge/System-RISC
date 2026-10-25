//! Owned deferred requests. Take a dispatch out of this queue before invoking
//! foreign code, then return it after the callback. No app pointer or registry
//! guard crosses that boundary. Every live job has a shared generation ID.
#![forbid(unsafe_code)]
use alloc::{collections::VecDeque, vec::Vec};

use mac_abi::{ACCESS_DENIED, BAD_ABI, BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, SERVICE_BUSY, Status};
use mac_module::ClassId;

use crate::toolbox::handles::{Kind, Table};
const MAX_PAYLOAD: usize = 1024 * 1024;
const MAX_OWNED_BYTES: usize = 1024 * 1024;
const MAX_JOBS: usize = Kind::DriverJobs.capacity();
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
    pub owner:     Owner,
    pub provider:  u64,
    pub class:     ClassId,
    pub operation: u32,
    pub ticket:    &'a mut u32,
    pub phase:     Phase,
    pub input:     &'a [u8],
    pub output:    &'a mut [u8]
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
    ticket:       u32,
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
    pub job:    u32
}
enum State
{
    Ready(Work),
    Running,
    Finished(Completed)
}
struct Record
{
    owner:     Owner,
    waiter:    Option<u64>,
    state:     State,
    cancel:    bool,
    discard:   bool,
    footprint: usize
}
pub struct Queue
{
    records:     Table<Record>,
    ready:       VecDeque<u32>,
    owned_bytes: usize
}
impl Default for Queue
{
    fn default() -> Self
    {
        Self::new()
    }
}
#[must_use = "return the owned dispatch with Queue::finish after invoking it"]
#[derive(Debug)]
pub struct Dispatch
{
    id:      u32,
    owner:   Owner,
    work:    Work,
    cancel:  bool,
    outcome: Option<Outcome>
}
impl Dispatch
{
    pub fn invoke(&mut self, broker: &mut impl Broker)
    {
        let phase = if self.work.ticket == 0
        {
            Phase::Start
        }
        else if self.cancel || self.work.forced_error.is_some()
        {
            Phase::Cancel
        }
        else
        {
            Phase::Poll
        };
        let Work { provider,
                   class,
                   operation,
                   input,
                   output,
                   ticket,
                   .. } = &mut self.work;
        let borrowed_input = if phase == Phase::Start { input.as_slice() } else { &[] };
        self.outcome = Some(broker.invoke(Call { owner: self.owner,
                                                 provider: *provider,
                                                 class: *class,
                                                 operation: *operation,
                                                 ticket,
                                                 phase,
                                                 input: borrowed_input,
                                                 output }));
    }
}
impl Queue
{
    pub const fn new() -> Self
    {
        Self { records:     Table::new(Kind::DriverJobs),
               ready:       VecDeque::new(),
               owned_bytes: 0 }
    }

    pub fn has_ready(&self) -> bool
    {
        !self.ready.is_empty()
    }

    pub fn submit(&mut self, owner: Owner, work: Work) -> Result<u32, SubmissionError>
    {
        let Some(footprint) = work.footprint().filter(|n| {
                                                  self.owned_bytes
                                                      .checked_add(*n)
                                                      .is_some_and(|total| total <= MAX_OWNED_BYTES)
                                              })
        else
        {
            return Err(SubmissionError { status: NO_MEMORY,
                                         work });
        };
        // Reserve for every possible completion before any callback runs. A
        // pending request can be requeued without allocation or losing its data.
        if self.ready.capacity() < MAX_JOBS && self.ready.try_reserve(MAX_JOBS).is_err()
        {
            return Err(SubmissionError { status: NO_MEMORY,
                                         work });
        }
        let mut work = Some(work);
        let result = self.records.insert_with(|_| {
                                     Record { owner,
                                              waiter: if let Owner::Thread(thread) = owner
                                              {
                                                  Some(thread)
                                              }
                                              else
                                              {
                                                  None
                                              },
                                              state: State::Ready(work.take().unwrap()),
                                              cancel: false,
                                              discard: false,
                                              footprint }
                                 });
        match result
        {
            Ok(id) =>
            {
                self.ready.push_back(id);
                self.owned_bytes += footprint;
                Ok(id)
            }
            Err(_) =>
            {
                Err(SubmissionError { status: NO_MEMORY,
                                      work:   work.unwrap() })
            }
        }
    }

    fn owned(&mut self, owner: Owner, id: u32) -> Result<&mut Record, Status>
    {
        let record = self.records.get_mut(id).ok_or(NOT_FOUND)?;
        if record.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        Ok(record)
    }

    pub fn pending(&mut self, owner: Owner, id: u32) -> Result<bool, Status>
    {
        Ok(!matches!(self.owned(owner, id)?.state, State::Finished(_)))
    }

    /// Resident-only attachment: authority remains with owner; a waiter cannot
    /// take/cancel/discard the result or replace another thread's attachment.
    pub fn attach_waiter(&mut self, owner: Owner, id: u32, thread: u64) -> Result<(), Status>
    {
        if thread <= 1
        {
            return Err(BAD_ARGUMENT);
        }
        let record = self.owned(owner, id)?;
        match owner
        {
            Owner::Kernel => (),
            Owner::Thread(own) if own == thread => (),
            _ => return Err(ACCESS_DENIED)
        }
        if record.waiter.is_some_and(|old| old != thread)
        {
            return Err(SERVICE_BUSY);
        }
        record.waiter = Some(thread);
        Ok(())
    }

    /// The scheduler supplies the actual generation-tagged OS thread ID.
    pub fn pending_waiter(&self, thread: u64, id: u32) -> Result<bool, Status>
    {
        let record = self.records.get(id).ok_or(NOT_FOUND)?;
        if record.waiter != Some(thread)
        {
            return Err(ACCESS_DENIED);
        }
        Ok(!matches!(record.state, State::Finished(_)))
    }

    pub fn detach_waiter(&mut self, thread: u64, id: u32) -> Result<(), Status>
    {
        let record = self.records.get_mut(id).ok_or(NOT_FOUND)?;
        if record.waiter != Some(thread)
        {
            return Err(ACCESS_DENIED);
        }
        record.waiter = None;
        Ok(())
    }

    pub fn completed_waiter(&mut self, owner: Owner, id: u32) -> Result<Option<u64>, Status>
    {
        let record = self.owned(owner, id)?;
        Ok(if matches!(record.state, State::Finished(_)) { record.waiter } else { None })
    }

    pub fn next_dispatch(&mut self) -> Option<Dispatch>
    {
        while let Some(id) = self.ready.pop_front()
        {
            let Some(record) = self.records.get_mut(id)
            else
            {
                continue;
            };
            if !matches!(record.state, State::Ready(_))
            {
                continue;
            }
            let State::Ready(work) = core::mem::replace(&mut record.state, State::Running)
            else
            {
                unreachable!()
            };
            return Some(Dispatch { id,
                                   owner: record.owner,
                                   work,
                                   cancel: record.cancel,
                                   outcome: None });
        }
        None
    }

    /// Claim one exact owned ready job without running unrelated callbacks.
    /// Dispatch ownership and completion use the ordinary queue protocol.
    pub fn dispatch_for(&mut self, owner: Owner, id: u32) -> Result<Option<Dispatch>, Status>
    {
        if !matches!(self.owned(owner, id)?.state, State::Ready(_))
        {
            return Ok(None);
        }
        let Some(index) = self.ready.iter().position(|ready| *ready == id)
        else
        {
            return Ok(None);
        };
        self.ready.remove(index);
        self.ready.push_front(id);
        Ok(self.next_dispatch())
    }

    /// A rejected/foreign completion returns its owned dispatch to the caller;
    /// a still-live broker ticket and its buffers must never be silently dropped.
    pub fn finish(&mut self, mut dispatch: Dispatch) -> Result<Option<CompletionNotice>, Dispatch>
    {
        let Some(record) =
            self.records
                .get_mut(dispatch.id)
                .filter(|r| r.owner == dispatch.owner && matches!(r.state, State::Running))
        else
        {
            return Err(dispatch);
        };
        let Some(outcome) = dispatch.outcome.take()
        else
        {
            return Err(dispatch);
        };
        if outcome == Outcome::Contended && !(record.cancel && dispatch.work.ticket == 0)
        {
            record.state = State::Ready(dispatch.work);
            self.ready.push_back(dispatch.id);
            return Ok(None);
        }
        if dispatch.work.ticket != 0
        {
            match outcome
            {
                Outcome::Pending | Outcome::Contended | Outcome::Failed(SERVICE_BUSY) => (),
                Outcome::Failed(error) => dispatch.work.forced_error = Some(error),
                Outcome::Complete(_) => dispatch.work.forced_error = Some(BAD_ABI)
            }
            record.state = State::Ready(dispatch.work);
            self.ready.push_back(dispatch.id);
            return Ok(None);
        }
        let result = if let Some(error) = dispatch.work.forced_error
        {
            Err(Failure::Service(error))
        }
        else
        {
            match outcome
            {
                Outcome::Pending => Err(Failure::Service(BAD_ABI)),
                Outcome::Contended => Err(Failure::Cancelled),
                Outcome::Failed(error) => Err(Failure::Service(error)),
                Outcome::Complete(length) if length <= dispatch.work.output.len() =>
                {
                    dispatch.work.output.truncate(length);
                    Ok(dispatch.work.output)
                }
                Outcome::Complete(_) => Err(Failure::Service(BAD_ABI))
            }
        };
        if record.discard
        {
            self.remove(dispatch.id);
            return Ok(None);
        }
        record.state = State::Finished(Completed { result,
                                                   cancellation_requested: record.cancel });
        Ok(Some(CompletionNotice { owner:  record.owner,
                                   waiter: record.waiter,
                                   job:    dispatch.id }))
    }

    fn remove(&mut self, id: u32)
    {
        if let Some(record) = self.records.remove(id)
        {
            self.owned_bytes -= record.footprint;
        }
        self.ready.retain(|queued| *queued != id);
    }

    pub fn take(&mut self, owner: Owner, id: u32) -> Result<Option<Completed>, Status>
    {
        if !matches!(self.owned(owner, id)?.state, State::Finished(_))
        {
            return Ok(None);
        }
        let record = self.records.remove(id).ok_or(NOT_FOUND)?;
        self.owned_bytes -= record.footprint;
        let State::Finished(result) = record.state
        else
        {
            unreachable!()
        };
        Ok(Some(result))
    }

    pub fn cancel(&mut self, owner: Owner, id: u32) -> Result<(), Status>
    {
        let record = self.owned(owner, id)?;
        if record.cancel || matches!(record.state, State::Finished(_))
        {
            return Ok(());
        }
        record.cancel = true;
        if matches!(&record.state, State::Ready(work) if work.ticket == 0)
        {
            record.state = State::Finished(Completed { result:
                                                           Err(Failure::Cancelled),
                                                       cancellation_requested: true });
            self.ready.retain(|queued| *queued != id);
        }
        Ok(())
    }

    pub fn discard(&mut self, owner: Owner, id: u32) -> Result<(), Status>
    {
        self.cancel(owner, id)?;
        let record = self.owned(owner, id)?;
        record.discard = true;
        if matches!(record.state, State::Finished(_))
        {
            self.remove(id);
        }
        Ok(())
    }

    pub fn discard_owner(&mut self, owner: Owner)
    {
        // A dead waiter cannot cancel a resident-owned mutation or consume its
        // result. Detach its exact thread generation before retiring app jobs.
        if let Owner::Thread(thread) = owner
        {
            for record in self.records.values_mut()
            {
                if record.waiter == Some(thread)
                {
                    record.waiter = None;
                }
            }
        }
        let mut ids = [0; MAX_JOBS];
        let mut count = 0;
        for (id, record) in self.records.iter()
        {
            if record.owner == owner
            {
                ids[count] = id;
                count += 1;
            }
        }
        for &id in &ids[..count]
        {
            let _ = self.discard(owner, id);
        }
    }
}

#[cfg(test)]
mod tests;
