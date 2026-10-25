//! Opt-in, bounded in-memory diagnostics. No per-event serial I/O or SDK ABI.
//! QEMU reads a paused snapshot at the address printed by `initialize`.
#![forbid(unsafe_code)]
#[cfg(feature = "event-trace")]
mod enabled
{
    use alloc::vec::Vec;

    use spinning_top::Spinlock;
    const APPS: usize = 16;
    const SERVICES: usize = 64;
    const RECORDS: usize = 8192;
    const APP_OFFSET: usize = 6;
    const SERVICE_OFFSET: usize = APP_OFFSET + APPS;
    const COUNTER_OFFSET: usize = SERVICE_OFFSET + APPS * SERVICES * 4;
    const RECORD_OFFSET: usize = COUNTER_OFFSET + APPS * 16;
    const WORDS: usize = RECORD_OFFSET + RECORDS * 8;
    // Preserve the contiguous version2 capture wire layout without reserving
    // another half MiB outside the resident heap, or constructing it on a stack.
    struct Trace
    {
        words: Vec<u64>
    }
    static TRACE: Spinlock<Option<Trace>> = Spinlock::new(None);
    pub fn initialize()
    {
        let mut words = Vec::new();
        if words.try_reserve_exact(WORDS).is_err()
        {
            serial!("EVENT_TRACE_UNAVAILABLE: diagnostic allocation failed");
            return;
        }
        words.resize(WORDS, 0);
        words[..APP_OFFSET].copy_from_slice(&[0x5253435954524143, 2, 0, RECORDS as u64,
                                              APPS as u64, SERVICES as u64]);
        let data = Trace { words };
        let address = data.words.as_ptr() as usize;
        *TRACE.lock() = Some(data);
        serial!("EVENT_TRACE address={} bytes={} version=2", address, WORDS * 8);
    }
    // Full generation-tagged identities, cumulative counters and the original
    // 8192-record ring remain intact. Exhausting the map never merges owners.
    fn slot(data: &mut Trace, app: u64) -> Option<usize>
    {
        if app == 0
        {
            return Some(0);
        }
        let apps = &mut data.words[APP_OFFSET..SERVICE_OFFSET];
        if let Some(index) = apps.iter().position(|id| *id == app)
        {
            return Some(index);
        }
        let index = apps.iter().enumerate().skip(1).find(|(_, id)| **id == 0)?.0;
        apps[index] = app;
        Some(index)
    }
    pub fn record(code: u64, app: u64, a: u64, b: u64, c: u64, d: u64)
    {
        let mut guard = TRACE.lock();
        let Some(data) = guard.as_mut()
        else
        {
            return;
        };
        if let Some(index) = slot(data, app)
        {
            data.words[COUNTER_OFFSET + index * 16 + code as usize] += 1;
        }
        if matches!(code, super::NULL | super::POLL | super::PRESENT)
        {
            return;
        }
        let seq = data.words[2];
        let at = RECORD_OFFSET + (seq as usize % RECORDS) * 8;
        data.words[at..at + 8].copy_from_slice(&[seq,
                                                 crate::platform::ticks(),
                                                 code,
                                                 app,
                                                 a,
                                                 b,
                                                 c,
                                                 d]);
        data.words[2] = seq + 1;
    }
    pub fn service(app: u64, number: u32, depth: usize, elapsed: u64)
    {
        let mut guard = TRACE.lock();
        let Some(data) = guard.as_mut()
        else
        {
            return;
        };
        let Some(slot) = slot(data, app)
        else
        {
            return;
        };
        let at = SERVICE_OFFSET
                 + (slot * SERVICES + (number as usize & (SERVICES - 1))) * 4
                 + if depth == 1 { 0 } else { 2 };
        data.words[at] += 1;
        data.words[at + 1] += elapsed;
    }
}
#[cfg(feature = "event-trace")]
pub use enabled::*;
#[cfg(not(feature = "event-trace"))]
#[inline]
pub fn initialize()
{
}
#[cfg(not(feature = "event-trace"))]
#[inline]
pub fn record(_: u64, _: u64, _: u64, _: u64, _: u64, _: u64)
{
}

#[inline]
pub fn clock() -> u64
{
    #[cfg(feature = "event-trace")]
    {
        crate::platform::ticks()
    }
    #[cfg(not(feature = "event-trace"))]
    {
        0
    }
}

/// TextEdit: a=0 rewrap,1 snapshot,2 draw,3 XOR caret; b/c/d are work metrics.
pub const TEXTEDIT: u64 = 0;
pub const INPUT: u64 = 1;
pub const DELIVERY: u64 = 2;
pub const PEEK: u64 = 3;
pub const SYNTH_UPDATE: u64 = 4;
pub const WINDOW: u64 = 5;
pub const ACTIVATE: u64 = 6;
pub const RENDER: u64 = 7;
pub const ROUTE: u64 = 8;
pub const NULL: u64 = 9;
pub const POLL: u64 = 10;
pub const PRESENT: u64 = 11;
pub const WAKE: u64 = 12;
pub const WAIT: u64 = 13;
pub const FRAME_COPY: u64 = 14;
#[cfg(feature = "event-trace")]
pub const SLOW_SERVICE: u64 = 15;

/// Diagnostic WINDOW operation reserved for frontbuffer drag-outline deltas.
#[cfg(target_arch = "riscv64")]
pub(crate) const WINDOW_OUTLINE_XOR: u64 = u64::MAX;
/// Content outline request: b=window (zero for desktop), c=four i16 edges,
/// d=visible. Correlates physical marquee input with feedback publication.
#[cfg(all(target_arch = "riscv64", feature = "event-trace"))]
pub(crate) const CONTENT_OUTLINE_REQUEST: u64 = u64::MAX - 1;
