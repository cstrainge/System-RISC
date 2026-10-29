//! Actual native CPU selected-state residency, independent of installed policy.
//! A row-local guard makes remote observations coherent. The owning CPU
//! locks only at idle/executing transitions. Transition time is sampled AFTER
//! that guard: an earlier capture tick cannot retroactively shorten a remote
//! reader's already-projected residency.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicU8, Ordering};

use mac_abi::inspection::cpus as wire;
use spinning_top::Spinlock;

const UNKNOWN: u8 = 0;
const IDLE: u8 = 1;
const EXECUTING: u8 = 2;
#[derive(Clone, Copy)]
struct Values
{
    mode:      u8,
    since:     u64,
    idle:      u64,
    executing: u64,
    observed:  u64,
    failure:   u32
}
impl Values
{
    fn totals(&self, at: u64) -> Result<(u64, u64), u32>
    {
        if at < self.observed
        {
            return Err(wire::ACTIVITY_CLOCK_REGRESSED);
        }
        let elapsed = at.checked_sub(self.since).ok_or(wire::ACTIVITY_CLOCK_REGRESSED)?;
        let mut idle = self.idle;
        let mut executing = self.executing;
        if self.mode == IDLE
        {
            idle = idle.checked_add(elapsed).ok_or(wire::ACTIVITY_OVERFLOW)?;
        }
        else
        {
            executing = executing.checked_add(elapsed).ok_or(wire::ACTIVITY_OVERFLOW)?;
        }
        // Total residency is also a bounded u64 counter, not a wrapped sum.
        idle.checked_add(executing).ok_or(wire::ACTIVITY_OVERFLOW)?;
        Ok((idle, executing))
    }
}
pub(super) struct Counter
{
    mode:   AtomicU8,
    values: Spinlock<Values>
}
impl Counter
{
    pub(super) const fn new() -> Self
    {
        Self { mode:   AtomicU8::new(UNKNOWN),
               values: Spinlock::new(Values { mode:      UNKNOWN,
                                              since:     0,
                                              idle:      0,
                                              executing: 0,
                                              observed:  0,
                                              failure:   0 }) }
    }

    pub(super) fn enter(&self)
    {
        self.transition(EXECUTING);
    }

    pub(super) fn resume(&self, idle: bool)
    {
        let next = if idle { IDLE } else { EXECUTING };
        self.transition(next);
    }

    fn transition(&self, next: u8)
    {
        let current = self.mode.load(Ordering::Relaxed);
        if current == next
        {
            return;
        }
        let mut values = self.values.lock();
        if values.mode == next
        {
            return;
        }
        // A reader may have projected the prior mode after the native outer
        // capture. Sample only after serializing with that reader; using the
        // earlier capture tick could otherwise make a counter decrease.
        let at = super::residency_ticks();
        if values.mode != UNKNOWN && values.failure == 0
        {
            match values.totals(at)
            {
                Ok((idle, executing)) =>
                {
                    values.idle = idle;
                    values.executing = executing;
                }
                Err(cause) =>
                {
                    values.failure = cause;
                    values.mode = next;
                    self.mode.store(next, Ordering::Release);
                    return;
                }
            }
        }
        values.since = at;
        values.observed = at;
        values.mode = next;
        self.mode.store(next, Ordering::Release);
    }

    fn observe(&self, row: &mut wire::Row)
    {
        let mut values = self.values.lock();
        if values.mode == UNKNOWN
        {
            row.activity = wire::ACTIVITY_UNINITIALIZED;
            return;
        }
        row.state = if values.mode == IDLE { wire::IDLE } else { wire::EXECUTING };
        if values.failure != 0
        {
            row.activity = values.failure;
            return;
        }
        // Read time AFTER taking this row's guard: a concurrent owner cannot
        // advance since beyond a timestamp sampled before we acquired it.
        let now = super::residency_ticks();
        let totals = values.totals(now);
        if totals.is_ok()
        {
            values.observed = now;
        }
        match totals
        {
            Ok((idle, executing)) =>
            {
                row.idle_ticks = idle;
                row.executing_ticks = executing;
                row.sampled_at = now;
                row.activity = wire::ACTIVITY_RESIDENCY;
                row.state = if values.mode == IDLE { wire::IDLE } else { wire::EXECUTING };
            }
            Err(cause) =>
            {
                values.failure = cause;
                row.activity = cause;
            }
        }
    }
}
pub(crate) enum ReadError
{
    Index,
    Binding
}

pub(crate) fn summary() -> Option<wire::Summary>
{
    let cpus = crate::machine::paging::enabled_cpus()?;
    let first = cpus.first()?;
    let (ticks_per_second, _) = crate::machine::monitor::cpu_observation(0, first.hart)?;
    (ticks_per_second != 0).then_some(wire::Summary { count: cpus.len() as u64,
                                                      ticks_per_second })
}

pub(crate) fn row(index: usize) -> Result<wire::Row, ReadError>
{
    let cpus = crate::machine::paging::enabled_cpus().ok_or(ReadError::Binding)?;
    let cpu = cpus.get(index).ok_or(ReadError::Index)?;
    let monitor =
        || crate::machine::monitor::cpu_observation(index, cpu.hart).ok_or(ReadError::Binding);
    let (_, stopped) = monitor()?;
    let mut result = wire::Row { hart: cpu.hart,
                                 ..wire::Row::default() };
    if let Some(registry) = super::REGISTRY.get()
    {
        if registry.rows.as_slice().len() != cpus.len()
        {
            return Err(ReadError::Binding);
        }
        let row = &registry.rows.as_slice()[index];
        if row.local.id as u64 != cpu.hart
        {
            return Err(ReadError::Binding);
        }
        if registry.primary == index
        {
            result.flags |= wire::PRIMARY;
        }
        let phase = row.phase.load(Ordering::Acquire);
        if matches!(phase, super::ADMITTED_IDLE | super::DISPATCH_ELIGIBLE)
        {
            result.flags |= wire::IS_ADMITTED;
            result.state = wire::ADMITTED;
            if phase == super::DISPATCH_ELIGIBLE && !stopped
            {
                result.flags |= wire::DISPATCH_ELIGIBLE;
            }
            if !stopped
            {
                row.activity.observe(&mut result);
            }
        }
        else if matches!(phase, super::RELEASING | super::STARTING)
        {
            result.state = wire::ADMITTING;
        }
    }
    // Stop has no timestamp in the current M wire. Never extrapolate residency
    // beyond an unknown stop instant. Check again after the counter snapshot.
    if stopped || monitor()?.1
    {
        result.state = wire::STOPPED;
        result.flags &= !wire::DISPATCH_ELIGIBLE;
        result.activity = wire::ACTIVITY_STOPPED;
        result.idle_ticks = 0;
        result.executing_ticks = 0;
        result.sampled_at = 0;
    }
    Ok(result)
}
