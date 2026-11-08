//! Platform startup and the shared process-lifetime service installation.
//! Remaining native adapters must bind real services; absent dependencies fail
//! linkage rather than acquiring host fallbacks.
use mac_abi::{Status, SystemServices};
use mac_traps::Client;

use crate::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

#[path = "callbacks.rs"]
mod callbacks;

static SERVICES: AtomicPtr<SystemServices> = AtomicPtr::new(crate::ptr::null_mut());
static MAIN_THREAD: AtomicU64 = AtomicU64::new(0);
static STREAMS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

pub(crate) fn stream(index: usize) -> u64
{
    STREAMS[index].load(Ordering::Acquire)
}

/// The Toolbox Cargo instance and this private sysroot instance share one
/// installation through this C-layout boundary. Null means startup is absent.
#[unsafe(no_mangle)]
pub extern "C" fn __system_risc_services_v1() -> *const SystemServices
{
    SERVICES.load(Ordering::Acquire)
}

/// # Safety
/// The native loader enters this once, with a readable aligned services table
/// and immutable descriptor storage pinned until all application threads quiesce.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> Status
{
    if services.is_null() || !services.addr().is_multiple_of(align_of::<SystemServices>())
    {
        mac_traps::emergency::abort(format_args!("STD_STARTUP_INVALID_SERVICES"));
    }
    if let Err(cause) = unsafe { Client::from_raw(services) }
    {
        mac_traps::emergency::abort(format_args!("STD_STARTUP_SERVICES_REFUSED: {cause}"));
    }
    if SERVICES.compare_exchange(crate::ptr::null_mut(),
                                 services.cast_mut(),
                                 Ordering::AcqRel,
                                 Ordering::Acquire)
               .is_err()
    {
        mac_traps::emergency::abort(format_args!("STD_STARTUP_REENTERED"));
    }
    // Main-thread storage is a loader/Thread Manager admission prerequisite,
    // not something std can create after current-thread initialization starts.
    let main_thread = unsafe { __system_risc_thread_id_v1() };
    if main_thread == 0
    {
        failure(mac_abi::BAD_ABI, b"std startup has no canonical native thread");
    }
    MAIN_THREAD.store(main_thread, Ordering::Release);
    let mut streams = mac_abi::processes::ProcessCall { operation:
                                                            mac_abi::processes::STANDARD_STREAMS,
                                                        ..Default::default() };
    if let Err(error) = client().process(&mut streams)
    {
        failure(error.status(), b"std startup could not read standard streams");
    }
    for (slot, handle) in STREAMS.iter().zip(streams.streams)
    {
        slot.store(handle, Ordering::Release);
    }
    unsafe extern "C" {
        // Measured pinned compiler wrapper ABI; not lang_start's isize ABI.
        fn main(argc: i32, argv: *const *const u8) -> i32;
    }
    // Graphical applications retain the empty argv contract.
    let result = if streams.streams == [0; 3]
    {
        unsafe { main(0, [crate::ptr::null()].as_ptr()) }
    }
    else
    {
        unsafe { run_cli_main(main) }
    };
    // lang_start has completed std cleanup. Normal main return now destroys
    // current-thread TLS while its services/heap still exist, then exits the
    // process with a real i32 result, never interpreting it as a service Status.
    unsafe {
        __system_risc_tls_teardown_v1();
        if streams.streams == [0; 3]
        {
            super::failure::show_main_error(result);
        }
        __system_risc_exit_v1(result)
    }
}

unsafe fn run_cli_main(main: unsafe extern "C" fn(i32, *const *const u8) -> i32) -> i32
{
    let mut bytes = [0; mac_abi::processes::ARGUMENT_BYTES];
    let mut argv = [crate::ptr::null(); mac_abi::processes::ARGUMENT_COUNT + 1];
    let (length, count, path_length) = match client().startup_argv(&mut bytes)
    {
        Ok(result) => result,
        Err(error) => failure(error.status(), b"std startup could not read arguments")
    };
    let data = &bytes[..length];
    let mut argc = 0;
    let mut start = 0;
    for (index, byte) in data.iter().enumerate()
    {
        if *byte == 0
        {
            if argc == count || crate::str::from_utf8(&data[start..index]).is_err()
            {
                failure(mac_abi::BAD_ABI, b"invalid startup arguments");
            }
            argv[argc] = data[start..].as_ptr();
            argc += 1;
            start = index + 1;
        }
    }
    if argc != count || start != length
    {
        failure(mac_abi::BAD_ABI, b"incomplete startup arguments");
    }
    if path_length != 0
    {
        let path = match crate::str::from_utf8(&bytes[length..length + path_length])
        {
            Ok(path) if !path.contains('\0') => path,
            _ => failure(mac_abi::BAD_ABI, b"invalid startup PATH")
        };
        // The process has not entered application main or spawned other threads.
        if unsafe { crate::sys::env::setenv(crate::ffi::OsStr::new("PATH"),
                                           crate::ffi::OsStr::new(path)) }.is_err()
        {
            failure(mac_abi::NO_MEMORY, b"std startup could not install PATH");
        }
    }
    unsafe { main(argc as i32, argv.as_ptr()) }
}

pub(super) fn is_main_thread() -> bool
{
    let main = MAIN_THREAD.load(Ordering::Acquire);
    main != 0 && main == unsafe { __system_risc_thread_id_v1() }
}

/// Explicit process exit skips user TLS destructors. The startup return path
/// performs its own teardown before entering this same signed exit boundary.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __system_risc_exit_v1(code: i32) -> !
{
    terminate(code, mac_traps::native_exit::ExitKind::Normal)
}

pub(super) fn terminate(code: i32, kind: mac_traps::native_exit::ExitKind) -> !
{
    match client().native_exit(code, kind)
    {
        Ok(never) => match never {},
        Err(cause) =>
        {
            mac_traps::emergency::abort(format_args!("STD_EXIT_CONTRACT_FAILURE: {cause}"))
        }
    }
}

pub fn client() -> Client<'static>
{
    // CRT publishes only the validated, immutable, process-lifetime table.
    match installed_client()
    {
        Some(client) => client,
        None => mac_traps::emergency::abort(format_args!("STD_BEFORE_SERVICES"))
    }
}

pub(super) fn installed_client() -> Option<Client<'static>>
{
    // SAFETY: only the native startup validates and publishes this immutable
    // process-lifetime table. A pre-startup null pointer fails without a read.
    unsafe { Client::from_raw(__system_risc_services_v1()) }.ok()
}

pub fn failure(status: Status, message: &[u8]) -> !
{
    let message = crate::str::from_utf8(message).unwrap_or("invalid runtime diagnostic UTF-8");
    super::failure::report(format_args!("Runtime failure: {message}\nStatus: {status}"))
}

pub use super::{failure::{__system_risc_abort_v1, __system_risc_oom_v1, __system_risc_panic_v1},
                native_tls::{__system_risc_thread_id_v1, __system_risc_tls_create_v1,
                             __system_risc_tls_delete_v1, __system_risc_tls_get_v1,
                             __system_risc_tls_set_v1, __system_risc_tls_teardown_v1},
                native_wait::{__system_risc_wait_u32_v1, __system_risc_wake_u32_v1}};
