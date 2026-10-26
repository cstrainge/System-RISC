//! Bounded recovery after a module entry returns. No borrowed caller buffer or
//! registry lock survives a callback, and a consumed reply is never polled twice.
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use super::*;
use crate::{drivers::registry::requests::{Origin,
                                          cancellation::{Acknowledged, Plan}},
            toolbox::handles::InvocationGate};

static WORK: AtomicBool = AtomicBool::new(false);
static CHANGED: AtomicBool = AtomicBool::new(false);
static MORE: AtomicBool = AtomicBool::new(false);
static CURSOR: AtomicU64 = AtomicU64::new(0);
static TURN: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);
static RETRY: AtomicU8 = AtomicU8::new(0);
static HAS_FLIGHT: AtomicBool = AtomicBool::new(false);
static FLIGHT: Spinlock<Option<Flight>> = Spinlock::new(None);
static ADMISSION_REFUSAL: Spinlock<Option<Error>> = Spinlock::new(None);

pub(crate) fn changed()
{
    CHANGED.store(true, Ordering::Release);
    WORK.store(true, Ordering::Release);
}
#[derive(Clone, Copy)]
enum Outcome
{
    Pending,
    Deferred(Status),
    Terminal
    {
        status: Status,
        length: usize
    },
    Uncertain(Status)
}
struct Flight
{
    ticket:  u64,
    plan:    Option<Plan>,
    outcome: Option<Outcome>,
    refusal: Option<Error>
}
struct Running;
impl Drop for Running
{
    fn drop(&mut self)
    {
        RUNNING.store(false, Ordering::Release);
    }
}
fn retain(flight: Flight)
{
    InvocationGate::run(|| {
        *FLIGHT.lock() = Some(flight);
        HAS_FLIGHT.store(true, Ordering::Release);
    });
}
fn take() -> Option<Flight>
{
    InvocationGate::run(|| {
        let flight = FLIGHT.lock().take();
        HAS_FLIGHT.store(false, Ordering::Release);
        flight
    })
}
fn reap() -> bool
{
    if CHANGED.swap(false, Ordering::AcqRel)
    {
        CURSOR.store(0, Ordering::Release);
        MORE.store(true, Ordering::Release);
    }
    if !MORE.load(Ordering::Acquire)
    {
        return false;
    }
    match RUNTIME.with(|runtime| {
                     let Some(module) =
                         runtime.registry.rollback_after(CURSOR.load(Ordering::Acquire))
                     else
                     {
                         MORE.store(false, Ordering::Release);
                         return false;
                     };
                     CURSOR.store(module, Ordering::Release);
                     match runtime.registry.rollback(module)
                     {
                         Ok(()) => serial!("MODULE_ROLLBACK_COMPLETE module={module}"),
                         Err(cause) =>
                         {
                             if let Some(name) = runtime.registry.recovery_refusal(module, cause)
                             {
                                 serial!("MODULE_ROLLBACK_RETAINED name={name} module={module} \
                                          reason={cause}");
                             }
                         }
                     }
                     true
                 })
    {
        Ok(attempted) => attempted,
        Err(_) =>
        {
            RETRY.store(8, Ordering::Release);
            true
        }
    }
}
fn invoke(flight: &mut Flight)
{
    let Some(plan) = flight.plan.as_mut()
    else
    {
        return;
    };
    let request = plan.request();
    let subject = plan.subject().clone();
    let mut call = abi::Call { header:          abi::Header::of::<abi::Call>(),
                               ticket:          plan.ticket(),
                               phase:           abi::CANCEL,
                               class:           request.class.0,
                               operation:       request.operation,
                               input:           core::ptr::null(),
                               input_len:       0,
                               output:          plan.output.as_mut_ptr(),
                               output_capacity: plan.output.len() as u32,
                               output_len:      0,
                               reserved:        0 };
    let mut started = false;
    // SAFETY: this linear plan owns its reserved output and exact publication
    // receipt throughout the synchronous callback. No pointer is retained.
    let result = unsafe {
        invoke_scoped(Origin::Direct,
                      &subject,
                      request.caller,
                      request.interface,
                      &mut call,
                      CompletionPolicy::Recovery,
                      &mut started)
    };
    flight.outcome = Some(match result
                          {
                              Ok(abi::PENDING) => Outcome::Pending,
                              Ok(status) =>
                              {
                                  Outcome::Terminal { status,
                                                      length: call.output_len as usize }
                              }
                              Err(status)
                                  if !started && matches!(status, SERVICE_BUSY | WRONG_THREAD) =>
                              {
                                  Outcome::Deferred(status)
                              }
                              Err(status) => Outcome::Uncertain(status)
                          });
}
/// One callback or rollback checkpoint per maintenance sample. Empty work is
/// atomic-only; returned replies and plans survive temporary registry refusal.
pub(crate) fn pump()
{
    if !WORK.load(Ordering::Acquire)
       && !CHANGED.load(Ordering::Acquire)
       && !MORE.load(Ordering::Acquire)
       && !HAS_FLIGHT.load(Ordering::Acquire)
    {
        return;
    }
    if RUNNING.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err()
    {
        return;
    }
    let _running = Running;
    if RETRY.try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1)).is_ok()
    {
        return;
    }
    if TURN.fetch_xor(true, Ordering::AcqRel) && reap()
    {
        return;
    }
    let mut flight = match take()
    {
        Some(flight) => flight,
        None =>
        {
            let next = RUNTIME.with(|runtime| {
                                  let next = runtime.registry.next_cancellation();
                                  if matches!(next, Ok(None))
                                  {
                                      WORK.store(false, Ordering::Release);
                                  }
                                  next
                              });
            match next
            {
                Ok(Ok(Some(plan))) =>
                {
                    Flight { ticket:  plan.ticket(),
                             plan:    Some(plan),
                             outcome: None,
                             refusal: None }
                }
                Ok(Ok(None)) =>
                {
                    reap();
                    return;
                }
                Ok(Err(cause)) =>
                {
                    let changed = InvocationGate::run(|| {
                        let mut old = ADMISSION_REFUSAL.lock();
                        if *old == Some(cause)
                        {
                            return false;
                        }
                        *old = Some(cause);
                        true
                    });
                    if changed
                    {
                        serial!("MODULE_CANCEL_ADMISSION_RETAINED reason={cause}");
                    }
                    RETRY.store(8, Ordering::Release);
                    return;
                }
                Err(_) =>
                {
                    RETRY.store(8, Ordering::Release);
                    return;
                }
            }
        }
    };
    if flight.outcome.is_none()
    {
        invoke(&mut flight);
    }
    let Some(outcome) = flight.outcome
    else
    {
        serial!("MODULE_CANCEL_PLAN_MISSING ticket={}; original record remains retained",
                flight.ticket);
        return;
    };
    if matches!(outcome, Outcome::Pending) && flight.plan.as_ref().is_some_and(Plan::note_waiting)
    {
        serial!("MODULE_CANCEL_WAITING ticket={}; awaiting actual provider acknowledgement",
                flight.ticket);
    }
    // Take the plan only after entering Registry. A rejected TryState closure
    // must not drop the owned plan captured by that closure.
    let returned = RUNTIME.with(|runtime| {
                              flight.plan.take().map(|plan| {
                                                    match outcome {
            Outcome::Pending | Outcome::Deferred(_) =>
                runtime.registry.return_cancellation(plan).map(|()| None),
            Outcome::Terminal { status, length } =>
                runtime.registry.acknowledge_cancellation(plan, status, length).map(Some),
            Outcome::Uncertain(status) =>
                runtime.registry.retain_uncertain_cancellation(plan, status).map(|()| None)
        }
                                                })
                          });
    match returned
    {
        Ok(Some(Ok(acknowledged))) =>
        {
            match outcome
            {
                Outcome::Pending =>
                {
                    WORK.store(true, Ordering::Release);
                    RETRY.store(8, Ordering::Release);
                }
                Outcome::Deferred(status) =>
                {
                    let _ = status;
                    WORK.store(true, Ordering::Release);
                    RETRY.store(8, Ordering::Release);
                }
                Outcome::Terminal { .. } =>
                {
                    if let Some(Acknowledged::ReplyRetained { bytes,
                                                              status }) = acknowledged
                    {
                        serial!("MODULE_CANCEL_REPLY_RETAINED ticket={} bytes={bytes} \
                                 status={status}; committed results require class recovery; \
                                 provider remains pinned",
                                flight.ticket);
                    }
                    else if let Outcome::Terminal { status,
                                                      .. } = outcome
                    {
                        serial!("MODULE_CANCEL_ACK ticket={} status={status}; original request \
                                 released",
                                flight.ticket);
                    }
                    changed();
                }
                Outcome::Uncertain(status) =>
                {
                    serial!("MODULE_CANCEL_UNCERTAIN ticket={} status={status}; original reply \
                             storage and provider remain pinned",
                            flight.ticket);
                    changed();
                }
            }
        }
        Ok(Some(Err(refusal))) =>
        {
            if flight.refusal != Some(refusal.cause)
            {
                serial!("MODULE_CANCEL_RETURN_RETAINED {refusal}");
                flight.refusal = Some(refusal.cause);
            }
            flight.plan = Some(refusal.plan);
            RETRY.store(8, Ordering::Release);
            retain(flight);
        }
        Err(_) =>
        {
            RETRY.store(8, Ordering::Release);
            retain(flight);
        }
        Ok(None) =>
        {
            serial!("MODULE_CANCEL_PLAN_MISSING ticket={}; original record remains retained",
                    flight.ticket)
        }
    }
}
