//! Allocation-free cleanup refusal retention. A reservation is allocated before
//! publication, never while absorbing failure. Tickets and registration pins
//! remain owned until actual completion; an interrupted destructor is not retried.
use alloc::{boxed::Box, sync::Arc};
use core::fmt;

use spinning_top::Spinlock;

use crate::{domain::{CompletionError, OwnerIndex, PoolError, ReleasedResource, ResourceKey},
            registry::Use,
            shared::{InvocationGate, OwnerRef}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause
{
    Interrupted,
    AwaitingTicket,
    Refused(PoolError)
}
impl fmt::Display for Cause
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Interrupted =>
            {
                f.write_str("resource destruction did not return; ticket and provider pin remain \
                             retained. Arbitrary privileged-fault containment is unproven \
                             (M-D04), a required future gate; restart System until a recovery \
                             contract is proven")
            }
            Self::AwaitingTicket =>
            {
                f.write_str("backing was destroyed while another dispatcher owned its release \
                             ticket; wait for that ticket before completing cleanup")
            }
            Self::Refused(error) =>
            {
                write!(f,
                       "cleanup completion refused: {error}; ticket and provider pin remain \
                        retained for retry")
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault
{
    pub key:   ResourceKey,
    pub cause: Cause
}
impl fmt::Display for Fault
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        write!(f,
               "{} {} slot {} generation {}: {}",
               self.key.owner(),
               self.key.kind(),
               self.key.slot(),
               self.key.generation(),
               self.cause)
    }
}
impl core::error::Error for Fault
{
}

pub(crate) struct Record
{
    key:             Option<ResourceKey>,
    ticket:          Option<ReleasedResource>,
    projection:      Option<crate::projected::ProjectionRelease>,
    usage:           Option<Use>,
    cause:           Cause,
    next:            Option<Box<Record>>,
    recycle:         Option<Arc<Recycling>>,
    acknowledgement: Option<crate::store::AcknowledgementPin>
}
impl Record
{
    fn fault(&self) -> Option<Fault>
    {
        Some(Fault { key:   self.key?,
                     cause: self.cause })
    }
}
// The sole preallocated completion record returns here only after destruction
// and the authoritative ledger ACK. A refusal/interrupted destructor keeps it
// in the existing owner quarantine, so the publisher remains unavailable.
pub(crate) struct Recycling
{
    record: Spinlock<Option<Box<Record>>>,
    give:   fn(&Spinlock<Option<Box<Record>>>, Box<Record>)
}
impl Recycling
{
    pub(crate) fn try_new<G: InvocationGate>() -> Result<Arc<Self>, alloc::alloc::AllocError>
    {
        let mut reserve = Reserve::try_new()?;
        Arc::try_new(Self { record: Spinlock::new(reserve.0.take()),
                            give:   give::<G> })
    }

    pub(crate) fn ready<G: InvocationGate>(&self) -> bool
    {
        G::run(|| self.record.lock().is_some())
    }

    pub(crate) fn take<G: InvocationGate>(self: &Arc<Self>) -> Option<Reserve>
    {
        G::run(|| {
            let mut record = self.record.lock().take()?;
            record.recycle = Some(self.clone());
            Some(Reserve(Some(record)))
        })
    }
}
fn give<G: InvocationGate>(destination: &Spinlock<Option<Box<Record>>>, record: Box<Record>)
{
    G::run(|| {
        let mut slot = destination.lock();
        debug_assert!(slot.is_none(), "one reserved completion record has one return destination");
        if slot.is_none()
        {
            *slot = Some(record);
        }
    });
}

// Completion records which have already received the real ledger ACK may be
// dropped by the walk outside the metadata lock. Its existing drop sites need
// no callbacks, allocations or knowledge of the reserved publisher.
pub(crate) struct Completed(Option<Box<Record>>);
impl Drop for Completed
{
    fn drop(&mut self)
    {
        if let Some(record) = self.0.take()
        {
            finish(record);
        }
    }
}

pub(crate) fn finish(mut record: Box<Record>)
{
    let recycle = record.recycle.take();
    record.key = None;
    record.ticket = None;
    record.next = None;
    // Provider and projection pins are released after the owner gate, before
    // advertising readiness. There is no Arc cycle through a recycled record.
    drop(record.projection.take());
    drop(record.usage.take());
    drop(record.acknowledgement.take());
    record.cause = Cause::Interrupted;
    if let Some(recycle) = recycle
    {
        (recycle.give)(&recycle.record, record);
    }
}

pub(crate) struct Reserve(Option<Box<Record>>);
impl Reserve
{
    pub(crate) fn try_new() -> Result<Self, alloc::alloc::AllocError>
    {
        Box::try_new(Record { key:             None,
                              ticket:          None,
                              projection:      None,
                              usage:           None,
                              cause:           Cause::Interrupted,
                              next:            None,
                              recycle:         None,
                              acknowledgement: None }).map(|record| Self(Some(record)))
    }

    pub(crate) fn with_projection(mut self,
                                  projection: Option<crate::projected::ProjectionRelease>)
                                  -> Self
    {
        if let Some(record) = &mut self.0
        {
            record.projection = projection;
        }
        self
    }

    pub(crate) fn with_acknowledgement(mut self, pin: Option<crate::store::AcknowledgementPin>)
                                       -> Self
    {
        if let Some(record) = &mut self.0
        {
            record.acknowledgement = pin;
        }
        self
    }

    pub(crate) fn live<G: InvocationGate>(mut self, owner: OwnerRef<G>, key: ResourceKey,
                                          usage: Option<Use>)
                                          -> Completion<G>
    {
        let mut record = self.0.take();
        if let Some(record) = &mut record
        {
            record.key = Some(key);
            record.usage = usage;
        }
        Completion { owner,
                     key,
                     record }
    }

    pub(crate) fn released<G: InvocationGate>(self, owner: OwnerRef<G>, ticket: ReleasedResource,
                                              usage: Option<Use>)
                                              -> Completion<G>
    {
        let mut completion = self.live(owner, ticket.key(), usage);
        if let Some(record) = &mut completion.record
        {
            record.ticket = Some(ticket);
        }
        completion
    }
}
impl Drop for Reserve
{
    fn drop(&mut self)
    {
        // Unpublished failure returns the untouched reservation as well.
        if let Some(record) = self.0.take()
        {
            finish(record);
        }
    }
}

/// The payload owner calls `run` around destruction. If destruction unwinds,
/// this guard retains the ticket and pin instead of acknowledging cleanup.
pub(crate) struct Completion<G: InvocationGate>
{
    owner:  OwnerRef<G>,
    key:    ResourceKey,
    record: Option<Box<Record>>
}
impl<G: InvocationGate> Completion<G>
{
    pub(crate) fn key(&self) -> ResourceKey
    {
        self.key
    }

    pub(crate) fn receive(&mut self, ticket: ReleasedResource) -> Result<(), CompletionError>
    {
        if self.key != ticket.key()
           || self.record.as_ref().is_none_or(|record| record.ticket.is_some())
        {
            return Err(<CompletionError as crate::registry::CleanupFailure>::registration_mismatch(ticket));
        }
        if let Some(record) = &mut self.record
        {
            record.ticket = Some(ticket);
        }
        Ok(())
    }

    pub(crate) fn run(&mut self, payload: impl FnOnce())
    {
        let Some(mut record) = self.record.take()
        else
        {
            payload();
            return;
        };
        if record.ticket.is_none()
        {
            record.ticket = self.owner.invalidate(self.key).ok();
        }
        let mut attempt = Attempt { owner:  self.owner.clone(),
                                    record: Some(record) };
        payload();
        if let Some(mut record) = attempt.record.take()
        {
            if let Some(ticket) = record.ticket.take()
            {
                match self.owner.complete_value_release(ticket)
                {
                    Ok(stopped) =>
                    {
                        finish(record);
                        G::resource_released(self.owner.owner());
                        if stopped
                        {
                            G::released(self.owner.owner());
                        }
                    }
                    Err(error) =>
                    {
                        record.cause = Cause::Refused(error.cause());
                        record.ticket = Some(error.into_ticket());
                        self.owner.retain_completion(record);
                    }
                }
            }
            else
            {
                record.cause = Cause::AwaitingTicket;
                self.owner.retain_completion(record);
            }
        }
    }
}
impl<G: InvocationGate> Drop for Completion<G>
{
    fn drop(&mut self)
    {
        if let Some(record) = self.record.take()
        {
            self.owner.retain_completion(record);
        }
    }
}
struct Attempt<G: InvocationGate>
{
    owner:  OwnerRef<G>,
    record: Option<Box<Record>>
}
impl<G: InvocationGate> Drop for Attempt<G>
{
    fn drop(&mut self)
    {
        if let Some(record) = self.record.take()
        {
            self.owner.retain_completion(record);
        }
    }
}

pub(crate) enum Routed
{
    Continue(ReleasedResource),
    Complete(Option<Completed>),
    Retained(Fault)
}
pub(crate) enum Retry
{
    Empty,
    Complete(Completed),
    Retained(Fault)
}
pub(crate) struct Queue
{
    head:   Option<Box<Record>>,
    count:  usize,
    cursor: usize
}
impl Queue
{
    pub(crate) const fn new() -> Self
    {
        Self { head:   None,
               count:  0,
               cursor: 0 }
    }

    pub(crate) fn len(&self) -> usize
    {
        self.count
    }

    pub(crate) fn next_fault(&self) -> Option<Fault>
    {
        self.at(self.cursor)
    }

    pub(crate) fn push(&mut self, mut record: Box<Record>)
    {
        if record.key.is_none()
        {
            return;
        }
        record.next = self.head.take();
        self.head = Some(record);
        self.count += 1;
        self.cursor = 0;
    }

    pub(crate) fn at(&self, index: usize) -> Option<Fault>
    {
        let mut current = self.head.as_deref();
        for _ in 0..index
        {
            current = current?.next.as_deref();
        }
        current?.fault()
    }

    pub(crate) fn count_kind(&self, kind: crate::domain::KindCode) -> usize
    {
        let mut count = 0;
        let mut current = self.head.as_deref();
        while let Some(record) = current
        {
            count += usize::from(record.key.is_some_and(|key| key.kind() == kind));
            current = record.next.as_deref();
        }
        count
    }

    pub(crate) fn route(&mut self, index: &mut OwnerIndex, ticket: ReleasedResource) -> Routed
    {
        let key = ticket.key();
        let mut at = 0;
        let mut current = self.head.as_deref();
        while let Some(record) = current
        {
            if record.key == Some(key) && record.ticket.is_none()
            {
                break;
            }
            at += 1;
            current = record.next.as_deref();
        }
        if current.is_none()
        {
            return Routed::Continue(ticket);
        }
        let mut link = &mut self.head;
        for _ in 0..at
        {
            let Some(record) = link.as_mut()
            else
            {
                return Routed::Continue(ticket);
            };
            link = &mut record.next;
        }
        let Some(record) = link.as_mut()
        else
        {
            return Routed::Continue(ticket);
        };
        if record.cause == Cause::Interrupted
        {
            record.ticket = Some(ticket);
            return Routed::Retained(Fault { key,
                                            cause: Cause::Interrupted });
        }
        match index.complete_value_release(ticket)
        {
            Ok(()) =>
            {
                let mut removed = link.take();
                if let Some(record) = &mut removed
                {
                    *link = record.next.take();
                }
                self.count -= 1;
                self.cursor = 0;
                Routed::Complete(removed.map(|record| Completed(Some(record))))
            }
            Err(error) =>
            {
                record.cause = Cause::Refused(error.cause());
                record.ticket = Some(error.into_ticket());
                Routed::Retained(Fault { key,
                                         cause: record.cause })
            }
        }
    }

    pub(crate) fn retry_one(&mut self, index: &mut OwnerIndex) -> Retry
    {
        if self.count == 0
        {
            return Retry::Empty;
        }
        let at = self.cursor.min(self.count - 1);
        let mut link = &mut self.head;
        for _ in 0..at
        {
            let Some(record) = link.as_mut()
            else
            {
                return Retry::Empty;
            };
            link = &mut record.next;
        }
        let Some(record) = link.as_mut()
        else
        {
            return Retry::Empty;
        };
        let Some(key) = record.key
        else
        {
            return Retry::Empty;
        };
        if matches!(record.cause, Cause::Refused(_))
        {
            if let Some(ticket) = record.ticket.take()
            {
                match index.complete_value_release(ticket)
                {
                    Ok(()) =>
                    {
                        let Some(mut removed) = link.take()
                        else
                        {
                            return Retry::Empty;
                        };
                        *link = removed.next.take();
                        self.count -= 1;
                        self.cursor = if self.count == 0 { 0 } else { at % self.count };
                        return Retry::Complete(Completed(Some(removed)));
                    }
                    Err(error) =>
                    {
                        record.cause = Cause::Refused(error.cause());
                        record.ticket = Some(error.into_ticket());
                    }
                }
            }
        }
        self.cursor = (at + 1) % self.count;
        Retry::Retained(Fault { key,
                                cause: record.cause })
    }
}
impl Drop for Queue
{
    fn drop(&mut self)
    {
        // World/manager destruction cannot acknowledge an interrupted cleanup.
        if let Some(head) = self.head.take()
        {
            core::mem::forget(head);
        }
    }
}
