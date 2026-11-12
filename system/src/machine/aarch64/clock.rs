//! Wall-clock source for a board that has none.
//!
//! This SoC carries a free-running counter, which is monotonic and has no
//! relation to calendar time, and no battery-backed real-time clock.
//!
//! A Macintosh whose clock battery has died does not refuse to save files: it
//! wakes up at a fixed, plainly wrong date and counts from there. This does the
//! same. The origin is 27 August 1956, the date that machine shows, so a stamp
//! written before anything sets the clock is recognisable as "the clock was
//! never set" rather than passing for a real one.
//!
//! When something can supply an epoch - a firmware-passed value, a network time
//! source, a real-time clock driver - it stores the origin here and the counter
//! keeps supplying elapsed time, so the clock becomes real without any caller
//! changing.

use core::sync::atomic::{AtomicI64, Ordering};

use mac_abi::{Status, dates::Timestamp};

/// 27 August 1956 00:00:00 in Macintosh seconds (since 1 January 1904).
const UNSET_MAC_SECONDS: i64 = 1_661_644_800;

/// Unix seconds at counter value zero.
static ORIGIN: AtomicI64 = AtomicI64::new(UNSET_MAC_SECONDS - Timestamp::MAC_TO_UNIX_SECONDS);

/// Calendar time: the stored origin plus time since the counter started.
pub(crate) fn now() -> Result<Timestamp, Status>
{
    let (count, hz): (u64, u64);
    // SAFETY: both registers are readable at EL1 and reading them has no effect.
    unsafe {
        core::arch::asm!("mrs {}, cntvct_el0", out(reg) count, options(nomem, nostack));
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) hz, options(nomem, nostack));
    }
    if hz == 0
    {
        return Err(mac_abi::NOT_SUPPORTED);
    }
    let seconds = i64::try_from(count / hz).map_err(|_| mac_abi::BAD_ARGUMENT)?;
    let micros = ((count % hz) as u128 * 1_000_000 / hz as u128) as u32;
    Timestamp::from_unix_parts(ORIGIN.load(Ordering::Relaxed).saturating_add(seconds), micros)
        .ok_or(mac_abi::BAD_ARGUMENT)
}
