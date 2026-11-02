//! Owned application admission across the launching thread's real I/O replay.
//! No caller pointer or manager guard survives provider execution or parking.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{processes::ProcessCall, *};
use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use super::LaunchAdmission;
use crate::{context::ThreadId,
            drivers::{OwnedJob, jobs::subjects::Subject},
            toolbox::{application_heap, handles::InvocationGate, native_threads}};

pub(super) struct Prepared
{
    pub admission:       LaunchAdmission,
    pub heap:            application_heap::RootBinding,
    pub tls:             native_threads::RootBinding,
    pub loaded:          Option<super::super::files::application::Loaded>,
    pub ready:           Option<OwnedJob>,
    pub directory:       Option<crate::toolbox::files::local::directory::LaunchDirectory>,
    pub directory_bound: bool
}
impl Drop for Prepared
{
    fn drop(&mut self)
    {
        // The queued Subject keeps the genuine runtime alive through actual
        // cancellation ACK after this caller abandons result delivery.
        drop(self.ready.take());
        drop(self.directory.take());
        retire(&mut self.admission);
    }
}

pub(super) fn retire(admission: &mut LaunchAdmission)
{
    if !admission.active
    {
        return;
    }
    admission.stop();
    match crate::kernel::with_desktop(|desktop| desktop.retire_unstarted(admission.id()))
    {
        Ok(Ok(())) => (),
        Ok(Err(status)) =>
        {
            serial!("PROCESS_LAUNCH_RETIRE_RETAINED app={} cleanup={status}", admission.id())
        }
        Err(error) =>
        {
            serial!("PROCESS_LAUNCH_RETIRE_RETAINED app={} desktop={error:?}", admission.id())
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Origin
{
    Process,
    Document(u64)
}

struct Slot
{
    origin:    Origin,
    thread:    ThreadId,
    volume:    u32,
    cnid:      u32,
    prepared:  Option<Prepared>,
    cancelled: bool,
    committed: bool
}
static FLIGHTS: Spinlock<Vec<Slot>> = Spinlock::new(Vec::new());

pub(crate) fn abandon_thread(thread: ThreadId)
{
    abandon(thread, None);
}

pub(super) fn abandon_document(thread: ThreadId, handle: u64)
{
    abandon(thread, Some(Origin::Document(handle)));
}

fn abandon(thread: ThreadId, origin: Option<Origin>)
{
    let removed = InvocationGate::run(|| {
        let mut flights = FLIGHTS.lock();
        let at =
            flights.iter()
                   .position(|slot| {
                       slot.thread == thread && origin.is_none_or(|origin| slot.origin == origin)
                   })?;
        if flights[at].prepared.is_some()
        {
            Some(flights.swap_remove(at))
        }
        else
        {
            if !flights[at].committed
            {
                flights[at].cancelled = true;
            }
            None
        }
    });
    drop(removed);
}

pub(super) fn pending() -> bool
{
    let thread = crate::context::current_thread();
    InvocationGate::run(|| FLIGHTS.lock().iter().any(|slot| slot.thread == thread))
}

struct Turn
{
    thread: ThreadId,
    active: bool
}
impl Drop for Turn
{
    fn drop(&mut self)
    {
        if self.active
        {
            let removed = InvocationGate::run(|| {
                let mut flights = FLIGHTS.lock();
                flights.iter()
                       .position(|slot| slot.thread == self.thread)
                       .map(|at| flights.swap_remove(at))
            });
            drop(removed);
        }
    }
}

fn accepting(thread: ThreadId) -> bool
{
    crate::context::current_thread() == thread
    && crate::context::current_owner().is_ok_and(|owner| owner.accepting())
}

fn claim(call: &ProcessCall, resume: bool, origin: Origin)
         -> Result<(Turn, Option<Prepared>), Status>
{
    let thread = crate::context::current_thread();
    let mut storage = Vec::new();
    let previous = loop
    {
        let step = InvocationGate::run(|| {
            let mut flights = FLIGHTS.lock();
            if !accepting(thread)
            {
                return Err(OPERATION_CANCELLED);
            }
            if let Some(slot) = flights.iter_mut().find(|slot| slot.thread == thread)
            {
                if !resume
                   || slot.origin != origin
                   || slot.cancelled
                   || slot.volume != call.volume
                   || slot.cnid != call.cnid
                {
                    return Err(BAD_ABI);
                }
                return slot.prepared.take().map(|prepared| Ok(Some(prepared))).ok_or(WRONG_THREAD);
            }
            if resume
            {
                return Err(NOT_FOUND);
            }
            if flights.len() == flights.capacity()
            {
                let needed = flights.len().checked_add(1).ok_or(NO_MEMORY)?;
                if storage.capacity() < needed
                {
                    return Ok(Err(needed));
                }
                // Capacity was prepared outside the manager/IRQ gate. Moving the
                // entries and swapping buffers cannot allocate or drop any owner.
                storage.extend(flights.drain(..));
                core::mem::swap(&mut *flights, &mut storage);
            }
            flights.push(Slot { origin,
                                thread,
                                volume: call.volume,
                                cnid: call.cnid,
                                prepared: None,
                                cancelled: false,
                                committed: false });
            Ok(Ok(None))
        })?;
        match step
        {
            Ok(previous) => break previous,
            Err(needed) =>
            {
                // Allocation/recovery and retired buffer disposal run with no
                // flight lock or invocation IRQ mask held. Revalidate on retry.
                storage = Vec::new();
                InvocationGate::with_system_allocation(|| storage.try_reserve(needed))
                    .map_err(|_| NO_MEMORY)?;
            }
        }
    };
    Ok((Turn { thread,
               active: true },
        previous))
}

fn cancelled(turn: &Turn) -> bool
{
    !accepting(turn.thread)
    || InvocationGate::run(|| {
        FLIGHTS.lock()
               .iter()
               .find(|slot| slot.thread == turn.thread)
               .is_none_or(|slot| slot.cancelled)
    })
}

// Accept publication exactly once under the same slot gate as abandonment.
// Caller stop before acceptance cancels; stop after acceptance does not retract
// an accepted launch. A later loading/publication failure still retires its owner.
fn commit(turn: &Turn) -> Result<(), Status>
{
    InvocationGate::run(|| {
        let mut flights = FLIGHTS.lock();
        let slot = flights.iter_mut()
                          .find(|slot| slot.thread == turn.thread)
                          .ok_or(OPERATION_CANCELLED)?;
        if slot.cancelled || !accepting(turn.thread)
        {
            return Err(OPERATION_CANCELLED);
        }
        slot.committed = true;
        Ok(())
    })
}

pub(crate) fn ready_job(tls: &native_threads::RootBinding,
                        owner: &crate::toolbox::handles::OwnerRef, provider: u64)
                        -> Result<OwnedJob, Status>
{
    let runtime = native_threads::runtime_admission(tls, owner).map_err(|cause| cause.status())?;
    let subject = Subject::for_runtime(&runtime)?;
    crate::drivers::submit_background_to(&subject,
                                         provider,
                                         mac_entropy_interface::CLASS,
                                         mac_entropy_interface::VERSION,
                                         mac_entropy_interface::READY,
                                         Vec::new(),
                                         0)
}

pub(super) fn start(call: &mut ProcessCall, loaded: super::super::files::application::Loaded,
                    origin: Origin)
                    -> Result<(), Status>
{
    drive(call, Some(loaded), origin)
}

pub(super) fn resume(call: &mut ProcessCall, origin: Origin) -> Result<(), Status>
{
    drive(call, None, origin)
}

fn drive(call: &mut ProcessCall, loaded: Option<super::super::files::application::Loaded>,
         origin: Origin)
         -> Result<(), Status>
{
    let (mut turn, previous) = match claim(call, loaded.is_none(), origin)
    {
        Ok(value) => value,
        Err(status) =>
        {
            abandon_thread(crate::context::current_thread());
            return Err(status);
        }
    };
    let mut prepared = match previous
    {
        Some(value) => value,
        None =>
        {
            match super::prepare_launch(call, loaded.ok_or(BAD_ABI)?)?
            {
                Some(value) => value,
                None => return Ok(())
            }
        }
    };
    if cancelled(&turn)
    {
        return Err(OPERATION_CANCELLED);
    }
    if let Some(job) = prepared.ready.as_mut()
    {
        let completed = match job.run_once()
        {
            Ok(value) => value,
            Err(status) => return refusal(call, status)
        };
        if let Some(completed) = completed
        {
            match completed.result
            {
                Ok(bytes) if bytes.is_empty() => drop(prepared.ready.take()),
                Ok(_) => return refusal(call, BAD_ABI),
                Err(crate::drivers::jobs::Failure::Cancelled) =>
                {
                    return refusal(call, OPERATION_CANCELLED);
                }
                Err(crate::drivers::jobs::Failure::Service(status)) => return refusal(call, status)
            }
        }
        else
        {
            if let Err(status) = defer_prepared(&mut turn, prepared)
            {
                return refusal(call, status);
            }
            return Err(SERVICE_BUSY);
        }
    }
    if cancelled(&turn)
    {
        return Err(OPERATION_CANCELLED);
    }
    if !prepared.directory_bound
    {
        if prepared.directory.is_none()
        {
            let original = prepared.loaded.as_ref().ok_or(BAD_ABI)?.catalog;
            let directory = crate::toolbox::files::local::directory::LaunchDirectory::prepare(
                prepared.admission.owner(), prepared.admission.allocation(), original)?;
            prepared.directory = Some(directory);
        }
        let completed = crate::toolbox::files::local::directory::LaunchDirectory::advance(
            &mut prepared.directory)?;
        if !completed
        {
            defer_prepared(&mut turn, prepared)?;
            return Err(SERVICE_BUSY);
        }
        prepared.directory_bound = true;
    }
    commit(&turn)?;
    super::finish_launch(call, &mut prepared)
}

fn refusal(call: &mut ProcessCall, status: Status) -> Result<(), Status>
{
    call.requirement_class = mac_entropy_interface::CLASS.0;
    call.requirement_version = mac_entropy_interface::VERSION;
    serial!("PROCESS_READY_REFUSED class=ENTR version=1 status={status}");
    Err(status)
}

fn defer_prepared(turn: &mut Turn, prepared: Prepared) -> Result<(), Status>
{
    if !crate::machine::traps::can_defer_io()
    {
        return Err(WRONG_THREAD);
    }
    let mut pending = Some(prepared);
    InvocationGate::run(|| {
        let mut flights = FLIGHTS.lock();
        let slot = flights.iter_mut()
                          .find(|slot| slot.thread == turn.thread)
                          .ok_or(OPERATION_CANCELLED)?;
        if slot.cancelled || !accepting(turn.thread)
        {
            return Err(OPERATION_CANCELLED);
        }
        slot.prepared = pending.take();
        let value = slot.prepared.as_ref().ok_or(BAD_ABI)?;
        if let Some(job) = value.ready.as_ref()
        {
            job.defer_current()
        }
        else
        {
            value.directory.as_ref().ok_or(BAD_ABI)?.defer_current()
        }
    })?;
    turn.active = false;
    Ok(())
}
