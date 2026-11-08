//! Bind std's OS-key TLS to the published canonical Thread Manager protocol.
use mac_abi::tls;
use mac_traps::native_threads::Failure;

use super::{failure, runtime};
use crate::{boxed::Box, ptr};

#[cfg(target_arch = "riscv64")]
#[path = "native_tls/riscv64.rs"]
mod machine;

#[cfg(not(target_arch = "riscv64"))]
compile_error!("System RISC std TLS requires a machine thread-pointer adapter");

const CACHE_SLOTS: usize = 64;

#[derive(Clone, Copy)]
struct CachedValue
{
    key:   usize,
    value: *mut u8
}

struct ThreadCache
{
    id:           u64,
    tearing_down: bool,
    values:       [CachedValue; CACHE_SLOTS]
}

// This target has no ELF TLS and reserves tp for its native runtime. The
// scheduler saves/restores tp with the actual thread frame, including across
// coroutine suspension. The cache is private userspace storage, never System
// authority. std's unsafe key API requires a live key; full generation-bearing
// keys distinguish reused slots. SET always updates the canonical TLS manager.
fn local() -> *mut ThreadCache
{
    let mut local = machine::thread_pointer().cast::<ThreadCache>();
    if local.is_null()
    {
        let empty = CachedValue { key:   0,
                                  value: ptr::null_mut() };
        let cache = ThreadCache { id:           0,
                                  tearing_down: false,
                                  values:       [empty; CACHE_SLOTS] };
        local = Box::into_raw(Box::new(cache));
        unsafe {
            machine::set_thread_pointer(local.cast());
        }
    }
    local
}

fn cache_slot(key: usize) -> usize
{
    (key ^ (key >> 32)) % CACHE_SLOTS
}

unsafe fn remember(local: *mut ThreadCache, key: usize, value: *mut u8)
{
    unsafe {
        if !(*local).tearing_down
        {
            (*local).values[cache_slot(key)] = CachedValue { key,
                                                             value };
        }
    }
}

fn refused(operation: &str, cause: Failure) -> !
{
    failure::report(format_args!("Native TLS {operation} refused: {cause}"))
}

/// # Safety
/// A supplied callback has this ABI and remains live through key retirement and
/// all outstanding callback receipts; it never unwinds through the C boundary.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __system_risc_tls_create_v1(destructor: Option<unsafe extern "C" fn(*mut u8)>)
                                                     -> usize
{
    let mut call = tls::Call { operation: tls::CREATE,
                               destructor: destructor.map_or(0, |callback| callback as usize),
                               ..Default::default() };
    if let Err(cause) = unsafe { runtime::client().tls(&mut call) }
    {
        refused("create", cause);
    }
    match usize::try_from(call.key)
    {
        Ok(key) if key != 0 => key,
        _ => refused("create", Failure::Protocol("CREATE returned an invalid key"))
    }
}

/// # Safety
/// The key is live in this process. The caller owns the opaque value lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __system_risc_tls_get_v1(key: usize) -> *mut u8
{
    let local = local();
    unsafe {
        let cached = (*local).values[cache_slot(key)];
        if !(*local).tearing_down && key != 0 && cached.key == key
        {
            return cached.value;
        }
    }
    let mut call = tls::Call { operation: tls::GET,
                               key: key as u64,
                               ..Default::default() };
    if let Err(cause) = unsafe { runtime::client().tls(&mut call) }
    {
        refused("get", cause);
    }
    let value = call.value as *mut u8;
    unsafe { remember(local, key, value) };
    value
}

/// # Safety
/// The key is live in this process; its callback may later receive this value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __system_risc_tls_set_v1(key: usize, value: *mut u8)
{
    let mut call = tls::Call { operation: tls::SET,
                               key: key as u64,
                               value: value.addr(),
                               ..Default::default() };
    if let Err(cause) = unsafe { runtime::client().tls(&mut call) }
    {
        refused("set", cause);
    }
    unsafe { remember(local(), key, value) };
}

/// # Safety
/// The caller will not reuse the key. Deletion does not run value destructors.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __system_risc_tls_delete_v1(key: usize)
{
    let mut call = tls::Call { operation: tls::DELETE,
                               key: key as u64,
                               ..Default::default() };
    if let Err(cause) = unsafe { runtime::client().tls(&mut call) }
    {
        refused("delete", cause);
    }
    unsafe {
        let local = local();
        let slot = &mut (*local).values[cache_slot(key)];
        if slot.key == key
        {
            *slot = CachedValue { key:   0,
                                  value: ptr::null_mut() };
        }
    }
}

/// # Safety
/// Run once on the actual exiting thread after its ordinary code has returned.
/// Services, heap and callback code remain pinned; no TLS use follows FINISH.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __system_risc_tls_teardown_v1()
{
    let local = local();
    // BEGIN/NEXT may clear values before invoking their destructors. During
    // teardown all lookups must observe that canonical state, including new
    // TLS initialized by another key's destructor and subsequent rounds.
    unsafe { (*local).tearing_down = true };
    if let Err(cause) = unsafe { mac_traps::native_threads::teardown_tls(runtime::client()) }
    {
        refused("teardown", cause);
    }
    unsafe {
        machine::set_thread_pointer(ptr::null_mut());
        drop(Box::from_raw(local));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __system_risc_thread_id_v1() -> u64
{
    // Identity queries also occur after canonical TLS FINISH (GUI error
    // reporting). Do not allocate or recreate TLS storage merely for an ID.
    let local = machine::thread_pointer().cast::<ThreadCache>();
    unsafe {
        if !local.is_null() && (*local).id != 0
        {
            return (*local).id;
        }
    }
    match runtime::client().current_native_thread()
    {
        Ok(thread) =>
        {
            if !local.is_null()
            {
                unsafe { (*local).id = thread.0 };
            }
            thread.0
        }
        Err(cause) => refused("current thread", cause)
    }
}
