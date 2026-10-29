//! Public CPU enumeration. Membership comes from immutable discovered topology,
//! not an installed scheduler. Activity is CPU residency, not domain CPU time.
use mac_abi::inspection::cpus as wire;
use mac_traps::{Client, Error, types::InlineText};

use super::{Failure, Inspection, Result};

/// Hardware admission/selected-state observation. Executing includes resident
/// maintenance; idle means its canonical System idle was selected, not a claim
/// of cycle-exact WFI or absence of M-mode work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuState
{
    Discovered,
    Admitting,
    Admitted,
    Idle,
    Executing,
    Stopped
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable
{
    Uninitialized,
    Stopped,
    ClockRegressed,
    Overflow
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity
{
    Unavailable(Unavailable),
    Residency(Residency)
}
/// Elapsed DT-clock ticks at serialized outer-capture/restore-commit observations.
/// Row-lock contention can delay that observation; time is never backdated.
/// Clock ordering fences have real, currently unmeasured observer cost.
/// Selected-idle residency includes its fixed restore/save tail; it is not
/// cycle-accurate WFI time. M work and virtualization pauses are not separated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Residency
{
    pub idle_ticks:      u64,
    pub executing_ticks: u64,
    pub sampled_at:      u64
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActivityDelta
{
    pub idle_ticks:      u64,
    pub executing_ticks: u64
}
impl ActivityDelta
{
    /// CPU residency fraction in this interval; not application utilization.
    pub fn executing_fraction(self) -> Option<f64>
    {
        let total = self.idle_ticks.checked_add(self.executing_ticks)?;
        (total != 0).then(|| self.executing_ticks as f64 / total as f64)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cpu
{
    pub hardware_id:       u64,
    pub state:             CpuState,
    pub primary:           bool,
    pub admitted:          bool,
    pub dispatch_eligible: bool,
    pub activity:          Activity
}
impl Cpu
{
    /// None if unavailable, different CPUs, no elapsed interval, or counters
    /// are inconsistent. Compare samples from the same running System only;
    /// hardware labels are not boot IDs. Rows need not be simultaneous.
    pub fn activity_since(self, before: Self) -> Option<ActivityDelta>
    {
        if self.hardware_id != before.hardware_id
        {
            return None;
        }
        let (Activity::Residency(now), Activity::Residency(old)) = (self.activity, before.activity)
        else
        {
            return None;
        };
        let elapsed = now.sampled_at.checked_sub(old.sampled_at)?;
        let idle_ticks = now.idle_ticks.checked_sub(old.idle_ticks)?;
        let executing_ticks = now.executing_ticks.checked_sub(old.executing_ticks)?;
        (elapsed != 0 && idle_ticks.checked_add(executing_ticks)? == elapsed)
            .then_some(ActivityDelta { idle_ticks, executing_ticks })
    }
}
fn invalid(text: &str) -> Failure
{
    let mut detail = [0; mac_abi::inspection::DETAIL_BYTES];
    detail[..text.len()].copy_from_slice(text.as_bytes());
    Failure { cause:  Error::BadAbi,
              detail: InlineText::from_reply(detail, text.len() as u32).unwrap_or_default() }
}
fn request(client: Client<'_>, call: &mut wire::Call) -> Result<()>
{
    client.inspect_cpus(call).map_err(|cause| {
                                 Failure {
        cause, detail: InlineText::from_reply(call.detail, call.detail_length).unwrap_or_default()
    }
                             })
}
#[derive(Clone, Copy)]
pub struct Cpus<'a>
{
    client:           Client<'a>,
    count:            u64,
    ticks_per_second: u64
}
impl<'a> Inspection<'a>
{
    /// Available to ordinary live applications without an inspection grant.
    /// This reveals CPU labels and aggregate residency, never another thread.
    pub fn cpus(self) -> Result<Cpus<'a>>
    {
        let mut call = wire::Call::default();
        request(self.client, &mut call)?;
        if call.summary.count == 0 || call.summary.ticks_per_second == 0
        {
            return Err(invalid("invalid CPU topology or clock reply; rebuild System and app \
                                together"));
        }
        Ok(Cpus { client:           self.client,
                  count:            call.summary.count,
                  ticks_per_second: call.summary.ticks_per_second })
    }
}
impl Cpus<'_>
{
    /// Total discovered enabled CPUs, including CPUs without dispatch grants.
    pub fn len(self) -> u64
    {
        self.count
    }

    pub fn is_empty(self) -> bool
    {
        self.count == 0
    }

    pub fn ticks_per_second(self) -> u64
    {
        self.ticks_per_second
    }

    /// Ordinal position in immutable discovered topology, not a hardware ID.
    pub fn at(self, index: u64) -> Result<Cpu>
    {
        let mut call = wire::Call { operation: wire::READ,
                                    index,
                                    ..wire::Call::default() };
        request(self.client, &mut call)?;
        let r = call.row;
        if call.summary.count != self.count
           || call.summary.ticks_per_second != self.ticks_per_second
           || r.reserved != 0
           || r.flags & !(wire::PRIMARY | wire::IS_ADMITTED | wire::DISPATCH_ELIGIBLE) != 0
        {
            return Err(invalid("invalid CPU row metadata; rebuild System and app together"));
        }
        let state = match r.state
        {
            wire::DISCOVERED => CpuState::Discovered,
            wire::ADMITTING => CpuState::Admitting,
            wire::ADMITTED => CpuState::Admitted,
            wire::IDLE => CpuState::Idle,
            wire::EXECUTING => CpuState::Executing,
            wire::STOPPED => CpuState::Stopped,
            _ => return Err(invalid("invalid CPU admission state"))
        };
        let activity = match r.activity
        {
            wire::ACTIVITY_RESIDENCY =>
            {
                if r.idle_ticks
                    .checked_add(r.executing_ticks)
                    .is_none_or(|total| total > r.sampled_at)
                   || !matches!(state, CpuState::Idle | CpuState::Executing)
                   || r.flags & wire::IS_ADMITTED == 0
                {
                    return Err(invalid("invalid CPU residency counters"));
                }
                Activity::Residency(Residency { idle_ticks:      r.idle_ticks,
                                                executing_ticks: r.executing_ticks,
                                                sampled_at:      r.sampled_at })
            }
            value =>
            {
                if r.idle_ticks != 0 || r.executing_ticks != 0 || r.sampled_at != 0
                {
                    return Err(invalid("unavailable CPU activity returned invented counters"));
                }
                Activity::Unavailable(match value
                                      {
                                          wire::ACTIVITY_UNINITIALIZED =>
                                          {
                                              Unavailable::Uninitialized
                                          }
                                          wire::ACTIVITY_STOPPED => Unavailable::Stopped,
                                          wire::ACTIVITY_CLOCK_REGRESSED =>
                                          {
                                              Unavailable::ClockRegressed
                                          }
                                          wire::ACTIVITY_OVERFLOW => Unavailable::Overflow,
                                          _ =>
                                          {
                                              return Err(invalid("invalid CPU activity \
                                                                  availability"));
                                          }
                                      })
            }
        };
        let admitted = r.flags & wire::IS_ADMITTED != 0;
        let dispatch_eligible = r.flags & wire::DISPATCH_ELIGIBLE != 0;
        if dispatch_eligible && (!admitted || state == CpuState::Stopped)
        {
            return Err(invalid("invalid CPU dispatch eligibility"));
        }
        Ok(Cpu { hardware_id: r.hart,
                 state,
                 primary: r.flags & wire::PRIMARY != 0,
                 admitted,
                 dispatch_eligible,
                 activity })
    }
}
