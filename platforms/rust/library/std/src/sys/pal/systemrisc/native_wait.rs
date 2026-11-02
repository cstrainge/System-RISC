//! The std atomic/futex algorithms use the real scheduler's compare-and-park.
use mac_traps::address_wait::WaitOutcome;

use super::{failure, runtime};
use crate::{sync::atomic::AtomicU32, time::Duration};

/// # Safety
/// Address names a live initialized AtomicU32 in this process's admitted writable
/// storage for the full call. Other threads use the same atomic protocol; no
/// non-atomic access, free, or address reuse is permitted during the borrow.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __system_risc_wait_u32_v1(address: *const u32, expected: u32,
                                                   timed: bool, seconds: u64, nanoseconds: u32)
                                                   -> bool
{
    if address.is_null()
       || !address.addr().is_multiple_of(align_of::<AtomicU32>())
       || nanoseconds >= 1_000_000_000
       || (!timed && (seconds != 0 || nanoseconds != 0))
    {
        runtime::failure(mac_abi::BAD_ARGUMENT, b"invalid std atomic wait request");
    }
    let atomic = unsafe { AtomicU32::from_ptr(address.cast_mut()) };
    let timeout = timed.then(|| Duration::new(seconds, nanoseconds));
    match runtime::client().wait_u32(atomic, expected, timeout)
    {
        Ok(WaitOutcome::TimedOut) => false,
        Ok(WaitOutcome::Woken | WaitOutcome::ValueChanged) => true,
        Err(cause) => failure::report(format_args!("Native atomic wait refused: {cause}"))
    }
}

/// # Safety
/// Address obeys the same atomic lifetime/aliasing contract as the wait call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __system_risc_wake_u32_v1(address: *const u32, all: bool) -> bool
{
    if address.is_null() || !address.addr().is_multiple_of(align_of::<AtomicU32>())
    {
        runtime::failure(mac_abi::BAD_ARGUMENT, b"invalid std atomic wake request");
    }
    let atomic = unsafe { AtomicU32::from_ptr(address.cast_mut()) };
    let result = if all
    {
        runtime::client().wake_all_u32(atomic).map(|count| count != 0)
    }
    else
    {
        runtime::client().wake_u32(atomic)
    };
    match result
    {
        Ok(awakened) => awakened,
        Err(cause) => failure::report(format_args!("Native atomic wake refused: {cause}"))
    }
}
