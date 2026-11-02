//! Owned staging and park/replay for native entropy. Only existing canonical
//! ThreadIds identify flights; no new object IDs or provider registry exist.
#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::fmt::Write;

use mac_abi::{entropy as wire, *};
use mac_entropy_interface as provider;
use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use crate::{context::ThreadId,
            drivers::{OwnedJob,
                      jobs::{Failure as JobFailure, subjects::Subject}},
            toolbox::{handles::InvocationGate, native_threads::RuntimeAdmission}};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key
{
    operation: u32,
    capacity:  usize
}
enum Step
{
    Request,
    Diagnostic(Status)
}
struct Flight
{
    provider: u64,
    job:      OwnedJob,
    step:     Step
}
// A slot remains present while its job is temporarily local to a callback.
// Failure/retirement can then mark cancellation instead of missing that flight.
struct Slot
{
    thread:    ThreadId,
    key:       Key,
    flight:    Option<Flight>,
    cancelled: bool
}
static FLIGHTS: Spinlock<Vec<Slot>> = Spinlock::new(Vec::new());

pub(crate) fn abandon_thread(thread: ThreadId)
{
    let removed = InvocationGate::run(|| {
        let mut slots = FLIGHTS.lock();
        let at = slots.iter().position(|slot| slot.thread == thread)?;
        if slots[at].flight.is_some()
        {
            Some(slots.swap_remove(at))
        }
        else
        {
            slots[at].cancelled = true;
            None
        }
    });
    // OwnedJob abandons result delivery and retains actual cancellation in the
    // established broker. Its disposal never occurs under the flight guard.
    drop(removed);
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
        if !self.active
        {
            return;
        }
        let removed = InvocationGate::run(|| {
            let mut slots = FLIGHTS.lock();
            slots.iter().position(|s| s.thread == self.thread).map(|at| slots.swap_remove(at))
        });
        drop(removed);
    }
}
impl Turn
{
    fn claim(thread: ThreadId, key: Key, admission: &RuntimeAdmission)
             -> Result<(Self, Option<Flight>), Status>
    {
        let mut storage = Vec::new();
        loop
        {
            let step = InvocationGate::run(|| {
                let mut slots = FLIGHTS.lock();
                // close() precedes the scheduler's abandon_thread walk. Checking
                // the genuine gate under this same slot lock covers publication
                // both before and after that walk, including in-flight callbacks.
                if !admission.accepting()
                {
                    return Err(OPERATION_CANCELLED);
                }
                if let Some(at) = slots.iter().position(|s| s.thread == thread)
                {
                    let slot = &mut slots[at];
                    if slot.cancelled || slot.key != key
                    {
                        return Err(BAD_ABI);
                    }
                    let flight = slot.flight.take().ok_or(WRONG_THREAD)?;
                    return Ok(Ok((Self { thread,
                                         active: true },
                                  Some(flight))));
                }
                if slots.len() == slots.capacity()
                {
                    let needed = slots.len().checked_add(1).ok_or(NO_MEMORY)?;
                    if storage.capacity() < needed
                    {
                        return Ok(Err(needed));
                    }
                    // No allocation or owner disposal inside the manager/IRQ gate.
                    storage.extend(slots.drain(..));
                    core::mem::swap(&mut *slots, &mut storage);
                }
                slots.push(Slot { thread,
                                  key,
                                  flight: None,
                                  cancelled: false });
                Ok(Ok((Self { thread,
                              active: true },
                       None)))
            })?;
            match step
            {
                Ok(claimed) => return Ok(claimed),
                Err(needed) =>
                {
                    storage = Vec::new();
                    InvocationGate::with_system_allocation(|| storage.try_reserve(needed))
                        .map_err(|_| NO_MEMORY)?;
                }
            }
        }
    }

    fn cancelled(&self, admission: &RuntimeAdmission) -> bool
    {
        InvocationGate::run(|| {
            !admission.accepting()
            || FLIGHTS.lock().iter().find(|s| s.thread == self.thread).is_none_or(|s| s.cancelled)
        })
    }

    fn wait(mut self, flight: Flight, admission: &RuntimeAdmission) -> Result<(), Status>
    {
        let mut pending = Some(flight);
        let result = InvocationGate::run(|| {
            let mut slots = FLIGHTS.lock();
            let slot =
                slots.iter_mut().find(|s| s.thread == self.thread).ok_or(OPERATION_CANCELLED)?;
            if slot.cancelled || !admission.accepting()
            {
                return Err(OPERATION_CANCELLED);
            }
            slot.flight = pending.take();
            // Internal bounded attachment only: no allocation, foreign callback,
            // I/O or wait occurs while this slot lock is held. No broker lock
            // survives its call to abandon_thread, so lock ordering is one-way.
            let job = &slot.flight.as_ref().ok_or(BAD_ABI)?.job;
            job.defer_current()
        });
        match result
        {
            Ok(()) =>
            {
                self.active = false;
                Ok(())
            }
            Err(status) => Err(status)
        }
        // On refusal Turn/OwnedJob drop outside both locks and cancel retained work.
    }
}

fn queue(subject: &Subject, selected: u64, operation: u32, capacity: usize)
         -> Result<OwnedJob, Status>
{
    crate::drivers::submit_background_to(subject,
                                         selected,
                                         provider::CLASS,
                                         provider::VERSION,
                                         operation,
                                         Vec::new(),
                                         capacity)
}

pub(crate) fn refusal(call: &mut wire::Call, status: Status, detail: core::fmt::Arguments<'_>)
                      -> Status
{
    struct Text<'a>
    {
        bytes: &'a mut [u8],
        used:  usize
    }
    impl core::fmt::Write for Text<'_>
    {
        fn write_str(&mut self, value: &str) -> core::fmt::Result
        {
            let mut count = value.len().min(self.bytes.len() - self.used);
            while !value.is_char_boundary(count)
            {
                count -= 1;
            }
            self.bytes[self.used..self.used + count].copy_from_slice(&value.as_bytes()[..count]);
            self.used += count;
            Ok(())
        }
    }
    call.written = 0;
    let mut text = Text { bytes: &mut call.detail,
                          used:  0 };
    let _ = text.write_fmt(detail);
    call.detail_length = text.used as u32;
    status
}

pub(crate) fn request(call: &mut wire::Call, output: &mut [u8]) -> Status
{
    call.written = 0;
    call.detail_length = 0;
    let thread = crate::context::current_thread();
    if call.flags != 0
       || call.reserved != [0; 2]
       || call.reserved2 != 0
       || output.len() != call.capacity as usize
       || output.len() > wire::MAX_CAPACITY
       || !matches!(call.operation, wire::READY | wire::FILL)
       || (call.operation == wire::READY && !output.is_empty())
       || (call.operation == wire::FILL && output.is_empty())
    {
        abandon_thread(thread);
        return refusal(call,
                       BAD_ARGUMENT,
                       format_args!("invalid entropy operation, flags, reserved fields or \
                                     buffer capacity"));
    }
    if !crate::machine::traps::can_defer_io()
    {
        return refusal(call,
                       WRONG_THREAD,
                       format_args!("entropy requires an ordinary application service boundary \
                                     that can park"));
    }
    // The borrowed executor ends before invoking any provider. The returned
    // genuine Process admission/root authority stays owned by this operation.
    let admitted = crate::context::with_application_executor(|executor| {
                       let admission = executor.with_tls(|slots| slots.runtime_admission())
                                               .map_err(|cause| cause.status())?
                                               .map_err(|cause| cause.status())?;
                       Ok::<_, Status>(admission)
                   }).map_err(|cause| cause.status())
                     .and_then(core::convert::identity);
    let admission = match admitted
    {
        Ok(value) => value,
        Err(status) =>
        {
            abandon_thread(thread);
            return refusal(call,
                           status,
                           format_args!("entropy Process/thread admission refused (status \
                                         {status}); no output was copied"));
        }
    };
    let key = Key { operation: call.operation,
                    capacity:  output.len() };
    let (turn, previous) = match Turn::claim(thread, key, &admission)
    {
        Ok(value) => value,
        Err(status) =>
        {
            abandon_thread(thread);
            return refusal(call,
                           status,
                           format_args!("entropy continuation changed or its original thread \
                                         is closing (status {status})"));
        }
    };
    let mut flight = match previous
    {
        Some(value) => value,
        None =>
        {
            let activity = match admission.activity()
            {
                Ok(value) => value,
                Err(cause) =>
                {
                    return refusal(call,
                                   cause.status(),
                                   format_args!("entropy runtime lifetime admission refused: \
                                                 {cause}"));
                }
            };
            let scope = match admission.service_scope()
            {
                Ok(value) => value,
                Err(cause) =>
                {
                    return refusal(call,
                                   cause.status(),
                                   format_args!("entropy runtime service scope refused: {cause}"));
                }
            };
            let crate::allocation::Target::Owner { account,
                                                   .. } = activity.allocation_target()
            else
            {
                return refusal(call,
                               BAD_ABI,
                               format_args!("entropy runtime has no original Process \
                                             allocation authority"));
            };
            let selected = match crate::drivers::admit_process_provider(activity.owner(),
                                                                        account,
                                                                        &scope,
                                                                        provider::CLASS,
                                                                        provider::VERSION)
            {
                Ok(value) => value,
                Err(cause) =>
                {
                    return refusal(call,
                                   cause.status(),
                                   format_args!("ENTR version 1 provider admission refused: \
                                                 {cause}"));
                }
            };
            let operation =
                if key.operation == wire::READY { provider::READY } else { provider::FILL };
            let subject = match Subject::for_runtime(&admission)
            {
                Ok(subject) => subject,
                Err(status) =>
                {
                    return refusal(call,
                                   status,
                                   format_args!("entropy operation ownership/account admission \
                                                 refused (status {status})"));
                }
            };
            match queue(&subject, selected, operation, output.len().min(provider::MAX_TRANSFER))
            {
                Ok(job) =>
                {
                    Flight { provider: selected,
                             job,
                             step: Step::Request }
                }
                Err(status) =>
                {
                    return refusal(call,
                                   status,
                                   format_args!("entropy request admission/storage refused \
                                                 (status {status}); no provider result was \
                                                 copied"));
                }
            }
        }
    };
    // At most the original callback and one error-detail callback can run here.
    // Hardware progress always returns Pending to the existing scheduler wait.
    for _ in 0..2
    {
        if turn.cancelled(&admission)
        {
            return refusal(call,
                           OPERATION_CANCELLED,
                           format_args!("entropy request cancelled by the original \
                                         Process/thread; broker cancellation retains pending \
                                         hardware state"));
        }
        let completion = match flight.job.run_once()
        {
            Ok(Some(result)) => result,
            Ok(None) =>
            {
                return match turn.wait(flight, &admission)
                {
                    Ok(()) => SERVICE_BUSY,
                    Err(status) =>
                    {
                        refusal(call,
                                status,
                                format_args!("entropy wait admission refused (status {status}); \
                                              original work handed to broker cancellation"))
                    }
                };
            }
            Err(status) =>
            {
                return refusal(call,
                               status,
                               format_args!("entropy result delivery refused (status \
                                             {status}); original work retains cancellation \
                                             ownership"));
            }
        };
        if turn.cancelled(&admission)
        {
            return refusal(call,
                           OPERATION_CANCELLED,
                           format_args!("entropy completed after Process cancellation; result \
                                         discarded"));
        }
        match flight.step
        {
            Step::Diagnostic(original) =>
            {
                return match completion.result
                {
                    Ok(bytes) =>
                    {
                        match core::str::from_utf8(&bytes)
                        {
                            Ok(text) if !text.is_empty() =>
                            {
                                refusal(call,
                                        original,
                                        format_args!("entropy provider status {original}; \
                                                      diagnostic snapshot: {text}"))
                            }
                            _ =>
                            {
                                refusal(call,
                                        original,
                                        format_args!("entropy provider refused status \
                                                      {original}; its diagnostic was malformed"))
                            }
                        }
                    }
                    Err(_) =>
                    {
                        refusal(call,
                                original,
                                format_args!("entropy provider refused status {original}; its \
                                              diagnostic is unavailable"))
                    }
                };
            }
            Step::Request =>
            {
                match completion.result
                {
                    Ok(bytes) =>
                    {
                        let valid = if key.operation == wire::READY
                        {
                            bytes.is_empty()
                        }
                        else
                        {
                            !bytes.is_empty()
                            && bytes.len() <= output.len().min(provider::MAX_TRANSFER)
                        };
                        if !valid
                        {
                            return refusal(call,
                                           BAD_ABI,
                                           format_args!("entropy provider returned an invalid \
                                                         actual byte count"));
                        }
                        output[..bytes.len()].copy_from_slice(&bytes);
                        call.written = bytes.len() as u32;
                        return OK;
                    }
                    Err(JobFailure::Cancelled) =>
                    {
                        return refusal(call,
                                       OPERATION_CANCELLED,
                                       format_args!("entropy broker acknowledged cancellation \
                                                     without delivering source bytes"));
                    }
                    Err(JobFailure::Service(status)) =>
                    {
                        let original_subject = flight.job.subject().clone();
                        match queue(&original_subject,
                                    flight.provider,
                                    provider::ERROR_TEXT,
                                    provider::MAX_ERROR_TEXT)
                        {
                            Ok(job) =>
                            {
                                flight.job = job;
                                flight.step = Step::Diagnostic(status);
                            }
                            Err(_) =>
                            {
                                return refusal(call,
                                               status,
                                               format_args!("entropy provider refused status \
                                                             {status}; no retained diagnostic \
                                                             could be admitted"));
                            }
                        }
                    }
                }
            }
        }
    }
    match turn.wait(flight, &admission)
    {
        Ok(()) => SERVICE_BUSY,
        Err(status) =>
        {
            refusal(call,
                    status,
                    format_args!("entropy diagnostic wait refused (status {status}); original \
                                  request retained for cancellation"))
        }
    }
}
