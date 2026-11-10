//! The only native trap entry: register/CSR preservation and checked ABI dispatch.

use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{traps::*, *};

use crate::{machine::trap_frame as abi, toolbox};
mod state;
mod definition_services;
pub(crate) mod user;
pub(crate) mod hart;
pub(crate) use hart::{Error as EntryError, OuterCapture, PreparedEntry, PreparedHart,
                      StartingHart, activity as cpus, admit_bootstrap, execution,
                      prepare_secondary};
pub(crate) const MAX_TRAP_DEPTH: usize = 8;
// One bootstrap baseline, eight admitted service levels, and one typed-refusal
// frame. The bound follows the existing native service recursion contract.
pub(crate) const CPU_SLOTS: usize = MAX_TRAP_DEPTH + 2;
pub(crate) use state::SavedState;
static CPU_STATE_BYTES: spin::Once<usize> = spin::Once::new();
unsafe extern "C" {
    pub(crate) fn __system_trap_gateway(number: u32, request: *mut (), size: usize) -> i64;
}
pub(crate) use __system_trap_gateway as gateway;
#[repr(C, align(16))]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct TrapFrame
{
    pub registers:             [usize; 30],
    pub pc:                    usize,
    pub status:                usize,
    pub(crate) cause:          usize,
    pub(crate) value:          usize,
    pub(crate) floating:       [u64; 32],
    pub(crate) fcsr:           usize,
    pub(crate) vl:             usize,
    pub(crate) vtype:          usize,
    pub(crate) vstart:         usize,
    pub(crate) vcsr:           usize,
    pub stack_pointer:         usize,
    pub prior_depth:           usize,
    /// Software thread pointer of the interrupted context. RISC-V holds it in
    /// `tp`, which is `registers[2]` and needs no field of its own; AArch64
    /// holds it in TPIDR_EL0, a system register that the shared 30-slot
    /// register file cannot represent. Only the AArch64 entry writes this.
    pub(crate) thread_pointer: usize,
    /// Return link of the interrupted context. RISC-V holds it in `ra`, which
    /// is `registers[0]`; AArch64 holds it in `x30`, which the 30-slot file
    /// stops short of. Only the AArch64 entry writes this.
    pub(crate) link_register:  usize,
    /// Which of the architecture's exception kinds this frame was entered
    /// through, in the architecture's own encoding.
    ///
    /// AArch64 needs it: an IRQ, FIQ or SError writes no `ESR_EL1`, so the
    /// recorded cause alone cannot say whether the word in it belongs to this
    /// exception. Without this field a stale exception class from the last
    /// `svc` would make an interrupt look like a service call. RISC-V's
    /// `scause` states the kind in the cause word itself, so its entry leaves
    /// this zero and nothing reads it.
    pub(crate) exception:      usize
}
// Integer offsets remain the assembly gate contract. The extended offsets and
// fixed frame size are generated above from this one Rust representation.
const _: () = assert!(core::mem::offset_of!(TrapFrame, pc) == 240
                      && core::mem::offset_of!(TrapFrame, status) == 248);
// The frame status words are the architecture's own; see `trap_frame`.

/// Validate the selected sizes against this CPU before allocating pools or
/// starting services. This is policy admission, not System-stack activation.
pub(crate) fn validate_stack_configuration(
    )
    -> Result<(), (&'static str, mac_kernel_model::stack::Error)>
{
    use mac_kernel_model::{pages::PageGeometry,
                           stack::{self, Policy}};

    use crate::machine::memory::*;
    let vector_bytes = crate::machine::cpu::vector_bytes() as u64;
    let cpu_bytes =
        stack::cpu_state_bytes(size_of::<TrapFrame>() as u64, 32, vector_bytes).map_err(|error| {
                                                                                   ("CPU frame",
                                                                                    error)
                                                                               })?;
    for (name, minimum, selected, maximum) in
        [("System per-hart", SYSTEM_STACK_MIN_BYTES, SYSTEM_STACK_BYTES, SYSTEM_STACK_MAX_BYTES),
         ("application per-thread",
          APPLICATION_STACK_MIN_BYTES,
          APPLICATION_STACK_BYTES,
          APPLICATION_STACK_MAX_BYTES)]
    {
        let geometry =
            PageGeometry::new(PAGE_BYTES).map_err(|_| {
                                             (name,
                                              stack::Error::Policy { minimum,
                                                                     default: selected,
                                                                     maximum,
                                                                     page_bytes: PAGE_BYTES })
                                         })?;
        Policy::new(minimum, selected, maximum, geometry).and_then(|policy| {
                                                             policy.select(None, cpu_bytes)
                                                         })
                                                         .map_err(|error| (name, error))?;
    }
    CPU_STATE_BYTES.call_once(|| cpu_bytes as usize);
    serial!("STACK_POLICY system={} application={} cpu_state={} vector_register_bytes={}",
            SYSTEM_STACK_BYTES, APPLICATION_STACK_BYTES, cpu_bytes, vector_bytes);
    Ok(())
}

/// Complete warm requests in their one existing Desktop turn. Only a validated
/// cold miss ends that borrow before page/monitor work, then re-enters and
/// revalidates. No borrowed manager state or synthetic worker crosses the gap.
enum DefinitionTurn<T>
{
    Complete(T),
    Prepare
    {
        kind: [u8; 4],
        id:   i16
    }
}
fn definition_request<T>(mut run: impl FnMut(&mut crate::toolbox::runtime::Toolbox)
                               -> Result<DefinitionTurn<T>, Status>)
                         -> Result<T, Status>
{
    let first =
        crate::kernel::with_toolbox(|toolbox| run(toolbox)).map_err(|cause| cause.status())??;
    let (kind, id) = match first
    {
        DefinitionTurn::Complete(result) => return Ok(result),
        DefinitionTurn::Prepare { kind,
                                  id } => (kind, id)
    };
    crate::loader::prepare_definition(kind, id).map_err(|cause| {
                                                   serial!("DEFINITION_PREPARATION_REFUSED \
                                                            kind={:?} id={} cause={}",
                                                           kind,
                                                           id,
                                                           cause);
                                                   crate::loader::status(cause)
                                               })?;
    match crate::kernel::with_toolbox(|toolbox| run(toolbox)).map_err(|cause| cause.status())??
    {
        DefinitionTurn::Complete(result) => Ok(result),
        // Source/request admission changed while the same real cold work was
        // outside the manager. Do not spin or commit with a mismatched source.
        DefinitionTurn::Prepare { .. } => Err(SERVICE_BUSY)
    }
}

/// Local CSR installation only. Warm entry uses this while its Control stays
/// poisoned; it must not initialize global tracing or use firmware logging.
///
/// Also called before the kernel root is activated, because until the vector is
/// installed the CPU takes every exception through whatever table the firmware
/// left in place - and that table is in the bootstrap, which a kernel space does
/// not map. A fault in that window therefore loops with no report at all.
pub(crate) fn install_local()
{
    // SAFETY: `__system_trap_vector` is this crate's own trap entry.
    unsafe {
        crate::machine::cpu::install_trap_vector(crate::machine::trap_entry::vector());
    }
}
pub(crate) fn install()
{
    install_local();
    serial!("TRAPS_READY: resident ecall dispatcher ABI={}", ABI_VERSION);
    crate::trace::initialize();
}

#[unsafe(no_mangle)]
extern "C" fn __system_trap_dispatch(frame: *mut TrapFrame) -> *mut TrapFrame
{
    // SAFETY: traps.S saved the complete frame on owned storage and hardware
    // cleared SIE before entering this sole native trap boundary.
    match unsafe {
              crate::machine::paging::with_kernel((*frame).prior_depth == 0, || {
                  // SAFETY: assembly saved the complete live frame. This borrow ends,
                  // including service/context readers, before any ownership handoff.
                  let (action, outer) = {
                      let borrowed = &mut *frame;
                      (dispatch_frame(borrowed), borrowed.prior_depth == 0)
                  };
                  // Every early service/Host/refusal branch reaches this one
                  // guard-free outer return. A safe-parked author must hand off
                  // its genuine claim rather than resume through such a branch.
                  let action = if outer
                                  && matches!(action, Dispatch::Resume)
                                  && crate::machine::scheduler::must_leave_current()
                  {
                      Dispatch::Schedule
                  }
                  else
                  {
                      action
                  };
                  let next = match action
                  {
                      Dispatch::Resume => frame,
                      Dispatch::Schedule =>
                      {
                          let capture = hart::captured_outer(frame);
                          if capture.hart().requires_schedule()
                          {
                              crate::machine::scheduler::schedule(capture)
                          }
                          else
                          {
                              // Warm admitted idle has genuine affine backing,
                              // but no permission to select any author thread.
                              capture.hart().resume_activity(true);
                              capture.frame()
                          }
                      }
                      Dispatch::Wake =>
                      {
                          // The mutable dispatch borrow and every service scope
                          // ended above. Only the authenticated complete capture
                          // can identify the actual row whose wake we acknowledge.
                          let capture = hart::captured_outer(frame);
                          let key = capture.hart();
                          match super::monitor::acknowledge_wake(key)
                          {
                              Ok(true) => crate::machine::scheduler::wake::acknowledged(key),
                              Ok(false) => (), // Empty/already-ACKed SSIP advances nothing.
                              Err(super::monitor::Error::Stopping) =>
                              {
                                  // This retains the pinned capture, posts no
                                  // ACK and cannot resume an author. The monitor
                                  // owns its bounded higher-privilege stop wait
                                  // and independent terminal failure reporting.
                                  super::monitor::wait_for_terminal_stop(key)
                              }
                              Err(error) =>
                              {
                                  // ACK has no allocation/contention refusal. An
                                  // invalid mandatory directory/key/epoch binding
                                  // is structural corruption, not lost work to retry.
                                  crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                                          format_args!("Supervisor wake binding \
                                                                        lost on hart {}: {}.",
                                                                       key.number(),
                                                                       error));
                              }
                          }
                          if key.requires_schedule()
                          {
                              crate::machine::scheduler::schedule(capture)
                          }
                          else
                          {
                              // Startup ACK does not itself authorize policy or
                              // author-body execution on an admitted idle hart.
                              key.resume_activity(true);
                              capture.frame()
                          }
                      }
                  };
                  if outer
                  {
                      // Every service/frame borrow and scheduler guard ended.
                      // A nested ECALL can still interrupt an outer guard, so
                      // only the genuine outer return posts deferred intent.
                      crate::machine::scheduler::wake::flush();
                  }
                  next
              })
          }
    {
        Ok(frame) => frame,
        Err(error) =>
        {
            crate::machine::paging::report_refusal(&error);
            crate::machine::cpu::breakpoint();
        }
    }
}
enum Dispatch
{
    Resume,
    Schedule,
    Wake
}

/// An interrupt this machine delivered, named in terms the shared dispatcher
/// acts on rather than in one architecture's cause encoding.
///
/// The architecture adapters produce these; nothing above them compares a cause
/// word to a bit pattern to decide whether a trap was an interrupt. That
/// comparison was RISC-V's `scause` vocabulary leaking into shared code, and it
/// had no meaning on a machine whose interrupts carry no cause at all.
pub(crate) enum Interrupt
{
    /// The running context's quantum expired: select again.
    Preempt,
    /// Another hart asked this one to look at the scheduler.
    Wake,
    /// A source nothing in the resident path enabled. It is completed and
    /// reported, and it is neither a tick nor a fault.
    Unhandled
    {
        number: u32
    }
}

/// Whether an unhandled interrupt has already been reported. A stray source
/// repeats until it is masked, and a terminal report would turn that into a dead
/// machine while an unbounded log would bury the boot.
static UNHANDLED_INTERRUPT_REPORTED: AtomicBool = AtomicBool::new(false);

fn dispatch_frame(frame: &mut TrapFrame) -> Dispatch
{
    // An interrupt is neither a call nor a fault, and the adapter answers which
    // it is before any cause is decoded: on a machine whose interrupts write no
    // cause register, reading the recorded cause as an exception class is how
    // an interrupt comes to look like a service call.
    match abi::interrupt(frame)
    {
        Some(Interrupt::Wake) => return Dispatch::Wake,
        Some(Interrupt::Preempt) =>
        {
            #[cfg(feature = "event-trace")]
            hart::observe_entry(frame as *mut TrapFrame as usize);
            return Dispatch::Schedule;
        }
        Some(Interrupt::Unhandled { number }) =>
        {
            // The answer every other kernel gives to an interrupt no handler
            // owns: say so once, complete it, and carry on. Nothing was armed
            // for it, so there is no work to lose, and reporting it as a
            // privileged fault would stop a machine that is otherwise healthy.
            if !UNHANDLED_INTERRUPT_REPORTED.swap(true, Ordering::Relaxed)
            {
                serial!("SYSTEM_INTERRUPT_UNHANDLED: number={} pc=0x{:x}; completed and ignored",
                        number, frame.pc);
            }
            return Dispatch::Resume;
        }
        None => ()
    }
    #[cfg(feature = "event-trace")]
    hart::observe_entry(frame as *mut TrapFrame as usize);

    let user_origin = abi::from_user(frame);
    // The cause this architecture reports for a service call. A genuine fault
    // keeps the value the hardware produced, so a fault is never classified as
    // a call and a call is never classified as a fault.
    let cause = abi::cause(frame);
    if user_origin && cause & (1usize << 63) == 0 && cause != 8
    {
        if frame.prior_depth != 0
        {
            entry_lost("hardware user fault arrived inside a privileged nested service");
        }
        match crate::machine::scheduler::capture_fault(frame)
        {
            Ok(()) => return Dispatch::Schedule,
            Err(status) =>
            {
                crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                                 format_args!("User fault lost its genuine \
                                                               running Process claim: status \
                                                               {status}."))
            }
        }
    }
    if !matches!((user_origin, cause), (true, 8) | (false, 9))
    {
        crate::machine::emergency::serial(format_args!("SYSTEM_TRAP: cause={} pc=0x{:x} \
                                                        value=0x{:x}",
                                                       frame.cause, frame.pc, frame.value));
        // No context/definition/registry lock is safe to reacquire after an
        // arbitrary privileged fault. Report trusted CPU fields and terminate;
        // this is a terminal screen/reset, never mid-call recovery (M-D04).
        crate::machine::emergency::enter(crate::emergency::Kind::PrivilegedFault,
                                         format_args!("Privileged trap: cause {}, PC {:#x}, \
                                                       fault address {:#x}. Execution cannot \
                                                       safely continue.",
                                                      frame.cause, frame.pc, frame.value));
    }
    let number = abi::number(frame);
    if user_origin
       && (frame.prior_depth != 0
           || number == mac_module::abi::worker::TRAP
           || number == crate::machine::scheduler::service::host::TRAP)
    {
        abi::set_result(frame, ACCESS_DENIED as usize);
        abi::advance(frame);
        return Dispatch::Resume;
    }
    if execution::policy::deny(execution::policy::Operation::Service)
    {
        // Refuse before a request pointer or an executor-owned trap scope is
        // borrowed. A policy may neither park nor enter ordinary services.
        abi::set_result(frame, ACCESS_DENIED as usize);
        abi::advance(frame);
        return Dispatch::Resume;
    }
    if number == mac_module::abi::worker::TRAP
    {
        return worker_trap(frame);
    }
    if number == crate::machine::scheduler::service::host::TRAP
    {
        // The one internal System gate validates its actual callsite/scalars;
        // no caller output slot is borrowed on the peer before migration.
        return match unsafe { crate::machine::scheduler::service::host::dispatch(frame) }
        {
            crate::machine::scheduler::service::host::Outcome::Resume => Dispatch::Resume,
            crate::machine::scheduler::service::host::Outcome::Migrate => Dispatch::Schedule
        };
    }
    let completion = match crate::machine::scheduler::service::enter_ecall(frame)
    {
        Ok(crate::machine::scheduler::service::Entry::Here(completion)) => completion,
        Ok(crate::machine::scheduler::service::Entry::Migrate) => return Dispatch::Schedule,
        Err(status) =>
        {
            abi::set_result(frame, status as isize as usize);
            abi::advance(frame);
            return Dispatch::Resume;
        }
    };
    let thread = crate::context::current_thread_context();
    let scope = thread.traps.enter();
    let physical_depth = frame.prior_depth + 1;
    let depth = hart::service_depth(physical_depth);
    if scope.depth > physical_depth
    {
        serial!("SYSTEM_TRAP_ACCOUNTING_LOST: logical={} physical={physical_depth}", scope.depth);
        super::power::fatal_shutdown();
    }
    if depth == 2 && !thread.traps.nesting_reported()
    {
        serial!("TRAP_NESTED: definition -> OS service");
    }
    if depth > MAX_TRAP_DEPTH
    {
        if !thread.traps.nesting_refusal_reported()
        {
            serial!("TRAP_NESTING_REFUSED: executor_app={} executor_thread={} depth={depth} \
                     limit={MAX_TRAP_DEPTH}; service not invoked",
                    thread.identity().app.0,
                    thread.identity().thread.0);
        }
        finish_service(frame, completion, SERVICE_BUSY as i64);
        return Dispatch::Resume;
    }
    let number = abi::number(frame);
    let user_pointer = abi::argument(frame);
    let user_size = abi::bytes(frame);
    let identity = crate::machine::scheduler::service::request_identity(frame);
    let mut user_call = if user_origin
    {
        match user::prepare_or_resume(identity, number, user_pointer, user_size)
        {
            Ok(call) => Some(call),
            Err(status) =>
            {
                drop(scope);
                finish_service(frame, completion, i64::from(status));
                return Dispatch::Resume;
            }
        }
    }
    else
    {
        None
    };
    if number == DEFINITION_PANIC
       && let Some(call) = user_call.as_mut()
    {
        let result = call.definition_panic(frame);
        drop(scope);
        // No return/copyout obligation survives a successful nonreturning
        // Process failure capture. Its original inflight Job is retained until
        // the actual callback THRD can no longer run or resume.
        let status = result.as_ref().map_or_else(|status| *status, |()| OK);
        drop(user_call.take());
        finish_service(frame, completion, i64::from(status));
        return if result.is_ok() { Dispatch::Schedule } else { Dispatch::Resume };
    }
    let captured = match user_call.as_mut()
    {
        Some(call) => call.request(),
        None => Ok((user_pointer, user_size))
    };
    let (request, size) = match captured
    {
        Ok(request) => request,
        Err(status) =>
        {
            drop(scope);
            // The original Call owns its continuation and checked buffers.
            // Failed snapshot access never starts dispatch or copies output;
            // dropping it cancels the same owned operation and surface loan.
            drop(user_call.take());
            finish_service(frame, completion, i64::from(status));
            return Dispatch::Resume;
        }
    };
    #[cfg(feature = "event-trace")]
    let started = crate::platform::ticks();
    let admission = if thread.is_module_worker() && number != LOG
    {
        // Scheduler maintenance retains the interrupted worker's identity.
        // Only the validated resident definition's exact borrowed surface and
        // synchronous query services are admitted, never application authority.
        unsafe { definition_services::admit(&thread.definitions, number, request, size) }
    }
    else
    {
        Ok(())
    };
    let (result, action) = if let Err(status) = admission
    {
        (status as i64, crate::machine::scheduler::service::RequestAction::Return)
    }
    else if matches!(number, WAIT_EVENT | POSITIONED_EVENT | READINESS) && physical_depth != 1
    {
        (WRONG_THREAD as i64, crate::machine::scheduler::service::RequestAction::Return)
    }
    else if user_call.is_some() && number == COROUTINE_STACK
    {
        let status = hart::service_scope(physical_depth == 1, || {
            match unsafe { self::request::<mac_abi::coroutine_stack::Call>(request, size) }
            {
                Some(call) =>
                {
                    super::scheduler::coroutine_stack(call, frame.stack_pointer).err().unwrap_or(OK)
                }
                None => BAD_ARGUMENT
            }
        });
        (i64::from(status), super::scheduler::service::RequestAction::Return)
    }
    else if user_call.is_some() && number == USER_CALLBACK
    {
        let suspension = crate::machine::scheduler::service::request_suspension_allowed(frame);
        let response = hart::service_scope(physical_depth == 1, || {
            match user_call.as_mut()
            {
                Some(call) => call.dispatch_callback(suspension),
                None => crate::machine::scheduler::service::RequestResult::done(BAD_ABI)
            }
        });
        (i64::from(response.status), response.action)
    }
    else if let Some(response) =
        user_call.as_mut().and_then(|call| {
                              let suspension =
                                  crate::machine::scheduler::service::request_suspension_allowed(frame);
                              hart::service_scope(physical_depth == 1, || {
                                  call.dispatch_definition(suspension)
                              })
                          })
    {
        (i64::from(response.status), response.action)
    }
    else if matches!(number, NATIVE_THREAD | ADDRESS_WAIT | PIPES)
              || (number == PROCESS
                  && (size == size_of::<mac_abi::processes::NativeExitCall>()
                      || size == size_of::<mac_abi::processes::ChildWaitCall>()))
    {
        let suspension = crate::machine::scheduler::service::request_suspension_allowed(frame);
        let response = hart::service_scope(physical_depth == 1, || unsafe {
            dispatch_native_request(number, request, size, identity, suspension)
        });
        (i64::from(response.status), response.action)
    }
    else
    {
        (hart::service_scope(physical_depth == 1, || unsafe { dispatch(number, request, size) }),
         crate::machine::scheduler::service::RequestAction::Return)
    };
    #[cfg(feature = "event-trace")]
    {
        let elapsed = crate::platform::ticks().wrapping_sub(started);
        crate::trace::service(thread.identity().app.0, number, depth, elapsed);
        if depth == 1 && elapsed > 5_000
        {
            crate::trace::record(crate::trace::SLOW_SERVICE,
                                 thread.identity().app.0,
                                 number as u64,
                                 elapsed,
                                 depth as u64,
                                 0);
        }
    }
    let deferred = if physical_depth == 1 { thread.traps.take_deferred() } else { None };
    drop(scope);
    if let Some(job) = deferred
    {
        if result != SERVICE_BUSY as i64
        {
            // Replaying an already successful mutation would duplicate it.
            // Treat a violated adapter contract as an ABI failure and abandon
            // delivery while cancellation safely drains the owned request.
            match job
            {
                crate::context::DeferredIo::FileOperation(handle) =>
                {
                    crate::toolbox::files::local::pending::abandon(thread.identity().thread,
                                                                   Some(handle))
                }
                crate::context::DeferredIo::FileTransfer(handle) =>
                {
                    crate::toolbox::files::abandon_transfer_waiter(thread.identity().thread.0,
                                                                   handle)
                }
                crate::context::DeferredIo::DriverJob(handle) =>
                {
                    crate::drivers::abandon_deferred_job(thread.identity().thread.0, handle)
                }
            }
            let result = finish_user_call(user_call.take(), BAD_ABI as i64);
            finish_service(frame, completion, result);
            return Dispatch::Resume;
        }
        // All service borrows ended. User requests retain the same owned
        // snapshot and physical pins; replay never rereads mutable app data.
        retain_user_call(user_call.take());
        match crate::machine::scheduler::wait_io(job)
        {
            Ok(true) =>
            {
                abi::rewind(frame);
                return Dispatch::Schedule;
            }
            Ok(false) => return Dispatch::Resume, // completion won the race; replay now
            Err(error) =>
            {
                let call = if user_origin
                {
                    match thread.take_user_call(identity, number, user_pointer.addr(), user_size)
                    {
                        Ok(call) => call,
                        Err(status) =>
                        {
                            crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                                             format_args!("Owned user I/O lost \
                                                                           its original request \
                                                                           binding: status \
                                                                           {status}."))
                        }
                    }
                }
                else
                {
                    None
                };
                let result = finish_user_call(call, i64::from(error));
                finish_service(frame, completion, result);
                return Dispatch::Resume;
            }
        }
    }
    if action == crate::machine::scheduler::service::RequestAction::Replay
    {
        retain_user_call(user_call.take());
        // Every Call/executor/manager borrow has ended. The original PC and
        // request identity remain owned by this same canonical continuation,
        // and the woken thread executes that call instruction again.
        abi::rewind(frame);
        return Dispatch::Schedule;
    }
    let result = if action == crate::machine::scheduler::service::RequestAction::Exit
    {
        // A nonreturning exit has no user output obligation. Release its
        // original request pins after every service/manager borrow has ended.
        drop(user_call.take());
        result
    }
    else
    {
        finish_user_call(user_call.take(), result)
    };
    finish_service(frame, completion, result);
    // Output delivery and the original service obligation have both ended.
    // Replay/deferred branches returned above; Yield/Exit do not return here
    // to the user. Never inspect request memory for a report.
    if user_origin
       && action == crate::machine::scheduler::service::RequestAction::Return
       && abi::result(frame) as isize == SERVICE_BUSY as isize
       && !thread.traps.terminal_busy_reported()
    {
        let identity = thread.identity();
        serial!("NATIVE_SERVICE_BUSY app={} thread={} trap={:#x} status={}", identity.app.0,
                identity.thread.0, number, SERVICE_BUSY);
    }
    if physical_depth == 1
       && matches!(action,
                   crate::machine::scheduler::service::RequestAction::Yield
                   | crate::machine::scheduler::service::RequestAction::Exit)
    {
        return Dispatch::Schedule;
    }
    if matches!(number, WAIT_EVENT | POSITIONED_EVENT | READINESS)
       && result == OK as i64
       && crate::machine::scheduler::blocked()
    {
        return Dispatch::Schedule;
    }
    Dispatch::Resume
}

fn finish_user_call(call: Option<user::Call>, result: i64) -> i64
{
    match call.map(user::Call::finish)
    {
        Some(Err(status)) =>
        {
            serial!("USER_COPYOUT_REFUSED status={status} service_status={result}");
            i64::from(status)
        }
        _ => result
    }
}

fn retain_user_call(call: Option<user::Call>)
{
    if let Some(call) = call
       && let Err((status, _held)) = call.retain()
    {
        // The validated request owns the vacant slot on this same THRD. A
        // lost internal binding cannot free an in-flight buffer: the returned
        // envelope remains live on this terminal, nonunwinding path.
        crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                         format_args!("Owned user request lost its native \
                                                       continuation: status {status}."));
    }
}

/// The service has finished and every manager/request borrow ended. Preserve
/// its real result/PC commit before restoring automatic eligibility. Deferred
/// replay intentionally never calls this helper.
fn finish_service(frame: &mut TrapFrame,
                  completion: Option<crate::machine::scheduler::service::Completion>, result: i64)
{
    abi::set_result(frame, result as usize);
    abi::advance(frame);
    if let Err(status) = crate::machine::scheduler::service::complete(completion)
    {
        abi::set_result(frame, status as isize as usize);
    }
}

/// Private worker suspension uses the actual CPU trap/switch path. A direct C
/// Host callback can never abandon a foreign frame or resident manager guard.
fn worker_trap(frame: &mut TrapFrame) -> Dispatch
{
    use mac_module::abi::{WorkerCall, worker};
    let operation = || -> Result<bool, Status> {
        if frame.prior_depth != 0
           || !crate::machine::trap_frame::interrupts_were_enabled(frame)
           || crate::context::critical_active()
        {
            return Err(WRONG_THREAD);
        }
        let context = crate::context::current_thread_context();
        if !context.is_worker_body()
        {
            return Err(WRONG_THREAD);
        }
        let (id, cancelled) = crate::machine::scheduler::worker_current()?;
        if id != context.identity().thread.0
        {
            return Err(WRONG_THREAD);
        }
        let call = unsafe {
                       request::<WorkerCall>(abi::argument(frame), abi::bytes(frame))
                   }.ok_or(BAD_ARGUMENT)?;
        if !call.header.valid::<WorkerCall>()
           || call.flags != 0
           || (!matches!(call.operation, worker::PARK_FOR) && call.microseconds != 0)
           || !call.specification.is_null()
           || (call.worker != 0 && call.worker != id)
        {
            return Err(BAD_ARGUMENT);
        }
        call.worker = id;
        call.flags = if cancelled { worker::CANCEL_REQUESTED } else { 0 };
        match call.operation
        {
            worker::PARK =>
            {
                call.state = worker::PARKED;
                call.exit_status = if cancelled { OPERATION_CANCELLED } else { OK };
                crate::machine::scheduler::worker_park(None)
            }
            worker::MONOTONIC =>
            {
                call.microseconds = super::monitor::ticks_to_microseconds(crate::platform::ticks())
                    .map_err(|_| BAD_ABI)?;
                Ok(false)
            }
            worker::PARK_FOR =>
            {
                let duration = super::monitor::microseconds_to_ticks_ceil(call.microseconds)
                    .map_err(|cause| if matches!(cause, super::monitor::Error::ClockOverflow)
                             { BAD_ARGUMENT } else { BAD_ABI })?;
                let deadline =
                    crate::platform::ticks().checked_add(duration).ok_or(BAD_ARGUMENT)?;
                call.state = worker::PARKED;
                call.exit_status = if cancelled { OPERATION_CANCELLED } else { OK };
                crate::machine::scheduler::worker_park(Some(deadline))
            }
            worker::EXIT =>
            {
                crate::machine::scheduler::worker_exit()?;
                call.state = worker::FINISHED;
                Ok(true)
            }
            _ => Err(BAD_ARGUMENT)
        }
    };
    let result = operation();
    abi::advance(frame);
    abi::set_result(frame, result.as_ref().map_or_else(|e| *e, |_| OK) as usize);
    if matches!(result, Ok(true)) { Dispatch::Schedule } else { Dispatch::Resume }
}

unsafe fn request<'a, T>(pointer: *mut (), size: usize) -> Option<&'a mut T>
{
    if pointer.is_null()
       || size != core::mem::size_of::<T>()
       || !(pointer as usize).is_multiple_of(core::mem::align_of::<T>())
    {
        None
    }
    else
    {
        Some(unsafe { &mut *pointer.cast::<T>() })
    }
}

/// Bounded, nonallocating per-call detail. Preserve UTF-8 boundaries and the
/// original typed status; diagnostics never classify or replace the cause.
fn application_heap_refusal(call: &mut application_heap::Call, status: Status,
                            cause: &impl core::fmt::Display)
                            -> Status
{
    use core::fmt::Write;
    struct Detail<'a>
    {
        bytes: &'a mut [u8],
        used:  usize
    }
    impl Write for Detail<'_>
    {
        fn write_str(&mut self, text: &str) -> core::fmt::Result
        {
            let mut count = text.len().min(self.bytes.len() - self.used);
            while !text.is_char_boundary(count)
            {
                count -= 1;
            }
            self.bytes[self.used..self.used + count].copy_from_slice(&text.as_bytes()[..count]);
            self.used += count;
            if count == text.len() { Ok(()) } else { Err(core::fmt::Error) }
        }
    }
    call.result = core::ptr::null_mut();
    let mut detail = Detail { bytes: &mut call.detail,
                              used:  0 };
    let _ = write!(&mut detail, "{cause}");
    call.detail_length = detail.used as u32;
    status
}

/// Actual action-bearing native families. Each request borrow ends before the
/// caller commits its saved frame or schedules a different continuation.
unsafe fn dispatch_native_request(number: u32, pointer: *mut (), size: usize,
                                  identity: crate::machine::scheduler::service::RequestIdentity,
                                  suspension: Result<(), Status>)
                                  -> crate::machine::scheduler::service::RequestResult
{
    use crate::machine::scheduler::service::RequestResult;
    match number
    {
        PROCESS if size == size_of::<mac_abi::processes::ChildWaitCall>() =>
        {
            match unsafe { request::<mac_abi::processes::ChildWaitCall>(pointer, size) }
            {
                Some(call) => toolbox::processes::wait_child(call, suspension),
                None => RequestResult::done(BAD_ARGUMENT)
            }
        }
        PROCESS =>
        {
            match unsafe { request::<mac_abi::processes::NativeExitCall>(pointer, size) }
            {
                Some(call) => toolbox::processes::termination::request(call, suspension),
                None => RequestResult::done(BAD_ARGUMENT)
            }
        }
        NATIVE_THREAD =>
        {
            match unsafe { request::<mac_abi::native_threads::Call>(pointer, size) }
            {
                Some(call) =>
                {
                    let mut name = [0u8; mac_module::MAX_NAME_BYTES];
                    toolbox::native_threads::thread_request::request(
                                                                     call,
                                                                     identity,
                                                                     suspension,
                                                                     |pointer, length| {
                                                                         use crate::machine::scheduler::application::Error;
                                                                         if length > name.len() || (length != 0 && pointer.is_null())
                            {
                                return Err(Error::Invalid("thread name pointer/length exceeds bounded inline storage"));
                            }
                                                                         if length != 0
                                                                         {
                                                                             // SAFETY: the native
                                                                             // supervisor ABI
                                                                             // requires readable
                                                                             // input
                                                                             // for this synchronous
                                                                             // call. Copy before
                                                                             // scheduler guards;
                                                                             // scalar validation
                                                                             // and bounded length
                                                                             // precede this access.
                                                                             name[..length].copy_from_slice(unsafe {
                                    core::slice::from_raw_parts(pointer, length)
                                });
                                                                         }
                                                                         core::str::from_utf8(&name[..length])
                                .map_err(|_| Error::Invalid("thread name is not UTF-8"))
                                                                     }
                    )
                }
                None =>
                {
                    crate::machine::scheduler::application::cancel_current();
                    RequestResult::done(BAD_ARGUMENT)
                }
            }
        }
        ADDRESS_WAIT =>
        {
            match unsafe { request::<mac_abi::address_wait::Call>(pointer, size) }
            {
                Some(call) =>
                {
                    crate::machine::scheduler::address::request(call, identity, suspension)
                }
                None =>
                {
                    crate::machine::scheduler::address::cancel_current();
                    crate::machine::scheduler::pipe::cancel_current();
                    RequestResult::done(BAD_ARGUMENT)
                }
            }
        }
        PIPES =>
        {
            match unsafe { request::<mac_abi::pipes::Call>(pointer, size) }
            {
                Some(call) =>
                {
                    // The bridge already validated this span for the operation's
                    // direction. A zero length lends an empty slice, never a null
                    // pointer with a nonzero count.
                    let empty_in: &[u8] = &[];
                    let empty_out: &mut [u8] = &mut [];
                    let (input, output) = if matches!(call.operation,
                                                      mac_abi::pipes::READ
                                                      | mac_abi::pipes::READ_WAIT
                                                      | mac_abi::pipes::TERMINAL_READ_WAIT)
                    {
                        let output: &mut [u8] = if call.length == 0
                        {
                            empty_out
                        }
                        else
                        {
                            unsafe { core::slice::from_raw_parts_mut(call.buffer, call.length) }
                        };
                        (empty_in, output)
                    }
                    else
                    {
                        let input: &[u8] = if call.length == 0
                        {
                            empty_in
                        }
                        else
                        {
                            unsafe { core::slice::from_raw_parts(call.buffer, call.length) }
                        };
                        (input, empty_out)
                    };
                    toolbox::pipes::request(call, input, output, identity, suspension)
                }
                None =>
                {
                    crate::machine::scheduler::pipe::cancel_current();
                    RequestResult::done(BAD_ARGUMENT)
                }
            }
        }
        _ => RequestResult::done(NOT_SUPPORTED)
    }
}

/// Native modules still share trusted supervisor mappings. Size/alignment are checked here;
/// valid live pointees remain the calling module's ABI contract, not an MMU guarantee.
unsafe fn dispatch(number: u32, pointer: *mut (), size: usize) -> i64
{
    if native_string_trap(number).is_some()
    {
        return unsafe {
            crate::toolbox::strings::dispatch(number, pointer, size, |n, p, s| dispatch(n, p, s))
        };
    }
    macro_rules! read_request {
        ($ty:ty) => {
            match unsafe { request::<$ty>(pointer, size) }
            {
                Some(call) => call,
                None => return BAD_ARGUMENT as i64
            }
        };
    }
    match number
    {
        ENTROPY =>
        {
            let call = read_request!(mac_abi::entropy::Call);
            if call.capacity as usize > mac_abi::entropy::MAX_CAPACITY
               || (call.capacity != 0 && call.output.is_null())
            {
                toolbox::entropy::abandon_thread(crate::context::current_thread());
                return toolbox::entropy::refusal(call,
                                                 BAD_ARGUMENT,
                                                 format_args!("entropy output pointer/capacity \
                                                               is invalid"))
                       as i64;
            }
            // Trusted native supervisor ABI: the caller owns this valid mutable
            // span for the synchronous call. No slice survives park/replay.
            let output = if call.capacity == 0
            {
                &mut []
            }
            else
            {
                unsafe { core::slice::from_raw_parts_mut(call.output, call.capacity as usize) }
            };
            toolbox::entropy::request(call, output) as i64
        }
        TLS =>
        {
            let call = read_request!(mac_abi::tls::Call);
            toolbox::native_threads::native::request(call) as i64
        }
        APPLICATION_HEAP =>
        {
            let call = read_request!(application_heap::Call);
            call.result = core::ptr::null_mut();
            call.detail_length = 0;
            // Only a borrowed, actual process baseline authorizes this heap.
            // Service placement and ambient module/I/O selections grant none.
            match crate::context::with_application_executor(|executor| unsafe {
                      toolbox::application_heap::request(executor, call)
                  })
            {
                Ok(Ok(())) => OK as i64,
                Ok(Err(cause)) => application_heap_refusal(call, cause.status(), &cause) as i64,
                Err(cause) => application_heap_refusal(call, cause.status(), &cause) as i64
            }
        }
        POWER =>
        {
            let call = read_request!(mac_abi::power::PowerCall);
            toolbox::power::native(call).map_or_else(|error| error, |_| OK) as i64
        }
        LOG =>
        {
            let call = read_request!(LogCall);
            if call.length > 1024 * 1024
            {
                return BAD_ARGUMENT as i64;
            }
            if call.length != 0
               && (call.bytes.is_null()
                   || core::str::from_utf8(unsafe {
                          core::slice::from_raw_parts(call.bytes, call.length)
                      }).is_err())
            {
                return BAD_ARGUMENT as i64;
            }
            let bytes = if call.length == 0 { b"".as_ptr() } else { call.bytes };
            (unsafe { crate::kernel::log(bytes, call.length) }) as i64
        }
        CRASH =>
        {
            let call = read_request!(mac_abi::crashes::CrashCall);
            if call.length > mac_abi::crashes::MAX_APPEND_BYTES
               || call.capacity > mac_abi::crashes::MAX_REPORT_BYTES
               || (call.length != 0 && call.data.is_null())
               || (call.capacity != 0 && call.output.is_null())
            {
                return BAD_ARGUMENT as i64;
            }
            let bytes = if call.length == 0
            {
                &[][..]
            }
            else
            {
                unsafe { core::slice::from_raw_parts(call.data, call.length) }
            };
            let Ok(input) = core::str::from_utf8(bytes)
            else
            {
                return BAD_ARGUMENT as i64;
            };
            let output = if call.capacity == 0
            {
                &mut [][..]
            }
            else
            {
                unsafe { core::slice::from_raw_parts_mut(call.output, call.capacity) }
            };
            crate::recovery::crash_request(call, input, output) as i64
        }
        CLOCK =>
        {
            let call = read_request!(dates::ClockCall);
            match super::clock::now()
            {
                Ok(now) =>
                {
                    call.unix_seconds = now.unix_seconds();
                    call.microseconds = now.subsec_microseconds();
                    call.reserved = 0;
                    OK as i64
                }
                Err(status) => status as i64
            }
        }
        MONOTONIC_CLOCK =>
        {
            let call = read_request!(events::MonotonicCall);
            call.nanoseconds = match super::monitor::ticks_to_nanoseconds(crate::platform::ticks())
            {
                Ok(value) => value,
                Err(_) => return BAD_ABI as i64
            };
            OK as i64
        }
        TEXTEDIT =>
        {
            crate::kernel::with_toolbox(|runtime| unsafe {
                toolbox::textedit::native::dispatch_with(number, pointer, size, |request| {
                    match request
                    {
                        toolbox::textedit::native::Request::Edit(call, input, output) =>
                        {
                            runtime.textedit_request(call, input, output)
                        }
                        toolbox::textedit::native::Request::Display(call) =>
                        {
                            runtime.textedit_display_request(call)
                        }
                        toolbox::textedit::native::Request::Font(call) =>
                        {
                            runtime.textedit_font_request(call)
                        }
                    }
                })
            }).unwrap_or_else(|error| error.status() as i64)
        }
        STANDARD_FILE =>
        {
            let call = read_request!(mac_abi::standard_file::StandardFileCall);
            crate::recovery::standard_file_request(call) as i64
        }
        DIALOG =>
        {
            let call = read_request!(mac_abi::dialogs::DialogCall);
            crate::recovery::dialog_request(call) as i64
        }
        MENU =>
        {
            let call = read_request!(mac_abi::menus::MenuCall);
            crate::kernel::with_toolbox(|runtime| runtime.menu(call)).unwrap_or_else(|error| {
                                                                         error.status()
                                                                     }) as i64
        }
        CPU_INFO =>
        {
            let call = read_request!(mac_abi::inspection::cpus::Call);
            toolbox::processes::cpu_inspection_request(call) as i64
        }
        INSPECTION =>
        {
            if size < size_of::<u32>()
            {
                return BAD_ARGUMENT as i64;
            }
            let operation = match unsafe { request::<u32>(pointer, size_of::<u32>()) }
            {
                Some(operation) => *operation,
                None => return BAD_ARGUMENT as i64
            };
            if matches!(operation,
                        mac_abi::inspection::memory::BEGIN | mac_abi::inspection::memory::READ)
            {
                let call = read_request!(mac_abi::inspection::memory::Call);
                toolbox::processes::memory_inspection_request(call) as i64
            }
            else
            {
                let call = read_request!(mac_abi::inspection::Call);
                toolbox::processes::inspection_request(call) as i64
            }
        }
        DEFINITION_PANIC =>
        {
            let Some((kind, id)) = crate::context::current_thread_context().definitions.current()
            else
            {
                return ACCESS_DENIED as i64;
            };
            let report = read_request!(mac_abi::definitions::DefinitionPanic);
            let text = report.message
                             .get(..report.length as usize)
                             .and_then(|bytes| core::str::from_utf8(bytes).ok())
                             .unwrap_or("invalid definition panic diagnostic");
            let bytes = kind.to_be_bytes();
            let family = core::str::from_utf8(&bytes).unwrap_or("invalid family");
            let identity = crate::context::current_thread_context().identity();
            crate::machine::emergency::serial(format_args!("DEFINITION_PANIC_FATAL: \
                                                            kind={family} id={id} app={} \
                                                            thread={} flags={} {text}",
                                                           identity.app.0,
                                                           identity.thread.0,
                                                           report.flags));
            if report.flags & mac_abi::definitions::DefinitionPanic::TRUNCATED != 0
            {
                crate::machine::emergency::serial(format_args!("DEFINITION_DIAGNOSTIC_TRUNCATED: \
                                                       retained {} UTF-8 bytes",
                                                      report.length));
            }
            if report.flags & mac_abi::definitions::DefinitionPanic::FORMAT_FAILED != 0
            {
                crate::machine::emergency::serial(format_args!("DEFINITION_DIAGNOSTIC_FORMAT_FAILED: \
                                                       partial panic information retained"));
            }
            #[cfg(feature = "emergency-qualification")]
            crate::kernel::qualify_emergency_with_allocator_locked();
            #[cfg(not(feature = "emergency-qualification"))]
            crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                             format_args!("Definition panic: {family} {id}, app \
                                                           {}, thread {}, flags {}.\n{text}",
                                                          identity.app.0,
                                                          identity.thread.0,
                                                          report.flags))
        }
        THEME =>
        {
            let palette = read_request!(mac_abi::theme::Palette);
            *palette = toolbox::theme::palette();
            OK as i64
        }
        PROCESS if size == size_of::<mac_abi::processes::LaunchArgvCall>() =>
        {
            let call = read_request!(mac_abi::processes::LaunchArgvCall);
            let length = call.length as usize;
            let Some(total) =
                length.checked_add(call.path_length as usize)
                      .filter(|&total| total != 0 && total <= mac_abi::processes::ARGUMENT_BYTES)
            else
            {
                return BAD_ARGUMENT as i64;
            };
            if call.bytes.is_null() || call.reserved != 0
            {
                return BAD_ARGUMENT as i64;
            }
            // The user envelope owns a validated read-only copy for this turn.
            let bytes = unsafe { core::slice::from_raw_parts(call.bytes, total) };
            toolbox::processes::request_with_argv(&mut call.launch, bytes, length, call.count)
            as i64
        }
        PROCESS if size == size_of::<mac_abi::processes::StartupArgvCall>() =>
        {
            let call = read_request!(mac_abi::processes::StartupArgvCall);
            if call.bytes.is_null()
               || call.capacity == 0
               || call.capacity as usize > mac_abi::processes::ARGUMENT_BYTES
            {
                return BAD_ARGUMENT as i64;
            }
            // The output buffer belongs to the checked user envelope.
            let output =
                unsafe { core::slice::from_raw_parts_mut(call.bytes, call.capacity as usize) };
            match toolbox::processes::startup_argv(output, call.path_length)
            {
                Ok((length, count, path_length)) =>
                {
                    call.length = length;
                    call.count = count;
                    call.path_length = path_length;
                    OK as i64
                }
                Err(status) => status as i64
            }
        }
        PROCESS if size == size_of::<mac_abi::processes::DocumentCall>() =>
        {
            let call = read_request!(mac_abi::processes::DocumentCall);
            toolbox::processes::documents::request(crate::context::current_thread_context().identity().app.0,call) as i64
        }
        PROCESS =>
        {
            let call = read_request!(mac_abi::processes::ProcessCall);
            toolbox::processes::request(call) as i64
        }
        FILE =>
        {
            let owner = crate::context::current_thread_context().identity().app.0;
            let watch = if size == size_of::<files::FileCall>()
            {
                let call = read_request!(files::FileCall);
                if matches!(call.operation,
                            files::OPEN
                            | files::OPEN_OPTIONS
                            | files::WRITE
                            | files::READ
                            | files::SEEK
                            | files::TELL
                            | files::SET_LENGTH
                            | files::SYNC
                            | files::CURRENT_DIRECTORY
                            | files::CHANGE_DIRECTORY
                            | files::RESOLVE_PATH
                            | files::OPEN_PATH
                            | files::CURRENT_EXECUTABLE_PATH
                            | files::WATCH_DIRECTORY
                            | files::POLL_WATCH
                            | files::DISPOSE_WATCH)
                {
                    // Capture the actual ledger before firmware I/O selects the
                    // bootstrap stack. Neither identity nor authority is on wire.
                    let authority = match crate::context::current_owner()
                    {
                        Ok(owner) => owner,
                        Err(status) => return status as i64
                    };
                    let kinds = if call.operation == files::WATCH_DIRECTORY
                    {
                        match toolbox::handles::file_watch_kinds()
                        {
                            Ok(kinds) => Some(kinds),
                            Err(status) => return status as i64
                        }
                    }
                    else
                    {
                        None
                    };
                    Some(toolbox::files::Caller { owner: authority,
                                                  kinds })
                }
                else
                {
                    None
                }
            }
            else if size == size_of::<files::DataWriteCall>()
                      && read_request!(files::DataWriteCall).operation == files::BEGIN_DATA_WRITE
                      || size == size_of::<files::ForkWriteCall>()
                         && read_request!(files::ForkWriteCall).operation == files::BEGIN_FORK_WRITE
                      || size == size_of::<files::IconGroupCall>()
                         && read_request!(files::IconGroupCall).operation == files::BEGIN_ICON_GROUP
            {
                let authority = match crate::context::current_owner()
                {
                    Ok(owner) => owner,
                    Err(status) => return status as i64
                };
                Some(toolbox::files::Caller { owner: authority,
                                              kinds: None })
            }
            else
            {
                None
            };
            if let Some(result) = unsafe {
                toolbox::files::native::dispatch_async(owner, watch.as_ref(), pointer, size)
            }
            {
                return result;
            }
            crate::kernel::with_files(|files| unsafe {
                toolbox::files::native::dispatch_owned(files, owner, pointer, size, watch.as_ref())
            }).unwrap_or_else(|error| error.status() as i64)
        }
        EVENTS =>
        {
            let call = read_request!(events::EventCall);
            crate::kernel::with_toolbox(|runtime| runtime.event_service(call))
                .unwrap_or_else(|error| error.status()) as i64
        }
        DRAG =>
        unsafe {
            toolbox::drag::native::dispatch(pointer, size, |call, input, output| {
                toolbox::runtime::native_drag(call, input, output)
            })
        },
        SCRAP =>
        {
            crate::kernel::with_scrap(|manager| unsafe {
                toolbox::scrap::native::dispatch(manager, number, pointer, size)
            }).unwrap_or_else(|error| error.status() as i64)
        }
        MEMORY =>
        {
            let owner = match crate::context::current_owner()
            {
                Ok(owner) => owner,
                Err(status) => return status as i64
            };
            crate::kernel::with_memory(|manager| unsafe {
                toolbox::memory::native::dispatch(manager, &owner, number, pointer, size)
            }).unwrap_or_else(|error| error.status() as i64)
        }
        SCROLL_BOX =>
        unsafe {
            toolbox::controls::scrollbox_native::dispatch(pointer, size, |call| {
                definition_request(|toolbox| {
                    if toolbox.preflight_scroll_box(call)?
                    {
                        return Ok(DefinitionTurn::Prepare { kind: *b"CDEF",
                                                            id:   1 });
                    }
                    toolbox.scroll_box(call).map(DefinitionTurn::Complete)
                })
            })
        },
        CONTROL if size == core::mem::size_of::<mac_abi::controls::PropertiesCall>() =>
        {
            let call = read_request!(mac_abi::controls::PropertiesCall);
            if call.length > isize::MAX as usize
               || call.capacity > isize::MAX as usize
               || (!call.input.is_null() && call.length == usize::MAX)
               || call.length != 0 && call.input.is_null()
               || call.capacity != 0 && call.output.is_null()
            {
                return BAD_ARGUMENT as i64;
            }
            // The production UserRequest has copied/pinned these spans before
            // dispatch. Internal callers lend the same exclusive live buffers.
            let input = if call.length == 0
            {
                &[]
            }
            else
            {
                unsafe { core::slice::from_raw_parts(call.input, call.length) }
            };
            let output = if call.capacity == 0
            {
                &mut []
            }
            else
            {
                unsafe { core::slice::from_raw_parts_mut(call.output, call.capacity) }
            };
            toolbox::runtime::native_properties(call, input, output) as i64
        }
        CONTROL =>
        {
            let call = read_request!(mac_abi::controls::ControlCall);
            if !call.surface.is_null()
            {
                return unsafe {
                    toolbox::controls::native::dispatch(CONTROL,
                                                        pointer,
                                                        size,
                                                        |id| {
                                                            crate::loader::prepare_definition(*b"CDEF", id).map_err(crate::loader::status)
                                                        },
                                                        gateway)
                };
            }
            if matches!(call.operation,
                        mac_abi::controls::CREATE_STANDARD | mac_abi::controls::CREATE_SCROLLBAR)
            {
                return toolbox::runtime::native_control(call) as i64;
            }
            let mut diagnostic = None;
            let status = definition_request(|runtime| {
                let prepared = runtime.preflight_control(call).map_err(|status| {
                    crate::toolbox::runtime::TrackingStage::refusal(&mut diagnostic, crate::toolbox::runtime::TrackingStage::Descriptor, status)
                })?;
                if let Some(id) = prepared
                {
                    return Ok(DefinitionTurn::Prepare { kind: *b"CDEF", id });
                }
                Ok(DefinitionTurn::Complete(runtime.control(call, &mut diagnostic)))
            }).unwrap_or_else(|status| status);
            crate::toolbox::runtime::report_tracking_status(call, diagnostic, status);
            status as i64
        }
        GEOMETRY => toolbox::quickdraw::geometry::dispatch(read_request!(GeometryCall)) as i64,
        SURFACE =>
        unsafe { toolbox::quickdraw::surface_gate(number, pointer, size) },
        QUICKDRAW | FONT | REGION | COPY_BITS | WORLD | CURSOR | MAPPING | RESOURCE | PICTURE
        | ICON | IMAGE =>
        unsafe {
            toolbox::quickdraw::native::dispatch(number,
                                                 pointer,
                                                 size,
                                                 crate::kernel::fonts().as_deref())
        },
        WINDOW =>
        {
            let call = read_request!(WindowCall);
            toolbox::runtime::native_window(call.operation, &mut call.request) as i64
        }
        DRAW =>
        {
            let call = read_request!(DrawCall);
            crate::kernel::with_toolbox(|runtime|runtime.draw(call.window,call.command)).unwrap_or_else(|error|error.status()) as i64
        }
        NEXT_EVENT =>
        {
            let call = read_request!(NextEventCall);
            crate::kernel::with_toolbox(|runtime| {
                call.event = runtime.next(call.mask);
                OK
            }).unwrap_or_else(|error| error.status()) as i64
        }
        TIMERS =>
        {
            toolbox::timers::request(read_request!(mac_abi::timers::Call)).err().unwrap_or(OK)
            as i64
        }
        READINESS =>
        {
            let call = read_request!(mac_abi::readiness::Call);
            // The U envelope captures and validates this entire writable span.
            let sources = if call.count == 0
            {
                &mut []
            }
            else
            {
                unsafe { core::slice::from_raw_parts_mut(call.sources, call.count) }
            };
            crate::machine::scheduler::readiness::request(call, sources).err().unwrap_or(OK) as i64
        }
        POSITIONED_EVENT =>
        {
            let call = read_request!(events::PositionedEventCall);
            crate::kernel::with_toolbox(|runtime| {
                let status = runtime.positioned_event(call);
                if status == OK
                   && call.operation == events::POSITION_WAIT
                   && call.present == 0
                   && call.timeout_ticks != 0
                {
                    call.waited = 1;
                    crate::trace::record(crate::trace::WAIT,
                                         crate::context::current_thread_context().identity().app.0,
                                         call.mask as u64,
                                         call.timeout_ticks as u64,
                                         0,
                                         0);
                    if let Err(status) =
                        crate::machine::scheduler::wait_current(call.mask, runtime.control_layout_wait(call.timeout_ticks))
                    {
                        return status;
                    }
                }
                status
            }).unwrap_or_else(|error| error.status()) as i64
        }
        WAIT_EVENT =>
        {
            let call = read_request!(WaitEventCall);
            crate::kernel::with_toolbox(|runtime| {
                let (event, available) = runtime.next_available(call.mask);
                call.event = event;
                call.waited = u32::from(!available && call.timeout_ticks != 0);
                if call.waited != 0
                {
                    crate::trace::record(crate::trace::WAIT,
                                         crate::context::current_thread_context().identity().app.0,
                                         call.mask as u64,
                                         call.timeout_ticks as u64,
                                         0,
                                         0);
                    if let Err(status) =
                        crate::machine::scheduler::wait_current(call.mask, runtime.control_layout_wait(call.timeout_ticks))
                    {
                        return status;
                    }
                }
                OK
            }).unwrap_or_else(|error| error.status()) as i64
        }
        DEFINITION =>
        {
            let call = read_request!(mac_abi::definitions::UserCall);
            // Only the S-origin path reaches this dispatcher with a direct
            // DTO. U calls use the envelope's captured definition binding and
            // checked family copies or their own user callback queue.
            unsafe { toolbox::resources::definitions::callbacks::dispatch_supervisor(call) }
        }
        CLASSIC =>
        {
            let call = read_request!(ClassicCall);
            if call.memory.is_null()
               || call.memory_length > 128 * 1024 * 1024
               || u64::from(call.guest_base) + call.memory_length as u64 > u64::from(u32::MAX) + 1
            {
                return BAD_ARGUMENT as i64;
            }
            let memory =
                unsafe { core::slice::from_raw_parts_mut(call.memory, call.memory_length) };
            crate::kernel::with_toolbox(|runtime| {
                toolbox::classic::dispatch(call.instruction, &mut call.registers, call.guest_base,
                                           memory, runtime)
            }).unwrap_or_else(|error| {
                  if error != crate::state::AccessError::Unavailable
                  {
                      return error.status();
                  }
                  toolbox::classic::dispatch(call.instruction,
                                             &mut call.registers,
                                             call.guest_base,
                                             memory,
                                             &mut toolbox::classic::NoServices)
              }) as i64
        }
        _ => NOT_SUPPORTED as i64
    }
}

pub(crate) fn prepare_entry(stack: &'static crate::machine::pages::Stack)
                            -> Result<PreparedEntry, EntryError>
{
    hart::prepare(stack)
}
/// The scheduler owns a completely captured or unclaimed CPU frame under its
/// lock. No other hart may restore it until this return-register write ends.
pub(crate) fn worker_resume_status(state: &SavedState, status: Status)
{
    // SAFETY: caller holds the actual thread's scheduler/backing locks, and
    // its execution claim is absent or backed by this hart's complete outer
    // capture. The mutable dispatch borrow has ended; no restore is concurrent.
    unsafe {
        abi::set_result(&mut *state.frame(), status as usize);
    }
}
pub(crate) fn publish_cpu(state: &SavedState)
{
    state.published();
    hart::publish(state)
}
pub(crate) fn entry_lost(reason: &str) -> !
{
    // Entry failures can occur on admitted peers. The fixed-owner ROM logger
    // is not a secondary service; terminal diagnostics bypass it entirely.
    crate::machine::emergency::serial(format_args!("SYSTEM_ENTRY_LOST: {reason}\n"));
    crate::machine::power::fatal_shutdown()
}

pub(crate) fn can_defer_io() -> bool
{
    hart::can_defer_io()
}

/// # Safety
/// Only the explicit HART entry role may call this. The native binding proves
/// its actual prepared row before interpreting the supplied monitor payload.
pub(crate) unsafe fn start_secondary(payload: *const (), bytes: usize) -> !
{
    hart::start_secondary(payload, bytes)
}
