//! Native Time Manager tasks backed by canonical, owner-scoped OS objects.
//! Callbacks remain userspace; legacy A-line/68k callback adaptation is separate.
use mac_abi::timers as wire;

use crate::{Error, Events, Result};
pub struct TimeTask<'a>
{
    client: mac_traps::Client<'a>,
    id:     Option<u64>,
    resume: u64,
    anchor: u64
}
impl<'a> TimeTask<'a>
{
    pub fn new(events: Events<'a>) -> Self
    {
        Self { client: events.client,
               id:     None,
               resume: 0,
               anchor: 0 }
    }

    pub fn insert(&mut self) -> Result<()>
    {
        self.install(false)
    }

    pub fn insert_extended(&mut self) -> Result<()>
    {
        self.install(true)
    }

    fn install(&mut self, extended: bool) -> Result<()>
    {
        if self.id.is_some()
        {
            return Err(Error::BadArgument);
        }
        let mut call = wire::Call { operation: wire::INSERT,
                                    extended: u32::from(extended),
                                    deadline: if extended { self.resume } else { 0 },
                                    anchor: if extended { self.anchor } else { 0 },
                                    ..Default::default() };
        self.client.timers(&mut call)?;
        self.id = Some(call.id);
        Ok(())
    }

    /// Positive delays mean milliseconds; negative delays mean microseconds.
    /// A still-active task must be removed/reinserted before rescheduling.
    pub fn prime(&mut self, delay: i32) -> Result<u64>
    {
        let mut call = wire::Call { operation: wire::PRIME,
                                    id: self.id.ok_or(Error::BadArgument)?,
                                    delay,
                                    ..Default::default() };
        self.client.timers(&mut call)?;
        Ok(call.deadline)
    }

    /// Retimes a native event-loop timer without destroying/reallocating its
    /// owner-ledger object. Existing observers follow the revised deadline.
    /// Unlike classic PrimeTime this explicitly restarts an active schedule.
    pub fn reset(&mut self, delay: i32) -> Result<u64>
    {
        let mut call = wire::Call { operation: wire::RESET,
                                    id: self.id.ok_or(Error::BadArgument)?,
                                    delay,
                                    ..Default::default() };
        self.client.timers(&mut call)?;
        Ok(call.deadline)
    }

    pub fn source(&self) -> Option<mac_abi::readiness::Source>
    {
        self.id.map(|id| {
                   mac_abi::readiness::Source { kind: mac_abi::readiness::TIMER,
                                                id,
                                                ..Default::default() }
               })
    }

    /// Acknowledge one expiration before re-priming. The OS retains the extended
    /// task's original scheduled anchor rather than the callback delivery time.
    pub fn fire(&mut self) -> Result<bool>
    {
        let mut call = wire::Call { operation: wire::FIRE,
                                    id: self.id.ok_or(Error::BadArgument)?,
                                    ..Default::default() };
        self.client.timers(&mut call)?;
        Ok(call.state != 0)
    }

    /// Invalidates the task identity and wakes outstanding observers. Return the
    /// remaining nanoseconds for an active task; an inactive task returns None.
    pub fn remove(&mut self) -> Result<Option<u64>>
    {
        let mut call = wire::Call { operation: wire::REMOVE,
                                    id: self.id.ok_or(Error::BadArgument)?,
                                    ..Default::default() };
        self.client.timers(&mut call)?;
        self.id = None;
        self.resume = call.deadline;
        self.anchor = call.anchor;
        Ok((call.state != 0).then_some(call.remaining))
    }
}
impl Drop for TimeTask<'_>
{
    fn drop(&mut self)
    {
        if self.id.is_some()
        {
            let _ = self.remove();
        }
    }
}
