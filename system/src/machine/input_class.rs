//! Input from drivers: the consumer of the `INPT` class.
//!
//! A driver that found a pointer or a keyboard publishes this class, and
//! System asks it what happened. The events are in the vocabulary the input
//! decoder already speaks, so everything above this point is shared with the
//! built-in virtio source.
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use mac_abi::Status;
use mac_input_interface::{CLASS, EVENT_BYTES, Event, MAX_EVENTS, POLL, VERSION};

static PROVIDER: AtomicU64 = AtomicU64::new(0);
static LAST_POLL: AtomicU64 = AtomicU64::new(0);
static LAST_SEARCH: AtomicU64 = AtomicU64::new(0);
static REPORTED: AtomicBool = AtomicBool::new(false);

/// A provider is asked no more often than this many display ticks, and a
/// machine with none is searched for one no more often than this.
const POLL_INTERVAL: u64 = 1;
const SEARCH_INTERVAL: u64 = 60;

fn call(provider: u64) -> Result<alloc::vec::Vec<u8>, Status>
{
    let subject = crate::drivers::jobs::subjects::resident()?.admit(
        &crate::toolbox::handles::subject::Authority::Client(crate::toolbox::handles::system_owner()?),
        crate::allocation::Target::System(crate::allocation::SystemClass::Ordinary))?;
    let mut job = crate::drivers::submit_background_to(&subject,
                                                       provider,
                                                       CLASS,
                                                       VERSION,
                                                       POLL,
                                                       alloc::vec::Vec::new(),
                                                       EVENT_BYTES * MAX_EVENTS)?;
    let completed = job.run_once()?.ok_or(mac_abi::SERVICE_BUSY)?;
    match completed.result
    {
        Ok(bytes) => Ok(bytes),
        Err(crate::drivers::jobs::Failure::Service(status)) => Err(status),
        Err(crate::drivers::jobs::Failure::Cancelled) => Err(mac_abi::OPERATION_CANCELLED)
    }
}

/// Asks the installed provider, if there is one, what has happened since the
/// last call, and hands each event to `emit` as a kind, a code and a value.
pub(crate) fn poll(mut emit: impl FnMut(u16, u16, u32))
{
    let now = super::display::ticks();
    let mut provider = PROVIDER.load(Ordering::Acquire);
    if provider == 0
    {
        if now.wrapping_sub(LAST_SEARCH.load(Ordering::Relaxed)) < SEARCH_INTERVAL
           && LAST_SEARCH.load(Ordering::Relaxed) != 0
        {
            return;
        }
        LAST_SEARCH.store(now.max(1), Ordering::Relaxed);
        match crate::drivers::find_kernel_provider(CLASS, VERSION, 0)
        {
            Ok(found) =>
            {
                PROVIDER.store(found, Ordering::Release);
                serial!("INPUT_PROVIDER_READY provider={found}");
                provider = found;
            }
            Err(_) => return
        }
    }
    if now.wrapping_sub(LAST_POLL.load(Ordering::Relaxed)) < POLL_INTERVAL
    {
        return;
    }
    LAST_POLL.store(now, Ordering::Relaxed);
    match call(provider)
    {
        Ok(bytes) =>
        {
            for event in bytes.chunks_exact(EVENT_BYTES).filter_map(Event::decode)
            {
                emit(event.kind, event.code, event.value);
            }
        }
        // Busy is another turn's answer still in flight, not a fault.
        Err(mac_abi::SERVICE_BUSY) => (),
        Err(status) =>
        {
            // Said once: a provider that keeps refusing would otherwise fill
            // the console at the rate System polls it.
            if !REPORTED.swap(true, Ordering::Relaxed)
            {
                serial!("INPUT_PROVIDER_FAILED status={status}");
            }
        }
    }
}
