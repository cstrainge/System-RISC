//! A single bounded inventory search, including pre-dispatch contention. Its
//! wall-clock deadline is never renewed by retries or new input events.
#![forbid(unsafe_code)]
use crate::Diagnostic;
pub const DISCOVERY_MICROSECONDS: u64 = 5_000_000;
const RETRY_MICROSECONDS: u64 = 1_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step
{
    Probe(u32),
    Wait,
    Exhausted,
    Refused(Diagnostic)
}
pub struct Discovery
{
    count:        u32,
    next:         u32,
    started:      Option<u64>,
    retry_at:     u64,
    retry_claims: bool,
    saw_busy:     bool,
    identified:   bool
}
impl Discovery
{
    pub const fn new(count: u32) -> Self
    {
        Self { count,
               next: 0,
               started: None,
               retry_at: 0,
               retry_claims: false,
               saw_busy: false,
               identified: false }
    }

    pub fn expired(&self, now: u64) -> bool
    {
        !self.identified
        && self.started.is_some_and(|start| now.saturating_sub(start) >= DISCOVERY_MICROSECONDS)
    }

    pub fn timeout_diagnostic(&self) -> Diagnostic
    {
        if self.saw_busy { Diagnostic::Claimed } else { Diagnostic::DiscoveryTimeout }
    }

    pub fn step(&mut self, now: u64) -> Step
    {
        self.started.get_or_insert(now);
        if self.expired(now)
        {
            return Step::Refused(self.timeout_diagnostic());
        }
        if now < self.retry_at
        {
            return Step::Wait;
        }
        if self.next < self.count
        {
            return Step::Probe(self.next);
        }
        if self.retry_claims
        {
            self.next = 0;
            self.retry_claims = false;
            self.retry_at = now.saturating_add(RETRY_MICROSECONDS);
            Step::Wait
        }
        else
        {
            Step::Exhausted
        }
    }

    /// Only a completed probe (including NOT_FOUND) consumes an ordinal.
    pub fn probed(&mut self)
    {
        if self.next < self.count
        {
            self.next += 1;
        }
    }

    pub fn probe_busy(&mut self, now: u64)
    {
        self.saw_busy = true;
        self.retry_at = now.saturating_add(RETRY_MICROSECONDS);
    }

    pub fn claim_busy(&mut self)
    {
        self.saw_busy = true;
        self.retry_claims = true;
    }

    pub fn identified(&mut self)
    {
        self.identified = true;
    }

    /// A disconnected connection receives a new budget only after real reset,
    /// unmap and release acknowledgement. Ordinary discovery never calls this.
    pub fn reconnect(&mut self, ordinal: u32)
    {
        self.next = ordinal;
        self.started = None;
        self.retry_at = 0;
        self.retry_claims = false;
        self.saw_busy = false;
        self.identified = false;
    }
}
