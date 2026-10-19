//! Opt-in, bounded in-memory diagnostics. No per-event serial I/O or SDK ABI.
//! QEMU reads a paused snapshot at the address printed by `initialize`.
#![forbid(unsafe_code)]
#[cfg(feature = "event-trace")]
mod enabled
{
    use spinning_top::Spinlock;
    const APPS: usize = 16;
    const SERVICES: usize = 32;
    const RECORDS: usize = 8192;
    #[repr(C)]
    struct Trace
    {
        header:   [u64; 6],
        services: [[[u64; 4]; SERVICES]; APPS],
        counters: [[u64; 16]; APPS],
        records:  [[u64; 8]; RECORDS]
    }
    static TRACE: Spinlock<Trace> = Spinlock::new(Trace { header:   [0x5253435954524143, 1, 0,
                                                                     RECORDS as u64, APPS as u64,
                                                                     SERVICES as u64],
                                                          services: [[[0; 4]; SERVICES]; APPS],
                                                          counters: [[0; 16]; APPS],
                                                          records:  [[0; 8]; RECORDS] });
    pub fn initialize()
    {
        let data = TRACE.lock();
        serial!("EVENT_TRACE address={} bytes={} version=1",
                core::ptr::from_ref(&*data) as usize,
                core::mem::size_of::<Trace>());
    }
    pub fn record(code: u64, app: u64, a: u64, b: u64, c: u64, d: u64)
    {
        let mut data = TRACE.lock();
        data.counters[app.min((APPS - 1) as u64) as usize][code as usize] += 1;
        if matches!(code, super::NULL | super::POLL | super::PRESENT)
        {
            return;
        }
        let seq = data.header[2];
        data.records[seq as usize % RECORDS] =
            [seq, crate::platform::ticks(), code, app, a, b, c, d];
        data.header[2] = seq + 1;
    }
    pub fn service(app: u64, number: u32, depth: usize, elapsed: u64)
    {
        let mut data = TRACE.lock();
        let row = &mut data.services[app.min((APPS - 1) as u64) as usize][(number & 31) as usize];
        let index = if depth == 1 { 0 } else { 2 };
        row[index] += 1;
        row[index + 1] += elapsed;
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
