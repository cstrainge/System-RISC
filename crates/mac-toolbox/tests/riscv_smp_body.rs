//! Native register invariants for uniquely named, normally scheduled bodies.
//! The host may correlate actual PC/SP snapshots with their relocated symbols.
//! Completion here establishes neither CPU placement nor migration.
#![cfg(target_arch = "riscv64")]

#[repr(C)]
#[derive(Default)]
pub struct Capture
{
    pub code:     u64,
    pub observed: u64,
    pub vlenb:    u64
}
pub type Probe = unsafe extern "C" fn(u64, u64, usize, *mut Capture);

#[derive(Debug)]
pub struct Failure
{
    pub code:     u64,
    pub observed: u64,
    pub seed:     u64
}
impl core::fmt::Display for Failure
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        write!(out,
               "register-check={} observed={:#x} seed={:#x}",
               self.code, self.observed, self.seed)
    }
}

/// # Safety
/// `body` must be an instance of the adjacent SMP_BODY assembly macro. It owns
/// no resources, preserves the LP64 callee state and writes this exact Capture.
pub unsafe fn hold(body: Probe, ticks: u64, seed: u64) -> Result<u64, Failure>
{
    let mut result = Capture::default();
    // SAFETY: caller selects a real macro instance; output is exclusive/live.
    unsafe { body(ticks, seed, 0, &mut result) };
    if result.code == 0
    {
        Ok(result.vlenb)
    }
    else
    {
        Err(Failure { code: result.code,
                      observed: result.observed,
                      seed })
    }
}
