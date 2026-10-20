//! Read-only QMP observations replace per-input UART synchronization in tests.
//! The desktop service boundary serializes writers; atomics and a sequence lock
//! make paused snapshots explicit and suitable for future concurrent readers.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicU64, Ordering};
static STATE: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];

pub fn address() -> usize
{
    STATE.as_ptr() as usize
}

pub fn publish(values: [u64; 15])
{
    STATE[0].fetch_add(1, Ordering::AcqRel);
    for (field, value) in STATE[1..].iter().zip(values)
    {
        field.store(value, Ordering::Relaxed);
    }
    STATE[0].fetch_add(1, Ordering::Release);
}
