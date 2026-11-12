//! The last few native services that refused an application.
//!
//! An application that reports a failure names what it was doing, and a status
//! a dozen services share. This keeps the trap number beside each refusal, and
//! prints the recent ones when an application's own log line reports a failure,
//! so the console shows which service said no immediately before the report.
//!
//! Refusals are routine - a lookup that finds nothing is one - so nothing is
//! printed until an application says something went wrong.
use core::sync::atomic::{AtomicU64, Ordering};

const SLOTS: usize = 16;

/// One refusal packed as sequence (24 bits), trap (24 bits) and the status's
/// low 16 bits; the application is kept beside it.
static ENTRIES: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
static APPS: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
static NEXT: AtomicU64 = AtomicU64::new(1);

pub(crate) fn record(app: u64, trap: u32, status: i64)
{
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let slot = sequence as usize % SLOTS;
    APPS[slot].store(app, Ordering::Relaxed);
    ENTRIES[slot].store((sequence & 0xff_ffff) << 40
                        | u64::from(trap & 0xff_ffff) << 16
                        | (status as u64 & 0xffff),
                        Ordering::Relaxed);
}

/// Prints the recent refusals, oldest first, when `line` is an application
/// reporting a failure.
pub(crate) fn report_for(line: &str)
{
    if !line.contains("_FAILED status=")
    {
        return;
    }
    let newest = NEXT.load(Ordering::Relaxed);
    for sequence in newest.saturating_sub(SLOTS as u64).max(1)..newest
    {
        let slot = sequence as usize % SLOTS;
        let entry = ENTRIES[slot].load(Ordering::Relaxed);
        if entry >> 40 != sequence & 0xff_ffff
        {
            continue;
        }
        serial!("SERVICE_REFUSED sequence={} app={} trap={:#x} status={}",
                sequence,
                APPS[slot].load(Ordering::Relaxed),
                (entry >> 16) & 0xff_ffff,
                entry as u16 as i16);
    }
}
