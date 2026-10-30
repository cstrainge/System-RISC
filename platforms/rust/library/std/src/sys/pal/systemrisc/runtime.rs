//! Platform startup and the shared process-lifetime service installation.
//! Remaining native adapters must bind real services; absent dependencies fail
//! linkage rather than acquiring host fallbacks.
use mac_abi::{Status, SystemServices};
use mac_traps::Client;

use crate::sync::atomic::{AtomicPtr, Ordering};

static SERVICES: AtomicPtr<SystemServices> = AtomicPtr::new(crate::ptr::null_mut());

/// The Toolbox Cargo instance and this private sysroot instance share one
/// installation through this C-layout boundary. Null means startup is absent.
#[unsafe(no_mangle)]
pub extern "C" fn __system_risc_services_v1() -> *const SystemServices
{
    SERVICES.load(Ordering::Acquire)
}

/// # Safety
/// The native loader enters this once, with a readable aligned services table
/// and immutable callbacks pinned until all application threads have quiesced.
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
    if unsafe { __system_risc_thread_id_v1() } == 0
    {
        failure(mac_abi::BAD_ABI, b"std startup has no canonical native thread");
    }
    unsafe extern "C" {
        // Measured pinned compiler wrapper ABI; not lang_start's isize ABI.
        fn main(argc: i32, argv: *const *const u8) -> i32;
    }
    // The existing graphical APPL launch contract has no argument vector.
    // Owned CLI/launch argument delivery is a separate resident service gate.
    let argv = [crate::ptr::null()];
    let result = unsafe { main(0, argv.as_ptr()) };
    // lang_start has completed std cleanup. Normal main return now destroys
    // current-thread TLS while its services/heap still exist, then exits the
    // process with a real i32 result, never interpreting it as a service Status.
    unsafe {
        __system_risc_tls_teardown_v1();
        __system_risc_exit_v1(result)
    }
}

unsafe extern "C" {
    // Creation consumes data iff it returns OK, and invokes start exactly once.
    // join/detach consume a join right only after their successful native turn.
    pub fn __system_risc_thread_create_v1(stack: usize, start: unsafe extern "C" fn(*mut u8),
                                          data: *mut u8, handle: *mut u64)
                                          -> Status;
    pub fn __system_risc_thread_join_v1(handle: u64);
    pub fn __system_risc_thread_detach_v1(handle: u64);
    pub fn __system_risc_thread_name_v1(name: *const u8, length: usize);
    pub fn __system_risc_thread_yield_v1();
    pub fn __system_risc_thread_sleep_v1(seconds: u64, nanoseconds: u32);
    pub fn __system_risc_parallelism_v1(count: *mut usize) -> Status;
    pub fn __system_risc_entropy_v1(bytes: *mut u8, length: usize) -> Status;
    pub fn __system_risc_exit_v1(code: i32) -> !;
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
