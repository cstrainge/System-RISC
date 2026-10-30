//! Bind std's OS-key TLS to the published canonical Thread Manager protocol.
use mac_abi::tls;
use mac_traps::native_threads::Failure;

use super::{failure, runtime};

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
    let mut call = tls::Call { operation: tls::GET,
                               key: key as u64,
                               ..Default::default() };
    if let Err(cause) = unsafe { runtime::client().tls(&mut call) }
    {
        refused("get", cause);
    }
    call.value as *mut u8
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
}

/// # Safety
/// Run once on the actual exiting thread after its ordinary code has returned.
/// Services, heap and callback code remain pinned; no TLS use follows FINISH.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __system_risc_tls_teardown_v1()
{
    if let Err(cause) = unsafe { mac_traps::native_threads::teardown_tls(runtime::client()) }
    {
        refused("teardown", cause);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __system_risc_thread_id_v1() -> u64
{
    match runtime::client().current_native_thread()
    {
        Ok(thread) => thread,
        Err(cause) => refused("current thread", cause)
    }
}
