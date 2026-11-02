//! Atomic-address wait transport for native runtimes. A wait may return
//! spuriously; callers always recheck their synchronization predicate.
use core::{sync::atomic::AtomicU32, time::Duration};

use mac_abi::address_wait as wire;

use crate::{Client,
            native_threads::{Failure, Result}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOutcome
{
    Woken,
    ValueChanged,
    TimedOut
}

impl Client<'_>
{
    /// Wait only if the atomic still equals `expected` when the scheduler
    /// admits the wait. Comparison and wake admission cannot lose a wake.
    /// The atomic must reside in this process's admitted writable storage.
    ///
    /// The borrowed atomic must remain alive throughout the call, including
    /// while another thread wakes it. This API does not identify individual
    /// allocations at a reused address. It provides no implicit ordering for
    /// the caller's other data; use the atomic protocol's required ordering.
    pub fn wait_u32(self, atomic: &AtomicU32, expected: u32, timeout: Option<Duration>)
                    -> Result<WaitOutcome>
    {
        let mut call = wire::Call { address: atomic.as_ptr() as usize,
                                    expected,
                                    ..wire::Call::default() };
        if let Some(timeout) = timeout
        {
            call.flags = wire::FINITE;
            call.seconds = timeout.as_secs();
            call.nanoseconds = timeout.subsec_nanos();
        }
        address_call(self, &mut call)?;
        if call.awakened != 0
        {
            return Err(Failure::Protocol("WAIT32 returned a wake count"));
        }
        match call.outcome
        {
            wire::WOKEN => Ok(WaitOutcome::Woken),
            wire::VALUE_CHANGED => Ok(WaitOutcome::ValueChanged),
            wire::TIMED_OUT => Ok(WaitOutcome::TimedOut),
            _ => Err(Failure::Protocol("WAIT32 returned an unknown outcome"))
        }
    }

    /// Wake one actual waiter. `false` means none was waiting at admission.
    pub fn wake_u32(self, atomic: &AtomicU32) -> Result<bool>
    {
        wake(self, atomic, false).map(|count| count != 0)
    }

    /// Wake all current waiters and return the number made ready.
    pub fn wake_all_u32(self, atomic: &AtomicU32) -> Result<u32>
    {
        wake(self, atomic, true)
    }
}

fn wake(client: Client<'_>, atomic: &AtomicU32, all: bool) -> Result<u32>
{
    let mut call = wire::Call { operation: wire::WAKE32,
                                flags: if all { wire::WAKE_ALL } else { 0 },
                                address: atomic.as_ptr() as usize,
                                ..wire::Call::default() };
    address_call(client, &mut call)?;
    if call.outcome != 0 || (!all && call.awakened > 1)
    {
        return Err(Failure::Protocol("WAKE32 returned invalid outcome/count"));
    }
    Ok(call.awakened)
}

fn address_call(client: Client<'_>, call: &mut wire::Call) -> Result<()>
{
    let original = *call;
    let status = client.invoke(mac_abi::traps::ADDRESS_WAIT, call);
    if call.operation != original.operation
       || call.flags != original.flags
       || call.address != original.address
       || call.expected != original.expected
       || call.seconds != original.seconds
       || call.nanoseconds != original.nanoseconds
       || call.reserved != original.reserved
       || call.reserved2 != original.reserved2
    {
        return Err(Failure::Protocol("address wait changed input fields"));
    }
    crate::native_threads::result(status, call.detail, call.detail_length)
}
