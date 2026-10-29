//! Allocation-free scheduler policy. Core supplies actual eligible rows and
//! time facts, enforces affinity/claims and commits a validated decision.
#![no_std]
#![forbid(unsafe_code)]
use core::time::Duration;

use mac_scheduler_interface::{Candidate, Decision, Error, Kind, Result, Round};

/// One priority boost per20ms continuously runnable, capped at the top band.
/// These are policy constants, not measured latency or starvation guarantees.
pub const AGING_MICROSECONDS: u64 = 20_000;
pub struct PriorityBands;
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Rank
{
    band:           u8,
    // Older runnable intervals outrank newer intervals within one band.
    age:            u64,
    // Prefer cache locality and stable identity only after runnable age.
    local:          bool,
    inverse_thread: u64
}
impl PriorityBands
{
    pub fn choose(view: Round<'_>) -> Result<Decision>
    {
        if view.is_empty()
        {
            return view.idle();
        }
        let context = view.context();
        let mut best: Option<(Candidate<'_>, Rank)> = None;
        for index in 0..view.len()
        {
            let candidate = view.candidate(index).ok_or(Error::Invalid)?;
            let record = candidate.record()?;
            let age = context.now_microseconds
                             .checked_sub(record.ready_since_microseconds)
                             .ok_or(Error::Invalid)?;
            let base = if record.interactive_wake
            {
                2
            }
            else
            {
                match record.kind
                {
                    Kind::System | Kind::Application => 1,
                    Kind::Module => 0
                }
            };
            let band = (base + (age / AGING_MICROSECONDS).min(3) as u8).min(3);
            let rank = Rank { band,
                              age,
                              local: record.last_hart == Some(context.hart),
                              inverse_thread: u64::MAX - record.thread };
            if best.is_none_or(|(_, previous)| rank > previous)
            {
                best = Some((candidate, rank));
            }
        }
        let (candidate, rank) = best.ok_or(Error::Ineligible)?;
        let preferred = match rank.band
        {
            2 | 3 => 2_000,
            1 => 4_000,
            _ => 8_000
        };
        let quantum = preferred.clamp(context.minimum_quantum_microseconds,
                                      context.maximum_quantum_microseconds);
        candidate.select(Duration::from_micros(u64::from(quantum)))
    }
}
