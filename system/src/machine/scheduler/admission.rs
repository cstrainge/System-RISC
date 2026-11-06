//! Detached native thread admission shared by actual Process, Module and System publishers.
//! Capacity and canonical publication preparation are allocated outside SCHEDULER;
//! obsolete backing is likewise returned before disposal.
use core::fmt;

use mac_object_table::{domain::PoolError,
                       store::{PreparedError, PublicationPermit}};

use super::*;
use crate::toolbox::handles::InvocationGate;

#[derive(Debug)]
pub(crate) enum Error
{
    Invalid(&'static str),
    Metadata(&'static str),
    Pool(PoolError),
    Publication(PreparedError),
    Admission
    {
        status: mac_abi::Status,
        stage:  &'static str
    }
}
impl Error
{
    pub(super) fn status(&self) -> mac_abi::Status
    {
        match self
        {
            Self::Invalid(_) => mac_abi::BAD_ARGUMENT,
            Self::Metadata(_) => mac_abi::NO_MEMORY,
            Self::Pool(cause) | Self::Publication(PreparedError::Pool(cause)) => crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(*cause)),
            Self::Publication(_) => mac_abi::BAD_ABI,
            Self::Admission { status, .. } => *status
        }
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Invalid(detail) => write!(out, "native thread admission: {detail}"),
            Self::Metadata(stage) =>
            {
                write!(out, "native thread {stage} capacity allocation refused")
            }
            Self::Pool(cause) => cause.fmt(out),
            Self::Publication(cause) => cause.fmt(out),
            Self::Admission { status,
                              stage } =>
            {
                write!(out,
                       "native thread {stage} refused with status {status}; original ownership \
                        retained")
            }
        }
    }
}
impl From<PoolError> for Error
{
    fn from(cause: PoolError) -> Self
    {
        Self::Pool(cause)
    }
}
pub(super) struct Buffers
{
    threads: Vec<Option<CompletionKey>>,
    waiters: Vec<EventWaiter>
}
impl Buffers
{
    pub(super) fn prepare() -> Result<Self, Error>
    {
        let (thread_capacity, waiter_capacity) = crate::machine::interrupts::with_masked(|| {
            let scheduler = SCHEDULER.lock();
            let needs_slot = !scheduler.threads.iter().any(Option::is_none);
            let required = scheduler.threads
                                    .len()
                                    .checked_add(usize::from(needs_slot))
                                    .ok_or(Error::Invalid("thread directory length overflows"))?;
            Ok::<_, Error>((if scheduler.threads.capacity() < required { required } else { 0 },
                            if scheduler.waiters.capacity() < required { required } else { 0 }))
        })?;
        crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
            let mut threads = Vec::new();
            if thread_capacity != 0
            {
                threads.try_reserve_exact(thread_capacity)
                       .map_err(|_| Error::Metadata("shared run queue"))?;
            }
            let mut waiters = Vec::new();
            if waiter_capacity != 0
            {
                waiters.try_reserve_exact(waiter_capacity)
                       .map_err(|_| Error::Metadata("shared event snapshot"))?;
            }
            Ok(Self { threads,
                      waiters })
        })
    }

    pub(super) fn install(&mut self, scheduler: &mut Scheduler) -> Result<usize, Error>
    {
        let slot =
            scheduler.threads.iter().position(Option::is_none).unwrap_or(scheduler.threads.len());
        let required = scheduler.threads
                                .len()
                                .checked_add(usize::from(slot == scheduler.threads.len()))
                                .ok_or(Error::Invalid("thread directory length overflows"))?;
        if scheduler.threads.capacity() < required && self.threads.capacity() < required
           || scheduler.waiters.capacity() < required && self.waiters.capacity() < required
        {
            return Err(Error::Publication(PreparedError::CapacityChanged));
        }
        if scheduler.threads.capacity() < required
        {
            self.threads.extend(scheduler.threads.drain(..));
            core::mem::swap(&mut scheduler.threads, &mut self.threads);
        }
        if scheduler.waiters.capacity() < required
        {
            self.waiters.extend(scheduler.waiters.drain(..));
            core::mem::swap(&mut scheduler.waiters, &mut self.waiters);
        }
        Ok(slot)
    }
}
pub(super) fn thread_permit(owner: &OwnerRef) -> Result<PublicationPermit<InvocationGate>, Error>
{
    loop
    {
        let plan = crate::machine::interrupts::with_masked(|| {
            let scheduler = SCHEDULER.lock();
            scheduler.records
                     .as_ref()
                     .ok_or(Error::Admission { status: mac_abi::BAD_ABI,
                                               stage:  "canonical THRD Store" })?
                     .plan_publication(owner)
                     .map_err(Error::Pool)
        })?;
        let preparation = plan.prepare()?;
        let result = crate::machine::interrupts::with_masked(|| {
            let mut scheduler = SCHEDULER.lock();
            match scheduler.records.as_mut()
            {
                Some(store) => Ok(store.admit_prepared(preparation)),
                None => Err(preparation)
            }
        });
        let result = match result
        {
            Ok(result) => result,
            Err(preparation) =>
            {
                drop(preparation);
                return Err(Error::Admission { status: mac_abi::BAD_ABI,
                                              stage:  "canonical THRD Store" });
            }
        };
        match result
        {
            Ok((permit, old)) =>
            {
                old.dispose();
                return Ok(permit);
            }
            Err(failure) =>
            {
                let cause = failure.cause;
                drop(failure.preparation);
                if cause != PreparedError::CapacityChanged
                {
                    return Err(Error::Publication(cause));
                }
            }
        }
    }
}
pub(super) struct Published
{
    pub(super) id:       u64,
    pub(super) assigned: Result<(), mac_abi::Status>
}
/// Origin follows the prepared thread, attribution its authentic original
/// account. A regular child/main cannot accidentally borrow protected storage.
pub(super) fn publish(prepared: &PreparedThread, runnable: bool) -> Result<Published, Error>
{
    let target = crate::allocator::Target::Owner { account: prepared.allocation.clone(),
                                                   class:
                                                       mac_kernel_model::policy::MemoryClass::Kernel };
    let allocate =
        || crate::allocator::with_target(target, || publish_selected(prepared, runnable));
    match &prepared.recovery
    {
        Some(permit) => crate::context::with_recovery_allocation(permit.clone(), allocate),
        None => crate::context::without_recovery(allocate)
    }
}
fn publish_selected(prepared: &PreparedThread, runnable: bool) -> Result<Published, Error>
{
    let mut permit = thread_permit(&prepared.owner)?;
    let id = permit.id();
    let mut record = Record::Scheduled(Thread { id,
                                                core: prepared.core.share(),
                                                state: Cell::new(RunState::new()),
                                                worker: None,
                                                scope: None });
    loop
    {
        let mut buffers = Buffers::prepare()?;
        let result = crate::machine::interrupts::with_masked(|| {
            let mut scheduler = SCHEDULER.lock();
            match buffers.install(&mut scheduler)
            {
                Err(cause) => Err((cause, permit, record)),
                Ok(slot) =>
                {
                    let publication =
                        match scheduler.records.as_mut()
                        {
                            Some(store) => store.publish_prepared(permit, record)
                                                .map_err(|failure| {
                                                    (Error::Publication(failure.cause),
                                                     failure.permit,
                                                     failure.value)
                                                }),
                            None =>
                            {
                                Err((Error::Admission { status: mac_abi::BAD_ABI,
                                                        stage:  "canonical THRD Store" },
                                     permit,
                                     record))
                            }
                        };
                    match publication
                    {
                        Err(failure) => Err(failure),
                        Ok((publication, old)) =>
                        {
                            let (_, receipt) = publication.into_parts();
                            if slot == scheduler.threads.len()
                            {
                                scheduler.threads.push(Some(receipt));
                            }
                            else
                            {
                                scheduler.threads[slot] = Some(receipt);
                            }
                            let assigned =
                                prepared.core
                                        .backing
                                        .lock()
                                        .as_ref()
                                        .map_or(Err(mac_abi::BAD_ABI), |backing| {
                                            backing.stack.bind(receipt)
                                        })
                                        .and_then(|()| {
                                            prepared.core.context.assign(prepared.owner.clone(),
                                                                         id,
                                                                         prepared.core.binding,
                                                                         &prepared.allocation,
                                                                         &prepared.heap)
                                        });
                            if assigned.is_ok()
                               && let Some(thread) = scheduler.thread(slot)
                            {
                                thread.update(|state| state.live = runnable);
                            }
                            Ok((assigned, old))
                        }
                    }
                }
            }
        });
        // Obsolete buffers/permits/readers never drop under SCHEDULER.
        drop(buffers);
        match result
        {
            Ok((assigned, old)) =>
            {
                old.dispose();
                return Ok(Published { id,
                                      assigned });
            }
            Err((cause, returned_permit, returned_record)) =>
            {
                permit = returned_permit;
                record = returned_record;
                if !matches!(cause, Error::Publication(PreparedError::CapacityChanged))
                {
                    return Err(cause);
                }
            }
        }
    }
}
