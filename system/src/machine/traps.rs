//! The only native trap entry: register/CSR preservation and checked ABI dispatch.
use core::arch::{asm, global_asm};

use mac_abi::{traps::*, *};

use crate::toolbox;
mod state;
mod definition_services;
mod hart;
pub(crate) use hart::{Error as EntryError, OuterCapture, PreparedEntry, PreparedHart,
                      StartingHart, activity as cpus, admit_bootstrap, execution,
                      prepare_secondary};
pub(crate) const MAX_TRAP_DEPTH: usize = 8;
// One bootstrap baseline, eight admitted service levels, and one typed-refusal
// frame. The bound follows the existing native service recursion contract.
pub(crate) const CPU_SLOTS: usize = MAX_TRAP_DEPTH + 2;
pub(crate) use state::SavedState;
static CPU_STATE_BYTES: spin::Once<usize> = spin::Once::new();
global_asm!(include_str!("traps.S"),
    frame_bytes=const core::mem::size_of::<TrapFrame>(),
    fp=const core::mem::offset_of!(TrapFrame,floating),
    fcsr=const core::mem::offset_of!(TrapFrame,fcsr),
    vl=const core::mem::offset_of!(TrapFrame,vl),
    vtype=const core::mem::offset_of!(TrapFrame,vtype),
    vstart=const core::mem::offset_of!(TrapFrame,vstart),
    vcsr=const core::mem::offset_of!(TrapFrame,vcsr),
    saved_sp=const core::mem::offset_of!(TrapFrame,stack_pointer),
    prior_depth=const core::mem::offset_of!(TrapFrame,prior_depth),
    ctl_entering=const core::mem::offset_of!(hart::Control,entering),
    ctl_depth=const core::mem::offset_of!(hart::Control,depth),
    ctl_frames=const core::mem::offset_of!(hart::Control,frames),
    ctl_stride=const core::mem::offset_of!(hart::Control,stride),
    ctl_floor=const core::mem::offset_of!(hart::Control,floor),
    ctl_top=const core::mem::offset_of!(hart::Control,top),
    ctl_tmp0=const core::mem::offset_of!(hart::Control,temporary0),
    ctl_tmp1=const core::mem::offset_of!(hart::Control,temporary1),
    ctl_tmp2=const core::mem::offset_of!(hart::Control,temporary2),
    ctl_sp=const core::mem::offset_of!(hart::Control,interrupted_sp),
    cpu_slots=const CPU_SLOTS);
unsafe extern "C" {
    fn __system_trap_vector();
    pub(crate) fn __system_trap_gateway(number: u32, request: *mut (), size: usize) -> i64;
}
pub(crate) use __system_trap_gateway as gateway;
#[repr(C, align(16))]
#[derive(Default)]
pub(crate) struct TrapFrame
{
    pub registers:     [usize; 30],
    pub pc:            usize,
    pub status:        usize,
    cause:             usize,
    value:             usize,
    floating:          [u64; 32],
    fcsr:              usize,
    vl:                usize,
    vtype:             usize,
    vstart:            usize,
    vcsr:              usize,
    pub stack_pointer: usize,
    pub prior_depth:   usize
}
// Integer offsets remain the assembly gate contract. The extended offsets and
// fixed frame size are generated above from this one Rust representation.
const _: () = assert!(core::mem::offset_of!(TrapFrame, pc) == 240
                      && core::mem::offset_of!(TrapFrame, status) == 248);
pub(crate) const INITIAL_STATUS: usize = 0x120 | 0x6600;
/// Validate the selected sizes against this CPU before allocating pools or
/// starting services. This is policy admission, not System-stack activation.
pub(crate) fn validate_stack_configuration(
    )
    -> Result<(), (&'static str, mac_kernel_model::stack::Error)>
{
    use mac_kernel_model::{pages::PageGeometry,
                           stack::{self, Policy}};

    use super::memory::*;
    let vector_bytes: u64;
    unsafe {
        asm!("csrr {},vlenb", out(reg)vector_bytes, options(nomem,nostack));
    }
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

/// Local CSR installation only. Warm entry uses this while its Control stays
/// poisoned; it must not initialize global tracing or use firmware logging.
fn install_local()
{
    unsafe {
        asm!("csrw stvec, {}",in(reg)__system_trap_vector as *const() as usize);
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
              super::paging::with_kernel(|| {
                  // SAFETY: assembly saved the complete live frame. This borrow ends,
                  // including service/context readers, before any ownership handoff.
                  let (action, outer) = {
                      let borrowed = &mut *frame;
                      (dispatch_frame(borrowed), borrowed.prior_depth == 0)
                  };
                  let next = match action
                  {
                      Dispatch::Resume => frame,
                      Dispatch::Schedule =>
                      {
                          let capture = hart::captured_outer(frame);
                          if capture.hart().requires_schedule()
                          {
                              super::scheduler::schedule(capture)
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
                              Ok(true) => super::scheduler::wake::acknowledged(key),
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
                                  super::emergency::enter(crate::emergency::Kind::SystemPanic,
                                                          format_args!("Supervisor wake binding \
                                                                        lost on hart {}: {}.",
                                                                       key.number(),
                                                                       error));
                              }
                          }
                          if key.requires_schedule()
                          {
                              super::scheduler::schedule(capture)
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
                      super::scheduler::wake::flush();
                  }
                  next
              })
          }
    {
        Ok(frame) => frame,
        Err(error) =>
        {
            super::paging::report_refusal(&error);
            unsafe {
                asm!("ebreak", options(noreturn));
            }
        }
    }
}
enum Dispatch
{
    Resume,
    Schedule,
    Wake
}
fn dispatch_frame(frame: &mut TrapFrame) -> Dispatch
{
    if frame.cause == (1usize << 63) | 1
    {
        // Clear the local hardware source before any ACK observation. No
        // frame reference or trace/context guard may survive into that ACK.
        // No nomem: keep publication of the later atomic ACK after this CSR.
        unsafe {
            asm!("csrci sip, 2", options(nostack));
        }
        return Dispatch::Wake;
    }
    #[cfg(feature = "event-trace")]
    hart::observe_entry(frame as *mut TrapFrame as usize);
    if frame.cause == (1usize << 63) | 5
    {
        return Dispatch::Schedule;
    }

    if frame.cause != 9
    {
        super::emergency::serial(format_args!("SYSTEM_TRAP: cause={} pc=0x{:x} value=0x{:x}",
                                              frame.cause, frame.pc, frame.value));
        // No context/definition/registry lock is safe to reacquire after an
        // arbitrary privileged fault. Report trusted CPU fields and terminate;
        // this is a terminal screen/reset, never mid-call recovery (M-D04).
        super::emergency::enter(crate::emergency::Kind::PrivilegedFault,
                                format_args!("Privileged trap: cause {}, PC {:#x}, fault \
                                              address {:#x}. Execution cannot safely continue.",
                                             frame.cause, frame.pc, frame.value));
    }
    let number = frame.registers[15] as u32;
    if execution::policy::deny(execution::policy::Operation::Service)
    {
        // Refuse before a request pointer or an executor-owned trap scope is
        // borrowed. A policy may neither park nor enter ordinary services.
        frame.registers[8] = ACCESS_DENIED as usize;
        frame.pc = frame.pc.wrapping_add(4);
        return Dispatch::Resume;
    }
    if number == mac_module::abi::worker::TRAP
    {
        return worker_trap(frame);
    }
    if number == super::scheduler::service::host::TRAP
    {
        // The one internal System gate validates its actual callsite/scalars;
        // no caller output slot is borrowed on the peer before migration.
        return match unsafe { super::scheduler::service::host::dispatch(frame) }
        {
            super::scheduler::service::host::Outcome::Resume => Dispatch::Resume,
            super::scheduler::service::host::Outcome::Migrate => Dispatch::Schedule
        };
    }
    let completion = match super::scheduler::service::enter_ecall(frame)
    {
        Ok(super::scheduler::service::Entry::Here(completion)) => completion,
        Ok(super::scheduler::service::Entry::Migrate) => return Dispatch::Schedule,
        Err(status) =>
        {
            frame.registers[8] = status as isize as usize;
            frame.pc = frame.pc.wrapping_add(4);
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
    let number = frame.registers[15] as u32;
    let request = frame.registers[8] as *mut ();
    let size = frame.registers[9];
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
    let result = if let Err(status) = admission
    {
        status as i64
    }
    else if matches!(number, WAIT_EVENT | POSITIONED_EVENT) && physical_depth != 1
    {
        WRONG_THREAD as i64
    }
    else
    {
        hart::service_scope(physical_depth == 1, || unsafe { dispatch(number, request, size) })
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
            finish_service(frame, completion, BAD_ABI as i64);
            return Dispatch::Resume;
        }
        // All request slices, legacy conversion temporaries and manager guards
        // are gone. Preserve original request registers/PC for a fresh borrow
        // when the completion wakes the same generation-tagged OS thread.
        match super::scheduler::wait_io(job)
        {
            Ok(true) => return Dispatch::Schedule,
            Ok(false) => return Dispatch::Resume, // completion won the race; replay now
            Err(error) =>
            {
                finish_service(frame, completion, error as i64);
                return Dispatch::Resume;
            }
        }
    }
    finish_service(frame, completion, result);
    if physical_depth == 1 && super::scheduler::retired()
    {
        return Dispatch::Schedule;
    }
    if matches!(number, WAIT_EVENT | POSITIONED_EVENT)
       && result == OK as i64
       && super::scheduler::blocked()
    {
        return Dispatch::Schedule;
    }
    Dispatch::Resume
}

/// The service has finished and every manager/request borrow ended. Preserve
/// its real result/PC commit before restoring automatic eligibility. Deferred
/// replay intentionally never calls this helper.
fn finish_service(frame: &mut TrapFrame,
                  completion: Option<super::scheduler::service::Completion>, result: i64)
{
    frame.registers[8] = result as usize;
    frame.pc = frame.pc.wrapping_add(4);
    if let Err(status) = super::scheduler::service::complete(completion)
    {
        frame.registers[8] = status as isize as usize;
    }
}

/// Private worker suspension uses the actual CPU trap/switch path. A direct C
/// Host callback can never abandon a foreign frame or resident manager guard.
fn worker_trap(frame: &mut TrapFrame) -> Dispatch
{
    use mac_module::abi::{WorkerCall, worker};
    let operation = || -> Result<bool, Status> {
        if frame.prior_depth != 0
           || frame.status & (1 << 5) == 0
           || crate::context::critical_active()
        {
            return Err(WRONG_THREAD);
        }
        let context = crate::context::current_thread_context();
        if !context.is_worker_body()
        {
            return Err(WRONG_THREAD);
        }
        let (id, cancelled) = super::scheduler::worker_current()?;
        if id != context.identity().thread.0
        {
            return Err(WRONG_THREAD);
        }
        let call = unsafe {
                       request::<WorkerCall>(frame.registers[8] as *mut (), frame.registers[9])
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
                super::scheduler::worker_park(None)
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
                super::scheduler::worker_park(Some(deadline))
            }
            worker::EXIT =>
            {
                super::scheduler::worker_exit()?;
                call.state = worker::FINISHED;
                Ok(true)
            }
            _ => Err(BAD_ARGUMENT)
        }
    };
    let result = operation();
    frame.pc = frame.pc.wrapping_add(4);
    frame.registers[8] = result.as_ref().map_or_else(|e| *e, |_| OK) as usize;
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
        NATIVE_THREAD =>
        {
            let call = read_request!(mac_abi::native_threads::Call);
            toolbox::native_threads::native::current(call) as i64
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
            crate::kernel::with_desktop(|runtime| unsafe {
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
            crate::kernel::with_desktop(|runtime| runtime.menu(call)).unwrap_or_else(|error| {
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
            super::emergency::serial(format_args!("DEFINITION_PANIC_FATAL: kind={family} \
                                                   id={id} app={} thread={} flags={} {text}",
                                                  identity.app.0, identity.thread.0, report.flags));
            if report.flags & mac_abi::definitions::DefinitionPanic::TRUNCATED != 0
            {
                super::emergency::serial(format_args!("DEFINITION_DIAGNOSTIC_TRUNCATED: \
                                                       retained {} UTF-8 bytes",
                                                      report.length));
            }
            if report.flags & mac_abi::definitions::DefinitionPanic::FORMAT_FAILED != 0
            {
                super::emergency::serial(format_args!("DEFINITION_DIAGNOSTIC_FORMAT_FAILED: \
                                                       partial panic information retained"));
            }
            #[cfg(feature = "emergency-qualification")]
            crate::kernel::qualify_emergency_with_allocator_locked();
            #[cfg(not(feature = "emergency-qualification"))]
            super::emergency::enter(crate::emergency::Kind::SystemPanic,
                                    format_args!("Definition panic: {family} {id}, app {}, \
                                                  thread {}, flags {}.\n{text}",
                                                 identity.app.0, identity.thread.0, report.flags))
        }
        THEME =>
        {
            let palette = read_request!(mac_abi::theme::Palette);
            *palette = toolbox::theme::palette();
            OK as i64
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
            crate::kernel::with_desktop(|runtime| runtime.event_service(call))
                .unwrap_or_else(|error| error.status()) as i64
        }
        DRAG =>
        unsafe {
            toolbox::drag::native::dispatch(pointer, size, |call, input, output| {
                crate::kernel::with_desktop(|runtime| runtime.drag_request(call, input, output))
                    .map_err(|error| error.status())?
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
                crate::kernel::with_desktop(|desktop|desktop.scroll_box(call)).unwrap_or_else(|e|Err(e.status()))
            })
        },
        CONTROL =>
        {
            let call = read_request!(mac_abi::controls::ControlCall);
            if !call.surface.is_null()
            {
                return crate::kernel::with_desktop(|runtime| unsafe {
                           toolbox::controls::native::dispatch(CONTROL,
                                                               pointer,
                                                               size,
                                                               |id| runtime.control_definition(id),
                                                               gateway)
                       }).unwrap_or_else(|error| error.status() as i64);
            }
            crate::kernel::with_desktop(|runtime| runtime.control(call)).unwrap_or_else(|error| {
                                                                            error.status()
                                                                        }) as i64
        }
        GEOMETRY => toolbox::quickdraw::geometry::dispatch(read_request!(GeometryCall)) as i64,
        SURFACE =>
        unsafe { toolbox::quickdraw::surface_gate(number, pointer, size) },
        QUICKDRAW | FONT | REGION | COPY_BITS | WORLD | CURSOR | MAPPING | RESOURCE | PICTURE
        | ICON | IMAGE =>
        unsafe {
            toolbox::quickdraw::native::dispatch(number, pointer, size, crate::kernel::fonts())
        },
        WINDOW =>
        {
            let call = read_request!(WindowCall);
            crate::kernel::with_desktop(|runtime|runtime.window(call.operation,&mut call.request)).unwrap_or_else(|error|error.status()) as i64
        }
        DRAW =>
        {
            let call = read_request!(DrawCall);
            crate::kernel::with_desktop(|runtime|runtime.draw(call.window,call.command)).unwrap_or_else(|error|error.status()) as i64
        }
        NEXT_EVENT =>
        {
            let call = read_request!(NextEventCall);
            crate::kernel::with_desktop(|runtime| {
                call.event = runtime.next(call.mask);
                OK
            }).unwrap_or_else(|error| error.status()) as i64
        }
        POSITIONED_EVENT =>
        {
            let call = read_request!(events::PositionedEventCall);
            crate::kernel::with_desktop(|runtime| {
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
                        super::scheduler::wait_current(call.mask, call.timeout_ticks)
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
            crate::kernel::with_desktop(|runtime| {
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
                        super::scheduler::wait_current(call.mask, call.timeout_ticks)
                    {
                        return status;
                    }
                }
                OK
            }).unwrap_or_else(|error| error.status()) as i64
        }
        DEFINITION =>
        {
            let call = read_request!(DefinitionCall);
            call.gate = gateway;
            crate::kernel::with_desktop(|runtime| runtime.definition(call)).unwrap_or_else(|error|error.status() as i64)
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
            crate::kernel::with_desktop(|runtime| {
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

pub(crate) fn prepare_entry(stack: &'static super::pages::Stack)
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
        (*state.frame()).registers[8] = status as usize;
    }
}
pub(crate) fn publish_cpu(state: &SavedState)
{
    hart::publish(state)
}
pub(super) fn entry_lost(reason: &str) -> !
{
    // Entry failures can occur on admitted peers. The fixed-owner ROM logger
    // is not a secondary service; terminal diagnostics bypass it entirely.
    super::emergency::serial(format_args!("SYSTEM_ENTRY_LOST: {reason}\n"));
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
