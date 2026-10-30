//! Private CRT bridge. Symbols must be supplied by the matching application
//! runtime; absent native services fail linkage, never acquire host fallbacks.
use mac_abi::{Status, SystemServices};
use mac_traps::Client;

pub type Destructor = unsafe extern "C" fn(*mut u8);

unsafe extern "C" {
    fn __system_risc_services_v1() -> *const SystemServices;
    fn __system_risc_failure_v1(status: Status, message: *const u8, length: usize) -> !;
    // Default hook only. The CRT streams to System's existing CRASH service;
    // identities come from the resident caller, not app-supplied report fields.
    pub fn __system_risc_panic_v1(message: *const u8, message_length: usize, file: *const u8,
                                  file_length: usize, line: u32, column: u32)
                                  -> !;
    // Explicit abort/custom-hook completion does not run the default hook again.
    pub fn __system_risc_abort_v1() -> !;
    // Infallible allocation refusal retains its exact layout and NO_MEMORY
    // cause through the System report path, without invoking a user panic hook.
    pub fn __system_risc_oom_v1(size: usize, alignment: usize) -> !;

    // TLS keys identify process-owned, generational Thread Manager records.
    // Adapters terminate on failure where std's internal interface is infallible.
    pub fn __system_risc_tls_create_v1(destructor: Option<Destructor>) -> usize;
    pub fn __system_risc_tls_get_v1(key: usize) -> *mut u8;
    pub fn __system_risc_tls_set_v1(key: usize, value: *mut u8);
    pub fn __system_risc_tls_delete_v1(key: usize);
    pub fn __system_risc_tls_teardown_v1();

    // Wait comparison/enqueue is atomic with wake; return false only on timeout.
    // The relative duration is split, preserving the full Rust Duration range.
    pub fn __system_risc_wait_u32_v1(address: *const u32, expected: u32, timed: bool,
                                     seconds: u64, nanoseconds: u32)
                                     -> bool;
    pub fn __system_risc_wake_u32_v1(address: *const u32, all: bool) -> bool;

    // Creation consumes data iff it returns OK, and invokes start exactly once.
    // join/detach consume a join right only after their successful native turn.
    pub fn __system_risc_thread_create_v1(stack: usize, start: unsafe extern "C" fn(*mut u8),
                                          data: *mut u8, handle: *mut u64)
                                          -> Status;
    pub fn __system_risc_thread_join_v1(handle: u64);
    pub fn __system_risc_thread_detach_v1(handle: u64);
    pub fn __system_risc_thread_id_v1() -> u64;
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
    match unsafe { Client::from_raw(__system_risc_services_v1()) }
    {
        Ok(client) => client,
        Err(cause) => failure(cause.status(), b"std entered before valid System services")
    }
}

pub fn failure(status: Status, message: &[u8]) -> !
{
    unsafe { __system_risc_failure_v1(status, message.as_ptr(), message.len()) }
}
