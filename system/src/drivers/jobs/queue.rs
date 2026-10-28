//! One IRQ/preemption-audited lock boundary, including dispatch guard Drop.
use core::sync::atomic::{AtomicU8, Ordering};

use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use super::*;
use crate::toolbox::handles::InvocationGate;
const READY: u8 = 1;
const NOTICE: u8 = 2;
const FAILURE: u8 = 4;
pub struct Queue
{
    core:   Spinlock<Core>,
    signal: AtomicU8
}
impl Default for Queue
{
    fn default() -> Self
    {
        Self::new()
    }
}
impl Queue
{
    pub const fn new() -> Self
    {
        Self { core:   Spinlock::new(Core::new()),
               signal: AtomicU8::new(0) }
    }

    fn access<R>(&self, operation: impl FnOnce(&mut Core) -> R) -> R
    {
        crate::toolbox::handles::InvocationGate::run(|| {
            let mut core = self.core.lock();
            let result = operation(&mut core);
            let signal = u8::from(!core.ready.is_empty()) * READY
                         | u8::from(!core.notices.is_empty()) * NOTICE
                         | u8::from(core.unreported != 0 || core.records.has_failures()) * FAILURE;
            if self.signal.load(Ordering::Relaxed) != signal
            {
                self.signal.store(signal, Ordering::Release);
            }
            result
        })
    }

    pub fn has_ready(&self) -> bool
    {
        self.signal.load(Ordering::Acquire) != 0
    }

    #[cfg(target_arch = "riscv64")]
    fn prepare_records(core: &mut Core) -> Result<(), Status>
    {
        if core.records.prepared()
        {
            return Ok(());
        }
        let kinds = crate::toolbox::handles::registry(|registry| {
                        Ok((registry.get(crate::toolbox::handles::KindCode(*b"DJOB"))?,
                            registry.get(crate::toolbox::handles::KindCode(*b"DJST"))?))
                    }).map_err(|error| error.status())?
                      .map_err(|error| crate::toolbox::handles::registry_status(&error))?;
        core.records.prepare(kinds)
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn reserve_publication(&self, owner: &crate::toolbox::handles::OwnerRef)
        -> Result<mac_object_table::store::PublicationReservation<crate::toolbox::handles::InvocationGate>, Status>
{
        self.access(|core| {
                Self::prepare_records(core)?;
                let reservation = core.records.reserve_publication(owner)?;
                let needed = core.records.len() + core.records.reserved_capacity();
                InvocationGate::with_system_allocation(|| {
                    core.ready.try_reserve(needed.saturating_sub(core.ready.len()))?;
                    core.notices.try_reserve(needed.saturating_sub(core.notices.len()))
                }).map_err(|_| NO_MEMORY)?;
                Ok(reservation)
            })
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn submit(&self, subject: &subjects::Subject, owner: Owner, work: Work)
                         -> Result<mac_object_table::store::Publication, SubmissionError>
    {
        let submit = || {
            self.access(|core| {
                    if let Err(status) = Self::prepare_records(core)
                    {
                        return Err(SubmissionError { status,
                                                     work });
                    }
                    core.submit(subject, owner, work)
                })
        };
        if let Some(activity) = subject.recovery_activity()
        {
            if subject.uses_protected_publication()
            {
                activity.with_storage(submit)
            }
            else
            {
                crate::context::without_recovery(submit)
            }
        }
        else
        {
            submit()
        }
    }

    #[cfg(test)]
    pub(crate) fn submit(&self, owner: Owner, work: Work) -> Result<u64, SubmissionError>
    {
        self.access(|core| {
                let subject = match tests::subject(core, owner)
                {
                    Ok(subject) => subject,
                    Err(status) =>
                    {
                        return Err(SubmissionError { status,
                                                     work });
                    }
                };
                core.submit(&subject, owner, work).map(mac_object_table::store::Publication::id)
            })
    }

    pub(crate) fn release(&self, ticket: mac_object_table::domain::ReleasedResource, storage: bool)
                          -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        self.access(|core| core.release(ticket, storage))
    }

    pub(crate) fn take_completion(&self, owner: Owner,
                                  receipt: mac_object_table::store::CompletionKey)
                                  -> Result<Option<Completed>, Status>
    {
        self.access(|core| core.take(owner, records::Access::Completion(receipt)))
    }

    pub(crate) fn completed_waiter_completion(&self, owner: Owner,
                                              receipt: mac_object_table::store::CompletionKey)
                                              -> Result<Option<u64>, Status>
    {
        self.access(|core| core.completed_waiter(owner, records::Access::Completion(receipt)))
    }

    pub(crate) fn cancel_completion(&self, owner: Owner,
                                    receipt: mac_object_table::store::CompletionKey)
                                    -> Result<(), Status>
    {
        self.access(|core| core.cancel(owner, records::Access::Completion(receipt)))
    }

    pub(crate) fn discard_completion(&self, owner: Owner,
                                     receipt: mac_object_table::store::CompletionKey)
                                     -> Result<(), Status>
    {
        self.access(|core| core.discard(owner, records::Access::Completion(receipt)))
    }

    pub(crate) fn dispatch_completion(&self, owner: Owner,
                                      receipt: mac_object_table::store::CompletionKey)
                                      -> Result<Option<Dispatch<'_>>, Status>
    {
        self.access(|core| core.dispatch_for(owner, records::Access::Completion(receipt)))
            .map(|node| {
                node.map(|node| {
                        Dispatch { queue:       self,
                                   node:        Some(node),
                                   in_callback: false }
                    })
            })
    }

    pub fn pending(&self, owner: Owner, id: u64) -> Result<bool, Status>
    {
        self.access(|core| core.pending(owner, id))
    }

    pub fn attach_waiter(&self, owner: Owner, id: u64, thread: u64) -> Result<(), Status>
    {
        self.access(|core| core.attach_waiter(owner, id, thread))
    }

    pub fn pending_waiter(&self, thread: u64, id: u64) -> Result<bool, Status>
    {
        self.access(|core| core.pending_waiter(thread, id))
    }

    pub fn detach_waiter(&self, thread: u64, id: u64) -> Result<(), Status>
    {
        self.access(|core| core.detach_waiter(thread, id))
    }

    pub fn completed_waiter(&self, owner: Owner, id: u64) -> Result<Option<u64>, Status>
    {
        self.access(|core| core.completed_waiter(owner, id.into()))
    }

    pub fn next_dispatch(&self) -> Option<Dispatch<'_>>
    {
        if self.signal.load(Ordering::Acquire) & READY == 0
        {
            return None;
        }
        self.access(|core| core.next_dispatch()).map(|node| {
                                                    Dispatch { queue:       self,
                                                               node:        Some(node),
                                                               in_callback: false }
                                                })
    }

    pub fn dispatch_for(&self, owner: Owner, id: u64) -> Result<Option<Dispatch<'_>>, Status>
    {
        self.access(|core| core.dispatch_for(owner, id.into())).map(|node| {
                                                                   node.map(|node| {
                                                                       Dispatch { queue:       self,
                                                                                  node:
                                                                                      Some(node),
                                                                                  in_callback:
                                                                                      false }
                                                                   })
                                                               })
    }

    pub fn finish<'q>(&self, mut dispatch: Dispatch<'q>)
                      -> Result<Option<CompletionNotice>, Dispatch<'q>>
    {
        if !core::ptr::eq(self, dispatch.queue) || dispatch.in_callback
        {
            return Err(dispatch);
        }
        let Some(node) = dispatch.node.take()
        else
        {
            return Err(dispatch);
        };
        match self.access(|core| core.finish(node))
        {
            Ok(notice) => Ok(notice),
            Err(node) =>
            {
                dispatch.node = Some(node);
                Err(dispatch)
            }
        }
    }

    pub(super) fn return_owned(&self, node: Box<Node>, interrupted: bool)
    {
        self.access(|core| core.return_owned(node, interrupted));
    }

    pub fn take_notice(&self) -> Option<CompletionNotice>
    {
        if self.signal.load(Ordering::Acquire) & NOTICE == 0
        {
            return None;
        }
        self.access(Core::notice)
    }

    pub fn take_return_failure(&self) -> Option<ReturnFailure>
    {
        if self.signal.load(Ordering::Acquire) & FAILURE == 0
        {
            return None;
        }
        self.access(Core::failure)
    }

    pub fn take(&self, owner: Owner, id: u64) -> Result<Option<Completed>, Status>
    {
        self.access(|core| core.take(owner, id.into()))
    }

    pub fn cancel(&self, owner: Owner, id: u64) -> Result<(), Status>
    {
        self.access(|core| core.cancel(owner, id.into()))
    }

    pub fn discard(&self, owner: Owner, id: u64) -> Result<(), Status>
    {
        self.access(|core| core.discard(owner, id.into()))
    }

    pub fn discard_owner(&self, owner: Owner)
    {
        self.access(|core| core.discard_owner(owner));
    }

    #[cfg(test)]
    pub(super) fn test_access<R>(&self, operation: impl FnOnce(&mut Core) -> R) -> R
    {
        self.access(operation)
    }

    #[cfg(test)]
    pub(super) fn owned_bytes(&self) -> usize
    {
        self.access(|core| core.owned_bytes)
    }
}
