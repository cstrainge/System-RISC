//! The only native trap entry: register/CSR preservation and checked ABI dispatch.
use core::arch::{asm, global_asm};

use mac_abi::{traps::*, *};

use crate::toolbox;
mod state;
mod hart;
pub(crate) use hart::{Error as EntryError, PreparedEntry};
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

pub(crate) fn install()
{
    unsafe {
        asm!("csrw stvec, {}",in(reg)__system_trap_vector as *const() as usize);
    }
    serial!("TRAPS_READY: resident ecall dispatcher ABI={}", ABI_VERSION);
    crate::trace::initialize();
}

#[unsafe(no_mangle)]
extern "C" fn __system_trap_dispatch(frame: &mut TrapFrame) -> *mut TrapFrame
{
    // SAFETY: traps.S saved the complete frame on owned storage and hardware
    // cleared SIE before entering this sole native trap boundary.
    match unsafe { super::paging::with_kernel(|| dispatch_frame(frame)) }
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
fn dispatch_frame(frame: &mut TrapFrame) -> *mut TrapFrame
{
    #[cfg(feature = "event-trace")]
    hart::observe_entry(frame as *mut TrapFrame as usize);
    if frame.cause == (1usize << 63) | 5
    {
        return super::scheduler::schedule(frame);
    }

    if frame.cause != 9
    {
        serial!("SYSTEM_TRAP: cause={} pc=0x{:x} value=0x{:x}", frame.cause, frame.pc, frame.value);
        if let Some((kind, id)) = crate::context::current_thread_context().definitions.current()
        {
            serial!("DEFINITION_FAULT_FATAL: kind=0x{kind:08x} id={id}");
        }
        super::power::fatal_shutdown();
    }
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
        frame.registers[8] = SERVICE_BUSY as usize;
        frame.pc = frame.pc.wrapping_add(4);
        return frame;
    }
    let number = frame.registers[15] as u32;
    let request = frame.registers[8] as *mut ();
    let size = frame.registers[9];
    #[cfg(feature = "event-trace")]
    let started = crate::platform::ticks();
    let result = if matches!(number, WAIT_EVENT | POSITIONED_EVENT) && physical_depth != 1
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
            frame.registers[8] = BAD_ABI as usize;
            frame.pc = frame.pc.wrapping_add(4);
            return frame;
        }
        // All request slices, legacy conversion temporaries and manager guards
        // are gone. Preserve original request registers/PC for a fresh borrow
        // when the completion wakes the same generation-tagged OS thread.
        match super::scheduler::wait_io(job)
        {
            Ok(true) => return super::scheduler::schedule(frame),
            Ok(false) => return frame, // completion won the race; replay now
            Err(error) =>
            {
                frame.registers[8] = error as usize;
                frame.pc = frame.pc.wrapping_add(4);
                return frame;
            }
        }
    }
    frame.registers[8] = result as usize;
    frame.pc = frame.pc.wrapping_add(4);
    if physical_depth == 1 && super::scheduler::retired()
    {
        return super::scheduler::schedule(frame);
    }
    if matches!(number, WAIT_EVENT | POSITIONED_EVENT)
       && result == OK as i64
       && super::scheduler::blocked()
    {
        return super::scheduler::schedule(frame);
    }
    frame
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
            if call.length > mac_abi::crashes::MAX_REPORT_BYTES
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
            crate::kernel::with_desktop(|runtime| {
                runtime.crash_request(call, input, output).map_or_else(|status| status, |()| OK)
            }).unwrap_or_else(|error| error.status()) as i64
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
            call.nanoseconds = crate::platform::ticks().saturating_mul(100);
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
            crate::kernel::with_desktop(|runtime| {
                runtime.standard_file_request(call).err().unwrap_or(OK)
            }).unwrap_or_else(|e| e.status()) as i64
        }
        DIALOG =>
        {
            let call = read_request!(mac_abi::dialogs::DialogCall);
            crate::kernel::with_desktop(|runtime| runtime.dialog(call)).unwrap_or_else(|error| {
                                                                           error.status()
                                                                       }) as i64
        }
        MENU =>
        {
            let call = read_request!(mac_abi::menus::MenuCall);
            crate::kernel::with_desktop(|runtime| runtime.menu(call)).unwrap_or_else(|error| {
                                                                         error.status()
                                                                     }) as i64
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
            serial!("DEFINITION_PANIC_FATAL: kind={family} id={id} app={} thread={} flags={} \
                     {text}",
                    identity.app.0,
                    identity.thread.0,
                    report.flags);
            if report.flags & mac_abi::definitions::DefinitionPanic::TRUNCATED != 0
            {
                serial!("DEFINITION_DIAGNOSTIC_TRUNCATED: retained {} UTF-8 bytes", report.length);
            }
            if report.flags & mac_abi::definitions::DefinitionPanic::FORMAT_FAILED != 0
            {
                serial!("DEFINITION_DIAGNOSTIC_FORMAT_FAILED: partial panic information retained");
            }
            super::power::fatal_shutdown()
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
                    super::scheduler::wait_current(call.mask, call.timeout_ticks);
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
                    super::scheduler::wait_current(call.mask, call.timeout_ticks);
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
pub(crate) fn publish_cpu(state: &SavedState)
{
    hart::publish(state)
}
pub(super) fn entry_lost(reason: &str) -> !
{
    serial!("SYSTEM_ENTRY_LOST: {reason}");
    crate::machine::power::fatal_shutdown()
}

pub(crate) fn can_defer_io() -> bool
{
    hart::can_defer_io()
}
