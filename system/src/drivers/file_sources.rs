//! Private leased-fork capabilities owned by the admitted module's ledger.
//! Completion authority is minted at publication and travels in owned plans;
//! losing a plan quarantines the protocol rather than acknowledging its I/O.
#![forbid(unsafe_code)]
use alloc::{collections::VecDeque, sync::Arc, vec::Vec};

use mac_abi::{ACCESS_DENIED, ALREADY_EXISTS, BAD_ABI, BAD_ARGUMENT, NO_MEMORY, NOT_FOUND,
              ROLLBACK_FAILED, SERVICE_BUSY, Status};
use mac_file_source_interface::{Info, MAX_TRANSFER};
use mac_object_table::{domain::{PoolError, ReleasedResource},
                       registry::CleanupFailure,
                       shared::InvocationGate as _,
                       store::CompletionKey};
use spinning_top::Spinlock;

use crate::toolbox::handles::{InvocationGate, KindLease, OwnerRef, Store, cleanup::Failure};
#[cfg(target_arch = "riscv64")]
pub(crate) mod runtime;
pub(crate) mod lifetime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity
{
    pub volume: u32,
    pub cnid:   u32,
    pub fork:   u32
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase
{
    Ready,
    Reading(u64),
    UncertainRead(u64),
    Closing,
    UncertainClose,
    FailedClose,
    Closed
}
struct State(Spinlock<Phase>);
impl State
{
    fn access<R>(&self, run: impl FnOnce(&mut Phase) -> R) -> R
    {
        InvocationGate::run(|| run(&mut self.0.lock()))
    }

    fn phase(&self) -> Phase
    {
        self.access(|phase| *phase)
    }
}
struct Source<B>
{
    id:       u64,
    identity: Identity,
    backing:  B,
    length:   u64,
    state:    Arc<State>
}

/// The only authority for finishing this exact read. It cannot be constructed
/// from a source number, cloned, or replaced with a later read of a reused slot.
/// Dropping it without actual lower ACK leaves its source quarantined.
pub struct Read<B>
{
    pub backing: B,
    pub offset:  u64,
    pub length:  usize,
    receipt:     CompletionKey,
    ticket:      u64,
    state:       Arc<State>,
    active:      bool
}
impl<B> Read<B>
{
    pub(crate) fn ticket(&self) -> u64
    {
        self.ticket
    }
}
impl<B> Drop for Read<B>
{
    fn drop(&mut self)
    {
        if self.active
        {
            self.state.access(|phase| {
                          if *phase == Phase::Reading(self.ticket)
                          {
                              *phase = Phase::UncertainRead(self.ticket);
                          }
                      });
        }
    }
}
/// Backing and original publication authority stay owned until real Close ACK.
/// The runtime owns this plan across asynchronous close; clients only observe it.
pub(crate) struct Close<B>
{
    pub(crate) backing: B,
    previous:           Phase,
    receipt:            CompletionKey,
    state:              Arc<State>,
    active:             bool
}
impl<B> Drop for Close<B>
{
    fn drop(&mut self)
    {
        if self.active
        {
            self.state.access(|phase| {
                          if *phase == Phase::Closing
                          {
                              *phase = Phase::UncertainClose;
                          }
                      });
        }
    }
}
/// Refusal never consumes or recreates an already admitted completion plan.
/// Its recipient can retry in the original manager; dropping it fails closed.
pub(crate) struct Refusal<P>
{
    pub(crate) cause: Status,
    pub(crate) plan:  P
}
impl<P> Refusal<P>
{
    pub(crate) fn into_parts(self) -> (Status, P)
    {
        (self.cause, self.plan)
    }
}
impl<P> core::fmt::Debug for Refusal<P>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        f.debug_struct("BackingCompletionRefusal")
         .field("cause", &self.cause)
         .finish_non_exhaustive()
    }
}
impl<P> core::fmt::Display for Refusal<P>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        write!(f,
               "backing-fork completion refused with status {}; original plan remains owned",
               self.cause)
    }
}
struct Record
{
    receipt:       CompletionKey,
    pending:       Option<ReleasedResource>,
    terminal:      bool,
    queued:        bool,
    failure:       Option<PoolError>,
    last_io_error: Option<Status>
}
pub struct Sources<B>
{
    entries: Store<Source<B>>,
    records: Vec<Record>,
    ready:   VecDeque<CompletionKey>
}
impl<B: Clone> Sources<B>
{
    pub(crate) fn new(kinds: (KindLease, KindLease)) -> Result<Self, Status>
    {
        Ok(Self { entries: Store::owned(kinds.0, kinds.1).map_err(pool_status)?,
                  records: Vec::new(),
                  ready:   VecDeque::new() })
    }

    fn at(&self, id: u64) -> Option<usize>
    {
        self.records.binary_search_by_key(&id, |record| record.receipt.id()).ok()
    }

    fn owned(&self, owner: &OwnerRef, id: u64) -> Result<&Source<B>, Status>
    {
        let (actual, source) = self.entries.get_with_owner(id).ok_or(NOT_FOUND)?;
        if actual != owner.owner()
        {
            return Err(ACCESS_DENIED);
        }
        Ok(source)
    }

    fn accessible(source: &Source<B>) -> Result<(), Status>
    {
        match source.state.phase()
        {
            Phase::Ready | Phase::Reading(_) => Ok(()),
            Phase::Closing => Err(SERVICE_BUSY),
            Phase::UncertainRead(_) | Phase::UncertainClose | Phase::FailedClose =>
            {
                Err(ROLLBACK_FAILED)
            }
            Phase::Closed => Err(NOT_FOUND)
        }
    }

    pub(crate) fn find(&self, owner: &OwnerRef, identity: Identity) -> Result<Option<u64>, Status>
    {
        let Some((id, source)) =
            self.entries
                .iter()
                .find(|(id, source)| {
                    self.entries.owner(*id) == Some(owner.owner()) && source.identity == identity
                })
        else
        {
            return Ok(None);
        };
        Self::accessible(source)?;
        Ok(Some(id))
    }

    /// Publish only after an owned backing open has actually acknowledged.
    /// Refusal leaves the adapter responsible for closing that exact backing.
    pub(crate) fn publish(&mut self, owner: &OwnerRef, identity: Identity, backing: &B,
                          length: u64)
                          -> Result<u64, Status>
    {
        if !owner.accepting()
        {
            return Err(SERVICE_BUSY);
        }
        if identity.volume == 0 || identity.cnid < 2 || identity.fork > 1
        {
            return Err(BAD_ARGUMENT);
        }
        if self.find(owner, identity)?.is_some()
        {
            return Err(ALREADY_EXISTS);
        }
        let needed = self.records.len().checked_add(1).ok_or(NO_MEMORY)?;
        InvocationGate::with_system_allocation(|| {
            self.records.try_reserve(1).map_err(|_| NO_MEMORY)?;
            self.ready.try_reserve(needed.saturating_sub(self.ready.len())).map_err(|_| NO_MEMORY)
        })?;
        let state = Arc::try_new(State(Spinlock::new(Phase::Ready))).map_err(|_| NO_MEMORY)?;
        let (id, receipt) = self.entries
                                .insert_with_receipt(owner, |id| {
                                    Source { id,
                                             identity,
                                             backing: backing.clone(),
                                             length,
                                             state }
                                })
                                .map_err(pool_status)?
                                .into_parts();
        // IDs stay opaque; this is only an ordered index of genuine receipts.
        let at = self.records.partition_point(|record| record.receipt.id() < id);
        self.records.insert(at,
                            Record { receipt,
                                     pending: None,
                                     terminal: false,
                                     queued: false,
                                     failure: None,
                                     last_io_error: None });
        Ok(id)
    }

    pub(crate) fn info(&self, owner: &OwnerRef, id: u64) -> Result<Info, Status>
    {
        let source = self.owned(owner, id)?;
        Self::accessible(source)?;
        Ok(Info { length: source.length })
    }

    pub(crate) fn begin_read(&mut self, owner: &OwnerRef, id: u64, ticket: u64, offset: u64,
                             capacity: usize)
                             -> Result<Read<B>, Status>
    {
        if ticket == 0 || capacity > MAX_TRANSFER
        {
            return Err(BAD_ARGUMENT);
        }
        let entry = self.owned(owner, id)?;
        Self::accessible(entry)?;
        if entry.state.phase() != Phase::Ready
        {
            return Err(SERVICE_BUSY);
        }
        let length =
            entry.length.checked_sub(offset).ok_or(BAD_ARGUMENT)?.min(capacity as u64) as usize;
        let receipt = self.records[self.at(id).ok_or(BAD_ABI)?].receipt;
        let read = Read { backing: entry.backing.clone(),
                          offset,
                          length,
                          receipt,
                          ticket,
                          state: entry.state.clone(),
                          active: true };
        entry.state.access(|phase| *phase = Phase::Reading(ticket));
        Ok(read)
    }

    fn read_source(&self, read: &Read<B>) -> Result<&Source<B>, Status>
    {
        let source = self.entries.completing(&read.receipt).ok_or(NOT_FOUND)?;
        if !Arc::ptr_eq(&source.state, &read.state)
        {
            return Err(ACCESS_DENIED);
        }
        match source.state.phase()
        {
            Phase::Reading(ticket) | Phase::UncertainRead(ticket) if ticket == read.ticket =>
            {
                Ok(source)
            }
            _ => Err(BAD_ABI)
        }
    }

    /// Called only after actual backend acknowledgement, including errors and
    /// cancellation. A quarantined ordinary ID is never looked up or revived.
    pub(crate) fn finish_read(&mut self, mut read: Read<B>) -> Result<(), Refusal<Read<B>>>
    {
        let result = self.read_source(&read).map(|source| source.state.clone());
        let state = match result
        {
            Ok(state) => state,
            Err(cause) =>
            {
                return Err(Refusal { cause,
                                     plan: read });
            }
        };
        let Some(at) = self.at(read.receipt.id())
        else
        {
            return Err(Refusal { cause: BAD_ABI,
                                 plan:  read });
        };
        state.access(|phase| *phase = Phase::Ready);
        read.active = false;
        self.enqueue(at);
        Ok(())
    }

    pub(crate) fn quarantine_read(&self, read: &Read<B>) -> Result<(), Status>
    {
        self.read_source(read)?.state.access(|phase| *phase = Phase::UncertainRead(read.ticket));
        Ok(())
    }

    fn closing(&self, receipt: CompletionKey) -> Result<Close<B>, Status>
    {
        let entry = self.entries.completing(&receipt).ok_or(NOT_FOUND)?;
        let previous = entry.state.phase();
        match previous
        {
            Phase::Ready | Phase::FailedClose => (),
            Phase::Reading(_) | Phase::Closing => return Err(SERVICE_BUSY),
            Phase::UncertainRead(_) | Phase::UncertainClose => return Err(ROLLBACK_FAILED),
            Phase::Closed => return Err(NOT_FOUND)
        }
        let close = Close { backing: entry.backing.clone(),
                            previous,
                            receipt,
                            state: entry.state.clone(),
                            active: true };
        entry.state.access(|phase| *phase = Phase::Closing);
        Ok(close)
    }

    pub(crate) fn begin_close(&mut self, owner: &OwnerRef, id: u64) -> Result<Close<B>, Status>
    {
        self.owned(owner, id)?;
        self.closing(self.records[self.at(id).ok_or(BAD_ABI)?].receipt)
    }

    fn close_source(&self, close: &Close<B>) -> Result<&Source<B>, Status>
    {
        let entry = self.entries.completing(&close.receipt).ok_or(NOT_FOUND)?;
        if !Arc::ptr_eq(&entry.state, &close.state)
        {
            return Err(ACCESS_DENIED);
        }
        if !matches!(entry.state.phase(), Phase::Closing | Phase::UncertainClose)
        {
            return Err(BAD_ABI);
        }
        Ok(entry)
    }

    pub(crate) fn quarantine_close(&self, close: &Close<B>) -> Result<(), Status>
    {
        self.close_source(close)?.state.access(|phase| *phase = Phase::UncertainClose);
        Ok(())
    }

    /// The adapter first retires its actual backing mark on a real Close ACK.
    /// A dispatcher that has not arrived still owns the source's unique ticket;
    /// terminal state is retained until it arrives, never acknowledged by guess.
    pub(crate) fn finish_close(&mut self, mut close: Close<B>) -> Result<(), Refusal<Close<B>>>
    {
        let state = match self.close_source(&close).map(|source| source.state.clone())
        {
            Ok(state) => state,
            Err(cause) =>
            {
                return Err(Refusal { cause,
                                     plan: close });
            }
        };
        let Some(at) = self.at(close.receipt.id())
        else
        {
            return Err(Refusal { cause: BAD_ABI,
                                 plan:  close });
        };
        state.access(|phase| *phase = Phase::Closed);
        close.active = false;
        self.records[at].terminal = true;
        self.retire(at);
        Ok(())
    }

    /// The adapter proved that no lower request was published. Restore the
    /// prior phase, preserving a previous real close failure if one existed.
    pub(crate) fn close_unsubmitted(&mut self, mut close: Close<B>)
                                    -> Result<(), Refusal<Close<B>>>
    {
        let state = match self.close_source(&close).map(|source| source.state.clone())
        {
            Ok(state) => state,
            Err(cause) =>
            {
                return Err(Refusal { cause,
                                     plan: close });
            }
        };
        let Some(at) = self.at(close.receipt.id())
        else
        {
            return Err(Refusal { cause: BAD_ABI,
                                 plan:  close });
        };
        state.access(|phase| *phase = close.previous);
        close.active = false;
        self.enqueue(at);
        Ok(())
    }

    /// A real terminal error acknowledges the request, not release of its
    /// backing lease. Reads stay refused; a subsequent close may retry safely.
    pub(crate) fn close_failed(&mut self, mut close: Close<B>) -> Result<(), Refusal<Close<B>>>
    {
        let state = match self.close_source(&close).map(|source| source.state.clone())
        {
            Ok(state) => state,
            Err(cause) =>
            {
                return Err(Refusal { cause,
                                     plan: close });
            }
        };
        let Some(at) = self.at(close.receipt.id())
        else
        {
            return Err(Refusal { cause: BAD_ABI,
                                 plan:  close });
        };
        state.access(|phase| *phase = Phase::FailedClose);
        close.active = false;
        self.enqueue(at);
        Ok(())
    }

    fn enqueue(&mut self, at: usize)
    {
        if self.records[at].queued
           || self.records[at].pending.is_none()
           || self.records[at].terminal
        {
            return;
        }
        let receipt = self.records[at].receipt;
        if self.entries
               .completing(&receipt)
               .is_some_and(|entry| matches!(entry.state.phase(), Phase::Ready | Phase::FailedClose))
        {
            self.records[at].queued = true;
            self.ready.push_back(receipt);
        }
    }

    /// The common callback queues intent; actual I/O runs after its context
    /// borrow ends. Each queued source produces at most one owned close plan.
    pub(crate) fn next_cleanup(&mut self) -> Result<Option<Close<B>>, Status>
    {
        while let Some(receipt) = self.ready.pop_front()
        {
            let Some(at) = self.at(receipt.id())
            else
            {
                continue;
            };
            self.records[at].queued = false;
            if self.records[at].terminal
            {
                continue;
            }
            return self.closing(receipt).map(Some);
        }
        Ok(None)
    }

    fn retire(&mut self, at: usize)
    {
        let record = &mut self.records[at];
        let removed = if let Some(ticket) = record.pending.take()
        {
            match self.entries.release(ticket)
            {
                Ok(removed) => Some(removed),
                Err(error) =>
                {
                    let cause = error.cause();
                    #[cfg(target_arch = "riscv64")]
                    if record.failure != Some(cause)
                    {
                        serial!("BACKING_SOURCE_RETIRE_REFUSED source={} cause={}",
                                record.receipt.id(),
                                cause);
                    }
                    record.failure = Some(cause);
                    record.pending = Some(error.into_ticket());
                    None
                }
            }
        }
        else
        {
            self.entries.remove(record.receipt.id())
        };
        if removed.is_some()
        {
            self.records.remove(at);
        }
        drop(removed);
    }

    pub(crate) fn release(&mut self, ticket: ReleasedResource, storage: bool)
                          -> Result<(), Failure>
    {
        if storage
        {
            let mut at = self.records.len();
            while at != 0
            {
                at -= 1;
                if self.records[at].terminal && self.records[at].pending.is_some()
                {
                    self.retire(at);
                }
            }
            return self.entries.release_storage(ticket).map_err(Failure::from);
        }
        let Some(source) = self.entries.releasing(&ticket)
        else
        {
            return Err(Failure::registration_mismatch(ticket));
        };
        let Some(at) = self.at(source.id)
        else
        {
            return Err(Failure::registration_mismatch(ticket));
        };
        if self.records[at].pending.is_some()
        {
            return Err(Failure::registration_mismatch(ticket));
        }
        self.records[at].pending = Some(ticket);
        if self.records[at].terminal
        {
            self.retire(at);
        }
        else
        {
            self.enqueue(at);
        }
        Ok(())
    }
}
impl<B> Drop for Sources<B>
{
    fn drop(&mut self)
    {
        self.entries.quarantine_live();
        // Losing the manager is not Close ACK. Keep original received tickets
        // just as the native store retains its uncertain payloads and backing.
        if self.records.iter().any(|record| record.pending.is_some())
        {
            core::mem::forget(core::mem::take(&mut self.records));
        }
    }
}
fn pool_status(error: PoolError) -> Status
{
    crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error))
}
#[cfg(test)]
mod tests;
