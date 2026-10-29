//! Temporary primary service obligations on the one canonical THRD record.
//! Base affinity never changes. Request pointers remain scalar until migration
//! finishes; only an actual completed attempt removes its ordinary obligation.
#![deny(unsafe_code)]
#[allow(unsafe_code)]
pub(crate) mod host;

use alloc::sync::Arc;

use mac_abi::{ACCESS_DENIED, CORRUPT_DATA, OPERATION_CANCELLED, SERVICE_BUSY, Status, WRONG_THREAD};

use super::{HartKey, IDLE, SCHEDULER, Thread, ThreadCore, TrapFrame, current, selected_hart};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Call
{
    pc:       usize,
    number:   usize,
    argument: usize,
    bytes:    usize
}
impl Call
{
    fn new(frame: &TrapFrame) -> Self
    {
        Self { pc:       frame.pc,
               number:   frame.registers[15],
               argument: frame.registers[8],
               bytes:    frame.registers[9] }
    }
}
#[derive(Clone, Copy)]
pub(super) struct State
{
    ecall:          Option<Call>,
    host_pending:   Option<Call>,
    host_depth:     u32,
    retire_pending: bool,
    fault:          Option<Status>
}
impl State
{
    pub(super) const fn new() -> Self
    {
        Self { ecall:          None,
               host_pending:   None,
               host_depth:     0,
               retire_pending: false,
               fault:          None }
    }

    pub(super) fn primary(self) -> bool
    {
        self.ecall.is_some()
        || self.host_pending.is_some()
        || self.host_depth != 0
        || self.fault.is_some()
    }

    pub(super) fn retire_committed(&mut self)
    {
        self.retire_pending = false;
    }

    pub(super) fn failure(self) -> Option<Status>
    {
        self.fault
    }

    pub(super) fn quiescent(self) -> bool
    {
        self.host_depth == 0 && self.fault.is_none()
    }
}
pub(crate) enum Route
{
    Here,
    Migrate
}
pub(crate) enum Entry
{
    Here(Option<Completion>),
    Migrate
}
/// A borrowed attempt may retain this actual core until final status/PC commit.
/// Dropping it during a deferred replay releases a reader, not the obligation.
pub(crate) struct Completion
{
    id:     u64,
    core:   Arc<ThreadCore>,
    call:   Call,
    _local: core::marker::PhantomData<*mut ()>
}
enum Binding
{
    // Genuine primary cold/idle/I/O executors cannot migrate. No synthetic THRD.
    Affine,
    Scheduled
    {
        slot: usize,
        id:   u64,
        core: Arc<ThreadCore>
    }
}
/// Non-Clone. Only current canonical primary acquisition constructs this pin.
pub(crate) struct ServiceLease
{
    binding: Option<Binding>,
    _local:  core::marker::PhantomData<*mut ()>
}
fn quarantine(core: &Arc<ThreadCore>, thread: Option<&Thread>, cause: Status) -> Option<Status>
{
    let first = core.service_quarantine.get().is_none();
    core.service_quarantine.call_once(|| (core.clone(), cause));
    let cause = core.service_quarantine.get().map_or(cause, |(_, cause)| *cause);
    core.cleanup_refusal.lock().replace(super::ReapState::NativeService(cause));
    if let Some(thread) = thread
    {
        let prior = thread.state.get();
        let was_retryable = prior.retired && prior.service.fault.is_none();
        if prior.service.fault.is_none()
        {
            // Move this exact worker's already-returned receipt out of the
            // retryable reaper count before making its fault permanent.
            super::worker::service_quarantined(thread);
        }
        thread.update(|state| {
                  state.service.fault = Some(cause);
                  state.service.retire_pending = true;
                  state.live = false;
                  state.retired = true;
              });
        if was_retryable
        {
            // Permanent retention is not retryable reap work. Keep its real
            // row/Core/ticket, but do not make every unrelated context release
            // wake a reaper that can never acknowledge this quarantined owner.
            if super::RETIRED_COUNT.try_update(core::sync::atomic::Ordering::AcqRel,
                                               core::sync::atomic::Ordering::Acquire,
                                               |count| count.checked_sub(1))
                                   .is_err()
            {
                // No wrap or cleanup panic on inconsistent accounting. The
                // actual permanent corruption record remains retained.
                core.cleanup_refusal.lock().replace(super::ReapState::NativeService(CORRUPT_DATA));
            }
        }
    }
    first.then_some(cause)
}

// Called only after SCHEDULER has been released; the retained native ownership
// keeps the bounded diagnostic alive without a callback under the claim lock.
fn report_quarantine(id: u64, cause: Option<Status>)
{
    if let Some(cause) = cause
       && fixed_primary()
    {
        serial!("NATIVE_SERVICE_QUARANTINED thread={id} status={cause}; original core and walk \
                 ticket retained until reset");
    }
}

fn fixed_primary() -> bool
{
    matches!((crate::machine::traps::execution::hart_id(),
              crate::machine::paging::firmware_owner_hart()),
             (Some(actual), Some(primary)) if actual == primary)
}

fn movable(frame: &TrapFrame) -> bool
{
    frame.prior_depth == 0
    && frame.status & (1 << 5) != 0
    && !super::in_io()
    && !super::critical_active()
}
fn matching_frame(thread: &Thread, hart: HartKey, frame: &TrapFrame) -> bool
{
    thread.state.get().claimed_by == Some(hart)
    && thread.core.cpu_frame() == Some(core::ptr::from_ref(frame).cast_mut())
}

pub(crate) fn enter_ecall(frame: &TrapFrame) -> Result<Entry, Status>
{
    let slot = current(); // Authenticates current cold/hot selection, never a copied app ID.
    if slot == IDLE
    {
        return if fixed_primary() { Ok(Entry::Here(None)) } else { Err(WRONG_THREAD) };
    }
    let hart = selected_hart();
    // Nested primary calls belong to the outer attempt and must not replace it.
    if frame.prior_depth != 0
    {
        return if hart.is_primary() { Ok(Entry::Here(None)) } else { Err(WRONG_THREAD) };
    }
    if !hart.is_primary() && !movable(frame)
    {
        return Err(WRONG_THREAD);
    }
    let scheduler = SCHEDULER.lock();
    let thread = scheduler.thread(slot).ok_or(WRONG_THREAD)?;
    if !matching_frame(thread, hart, frame)
    {
        return Err(WRONG_THREAD);
    }
    let state = thread.state.get();
    if thread.core.context.is_module_worker() && frame.registers[15] as u32 != mac_abi::traps::LOG
    {
        // Primary maintenance may synchronously invoke a validated resident
        // definition on this very worker continuation. This only admits the
        // routing stage; dispatch still validates the exact borrowed Surface
        // and service allowlist before any service effects. Direct workers and
        // peer calls receive no exception and cannot migrate a drawing borrow.
        if !hart.is_primary() || thread.core.context.definitions.current().is_none()
        {
            return Err(WRONG_THREAD);
        }
    }
    if let Some(cause) = state.service.fault
    {
        return Err(cause);
    }
    if state.service.retire_pending
       && matches!(frame.registers[15] as u32,
                   mac_abi::traps::WAIT_EVENT | mac_abi::traps::POSITIONED_EVENT)
    {
        return Err(OPERATION_CANCELLED);
    }
    if let Some(original) = state.service.ecall
    {
        if original != Call::new(frame)
        {
            return Err(ACCESS_DENIED);
        }
        if !hart.is_primary()
        {
            return Ok(Entry::Migrate);
        }
        return Ok(Entry::Here(Some(Completion { id:     thread.id,
                                                core:   thread.core.clone(),
                                                call:   original,
                                                _local: core::marker::PhantomData })));
    }
    if hart.is_primary()
    {
        return Ok(Entry::Here(None));
    }
    if !state.live || state.retired
    {
        return Err(OPERATION_CANCELLED);
    }
    thread.update(|state| state.service.ecall = Some(Call::new(frame)));
    Ok(Entry::Migrate)
}

/// Called after writing the final result and advancing PC, never on deferred replay.
pub(crate) fn complete(completion: Option<Completion>) -> Result<(), Status>
{
    let Some(completion) = completion
    else
    {
        return Ok(());
    };
    let scheduler = SCHEDULER.lock();
    let Some(thread) = scheduler.thread(current())
    else
    {
        let report = quarantine(&completion.core, None, CORRUPT_DATA);
        drop(scheduler);
        report_quarantine(completion.id, report);
        return Err(CORRUPT_DATA);
    };
    if !Arc::ptr_eq(&thread.core, &completion.core)
       || thread.id != completion.id
       || thread.state.get().claimed_by != Some(selected_hart())
       || !selected_hart().is_primary()
       || thread.state.get().service.ecall != Some(completion.call)
    {
        let same = (thread.id == completion.id && Arc::ptr_eq(&thread.core, &completion.core)).then_some(thread);
        let report = quarantine(&completion.core, same, CORRUPT_DATA);
        drop(scheduler);
        report_quarantine(completion.id, report);
        return Err(CORRUPT_DATA);
    }
    thread.update(|state| state.service.ecall = None);
    Ok(())
}

pub(super) fn defer_retire(thread: &Thread) -> bool
{
    if !thread.state.get().service.quiescent()
    {
        thread.update(|state| {
                  state.service.retire_pending = true;
                  if state.live && state.service.fault.is_none()
                  {
                      // An already parked Host continuation must resume to release
                      // its lease, even after owner stop removed its event source.
                      state.waiting = None;
                      state.woken = state.woken.max(1);
                  }
              });
        true
    }
    else
    {
        false
    }
}

pub(super) fn suspension_allowed(state: super::RunState) -> Result<(), Status>
{
    if state.service.retire_pending || state.service.fault.is_some()
    {
        Err(OPERATION_CANCELLED)
    }
    else
    {
        Ok(())
    }
}
/// A real EXIT cannot abandon the native guard on this very continuation.
pub(crate) fn exit_allowed() -> Result<(), Status>
{
    let scheduler = SCHEDULER.lock();
    if scheduler.thread(current()).is_some_and(|thread| !thread.state.get().service.quiescent())
    {
        Err(SERVICE_BUSY)
    }
    else
    {
        Ok(())
    }
}

pub(super) fn refuse_host(frame: &mut TrapFrame, status: Status)
{
    let original = Call::new(frame);
    frame.registers[8] = status as isize as usize;
    frame.pc = frame.pc.wrapping_add(4);
    let scheduler = SCHEDULER.lock();
    if let Some(thread) = scheduler.thread(current())
       && thread.state.get().claimed_by == Some(selected_hart())
       && thread.state.get().service.host_pending == Some(original)
    {
        thread.update(|state| state.service.host_pending = None);
    }
}

pub(super) fn host_route(frame: &TrapFrame) -> Result<Route, Status>
{
    let hart = selected_hart();
    if !hart.is_primary() && !movable(frame)
    {
        return Err(WRONG_THREAD);
    }
    let scheduler = SCHEDULER.lock();
    let thread = scheduler.thread(current()).ok_or(WRONG_THREAD)?;
    if !matching_frame(thread, hart, frame)
    {
        return Err(WRONG_THREAD);
    }
    let state = thread.state.get();
    if let Some(cause) = state.service.fault
    {
        return Err(cause);
    }
    if let Some(original) = state.service.host_pending
    {
        if original != Call::new(frame)
        {
            return Err(ACCESS_DENIED);
        }
    }
    else if !hart.is_primary()
    {
        if !state.live || state.retired
        {
            return Err(OPERATION_CANCELLED);
        }
        thread.update(|state| state.service.host_pending = Some(Call::new(frame)));
    }
    Ok(if hart.is_primary() { Route::Here } else { Route::Migrate })
}
fn acquire(thread: &Thread, slot: usize) -> Result<ServiceLease, Status>
{
    let state = thread.state.get();
    if let Some(cause) = state.service.fault
    {
        return Err(cause);
    }
    let depth = state.service.host_depth.checked_add(1).ok_or(SERVICE_BUSY)?;
    let pin = thread.core.clone();
    thread.update(|state| {
              state.service.host_depth = depth;
              state.service.host_pending = None;
          });
    Ok(ServiceLease { binding: Some(Binding::Scheduled { slot,
                                                         id: thread.id,
                                                         core: pin }),
                      _local:  core::marker::PhantomData })
}
pub(super) fn acquire_host(frame: &TrapFrame) -> Result<ServiceLease, Status>
{
    let hart = selected_hart();
    if !hart.is_primary()
    {
        return Err(WRONG_THREAD);
    }
    let scheduler = SCHEDULER.lock();
    let thread = scheduler.thread(current()).ok_or(WRONG_THREAD)?;
    if !matching_frame(thread, hart, frame)
    {
        return Err(WRONG_THREAD);
    }
    if thread.state.get().service.host_pending.is_some_and(|call| call != Call::new(frame))
    {
        return Err(ACCESS_DENIED);
    }
    acquire(thread, current())
}
pub(super) fn try_acquire_host_here() -> Result<Option<ServiceLease>, Status>
{
    crate::machine::interrupts::with_masked(|| {
        let slot = current();
        if slot == IDLE
        {
            // current() authenticated cold/idle/I/O selection before this
            // comparison. Physical metadata is not a caller-selected label.
            return if fixed_primary()
            {
                Ok(Some(ServiceLease { binding: Some(Binding::Affine),
                                       _local:  core::marker::PhantomData }))
            }
            else
            {
                Err(WRONG_THREAD)
            };
        }
        let hart = selected_hart();
        if !hart.is_primary()
        {
            return Ok(None);
        }
        let scheduler = SCHEDULER.lock();
        let thread = scheduler.thread(slot).ok_or(WRONG_THREAD)?;
        if thread.state.get().claimed_by != Some(hart)
        {
            return Err(WRONG_THREAD);
        }
        acquire(thread, slot).map(Some)
    })
}
impl Drop for ServiceLease
{
    fn drop(&mut self)
    {
        let Some(binding) = self.binding.take()
        else
        {
            return;
        };
        crate::machine::interrupts::with_masked(|| {
            let Binding::Scheduled { slot,
                                     id,
                                     core } = binding
            else
            {
                return;
            };
            let mut report = None;
            {
                let scheduler = SCHEDULER.lock();
                match scheduler.thread(slot)
                {
                    Some(thread)
                        if thread.id == id
                           && Arc::ptr_eq(&thread.core, &core)
                           && current() == slot
                           && selected_hart().is_primary()
                           && thread.state.get().claimed_by == Some(selected_hart()) =>
                    {
                        if let Some(depth) = thread.state.get().service.host_depth.checked_sub(1)
                        {
                            thread.update(|state| state.service.host_depth = depth);
                            if depth == 0 && thread.state.get().service.retire_pending
                            {
                                super::retire(thread);
                            }
                        }
                        else
                        {
                            report = quarantine(&core, Some(thread), CORRUPT_DATA);
                        }
                    }
                    other =>
                    {
                        let same = other.filter(|thread| {
                                            thread.id == id && Arc::ptr_eq(&thread.core, &core)
                                        });
                        report = quarantine(&core, same, CORRUPT_DATA);
                    }
                }
            }
            // Retirement may now make this continuation unselectable. Keep
            // local IRQs masked until the real owned Arc is gone, or a timer
            // could abandon this very destructor with its pin on the stack.
            // SCHEDULER is released first. The actual running claim/Store pin,
            // or the prepaid uncertainty quarantine, prevents a last destructor.
            report_quarantine(id, report);
            drop(core);
            // The actual current claim still pins this continuation. Its
            // eventual release, rather than every temporary Host pin, wakes
            // reclamation; normal services do not generate cleanup storms.
        });
    }
}
