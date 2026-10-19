//! The only native trap entry: register/CSR preservation and checked ABI dispatch.
use core::arch::{asm, global_asm};

use mac_abi::{traps::*, *};

use crate::toolbox;
global_asm!(include_str!("traps.S"));
unsafe extern "C" {
    fn __system_trap_vector();
    pub(crate) fn __system_trap_gateway(number: u32, request: *mut (), size: usize) -> i64;
}
pub(crate) use __system_trap_gateway as gateway;
#[repr(C)]
struct TrapFrame
{
    registers: [usize; 30],
    pc:        usize,
    status:    usize,
    cause:     usize,
    value:     usize
}

pub(crate) fn install()
{
    unsafe {
        asm!("csrw mtvec, {}",in(reg)__system_trap_vector as *const() as usize);
    }
    serial!("TRAPS_READY: resident ecall dispatcher ABI={}", ABI_VERSION);
}

#[unsafe(no_mangle)]
extern "C" fn __system_trap_dispatch(frame: &mut TrapFrame)
{
    if frame.cause != 11
    {
        serial!("SYSTEM_TRAP: cause={} pc=0x{:x} value=0x{:x}", frame.cause, frame.pc, frame.value);
        loop
        {
            core::hint::spin_loop();
        }
    }
    let thread = crate::context::current_thread_context();
    let scope = thread.traps.enter();
    let depth = scope.depth;
    if depth == 2 && !thread.traps.nesting_reported()
    {
        serial!("TRAP_NESTED: definition -> OS service");
    }
    if depth > 8
    {
        panic!("excessive native trap nesting");
    }
    let number = frame.registers[15] as u32;
    let request = frame.registers[8] as *mut ();
    let size = frame.registers[9];
    let result = unsafe { dispatch(number, request, size) };
    frame.registers[8] = result as usize;
    frame.pc = frame.pc.wrapping_add(4);
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

/// Native modules share one M-mode address space. Size/alignment are checked here;
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
        FILE =>
        {
            crate::kernel::with_files(|files| unsafe {
                toolbox::files::native::dispatch(files, pointer, size)
            }).unwrap_or_else(|error| error.status() as i64)
        }
        MEMORY =>
        {
            crate::kernel::with_memory(|manager| unsafe {
                toolbox::memory::native::dispatch(manager, number, pointer, size)
            }).unwrap_or_else(|error| error.status() as i64)
        }
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
