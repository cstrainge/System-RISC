//! SCHD/SCTL v1. Wire labels are not authority. Core retains the actual module
//! execution/image/owner, calls outside its guards and revalidates the live
//! policy generation, eligible state and running claim before switching.
#![no_std]
#![forbid(unsafe_code)]
use core::time::Duration;

use mac_module::ClassId;
pub const POLICY_CLASS: ClassId = ClassId(*b"SCHD");
pub const CONTROL_CLASS: ClassId = ClassId(*b"SCTL");
pub const VERSION: u32 = 1;
pub const CHOOSE: u32 = 1;
pub const MAX_CANDIDATES: usize = 4096;
pub const ROUND_HEADER: usize = 64;
pub const CANDIDATE_BYTES: usize = 40;
pub const MAX_ROUND_BYTES: usize = ROUND_HEADER + MAX_CANDIDATES * CANDIDATE_BYTES;
pub const DECISION_BYTES: usize = 48;
pub mod control;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Truncated,
    Version,
    Limit,
    Invalid,
    WrongRound,
    Ineligible
}
pub type Result<T> = core::result::Result<T, Error>;
pub(crate) fn word<const N: usize>(b: &[u8], at: usize) -> Result<[u8; N]>
{
    b.get(at..at.checked_add(N).ok_or(Error::Limit)?)
     .ok_or(Error::Truncated)?
     .try_into()
     .map_err(|_| Error::Truncated)
}
pub(crate) fn u32_at(b: &[u8], at: usize) -> Result<u32>
{
    Ok(u32::from_le_bytes(word(b, at)?))
}
pub(crate) fn u64_at(b: &[u8], at: usize) -> Result<u64>
{
    Ok(u64::from_le_bytes(word(b, at)?))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Kind
{
    System      = 0,
    Application = 1,
    Module      = 2
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Cause
{
    Handoff = 0,
    Timer   = 1,
    Wake    = 2,
    Block   = 3,
    Exit    = 4,
    Idle    = 5
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context
{
    pub generation:                   u64,
    /// Checked per-hart decision sequence, not an object ID or broker ticket.
    pub round:                        u64,
    /// Actual hardware ID; sparse and full-width, never an array index.
    pub hart:                         u64,
    pub now_microseconds:             u64,
    pub previous_thread:              u64,
    pub cause:                        Cause,
    pub minimum_quantum_microseconds: u32,
    pub maximum_quantum_microseconds: u32
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record
{
    /// Core-issued full thread incarnation, distinct from an application ID.
    pub thread:                   u64,
    /// Actual transition into this runnable interval; resets after dispatch.
    pub ready_since_microseconds: u64,
    pub last_hart:                Option<u64>,
    pub running_microseconds:     u64,
    pub interactive_wake:         bool,
    pub kind:                     Kind
}
#[derive(Clone, Copy)]
pub struct Round<'a>
{
    context: Context,
    records: &'a [u8]
}
impl<'a> Round<'a>
{
    pub fn decode(bytes: &'a [u8]) -> Result<Self>
    {
        if bytes.get(..4) != Some(b"SCH1") || u32_at(bytes, 4)? != CANDIDATE_BYTES as u32
        {
            return Err(Error::Version);
        }
        let cause = match u32_at(bytes, 52)?
        {
            0 => Cause::Handoff,
            1 => Cause::Timer,
            2 => Cause::Wake,
            3 => Cause::Block,
            4 => Cause::Exit,
            5 => Cause::Idle,
            _ => return Err(Error::Invalid)
        };
        let context = Context { generation: u64_at(bytes, 8)?,
                                round: u64_at(bytes, 16)?,
                                hart: u64_at(bytes, 24)?,
                                now_microseconds: u64_at(bytes, 32)?,
                                previous_thread: u64_at(bytes, 40)?,
                                cause,
                                minimum_quantum_microseconds: u32_at(bytes, 56)?,
                                maximum_quantum_microseconds: u32_at(bytes, 60)? };
        validate_context(context)?;
        let count = u32_at(bytes, 48)? as usize;
        if count > MAX_CANDIDATES || bytes.len() != ROUND_HEADER + count * CANDIDATE_BYTES
        {
            return Err(Error::Limit);
        }
        let view = Self { context,
                          records: &bytes[ROUND_HEADER..] };
        for index in 0..count
        {
            view.record(index)?;
        }
        Ok(view)
    }

    pub fn context(self) -> Context
    {
        self.context
    }

    pub fn len(self) -> usize
    {
        self.records.len() / CANDIDATE_BYTES
    }

    pub fn is_empty(self) -> bool
    {
        self.records.is_empty()
    }

    fn record(self, index: usize) -> Result<Record>
    {
        let at = index.checked_mul(CANDIDATE_BYTES).ok_or(Error::Limit)?;
        let bytes = self.records.get(at..at + CANDIDATE_BYTES).ok_or(Error::Truncated)?;
        let flags = u32_at(bytes, 32)?;
        let kind = match u32_at(bytes, 36)?
        {
            0 => Kind::System,
            1 => Kind::Application,
            2 => Kind::Module,
            _ => return Err(Error::Invalid)
        };
        let thread = u64_at(bytes, 0)?;
        let ready_since_microseconds = u64_at(bytes, 8)?;
        let hart = u64_at(bytes, 16)?;
        if thread == 0
           || ready_since_microseconds > self.context.now_microseconds
           || flags & !3 != 0
           || flags & 2 == 0 && hart != 0
        {
            return Err(Error::Invalid);
        }
        Ok(Record { thread,
                    ready_since_microseconds,
                    last_hart: (flags & 2 != 0).then_some(hart),
                    running_microseconds: u64_at(bytes, 24)?,
                    interactive_wake: flags & 1 != 0,
                    kind })
    }

    /// Bounded indexed access to already validated immutable records. The core
    /// producer owns uniqueness and complete eligible-set construction.
    pub fn candidate(self, index: usize) -> Option<Candidate<'a>>
    {
        (index < self.len()).then_some(Candidate { view: self,
                                                   index })
    }

    pub fn idle(self) -> Result<Decision>
    {
        if !self.is_empty()
        {
            return Err(Error::Ineligible);
        }
        Ok(Decision { generation:           self.context.generation,
                      round:                self.context.round,
                      hart:                 self.context.hart,
                      thread:               0,
                      quantum_microseconds: 0 })
    }

    pub fn decode_decision(self, bytes: &[u8]) -> Result<Decision>
    {
        if bytes.len() != DECISION_BYTES
           || bytes.get(..4) != Some(b"SCD1")
           || u32_at(bytes, 4)? != VERSION
        {
            return Err(Error::Version);
        }
        let decision = Decision { generation:           u64_at(bytes, 8)?,
                                  round:                u64_at(bytes, 16)?,
                                  hart:                 u64_at(bytes, 24)?,
                                  thread:               u64_at(bytes, 32)?,
                                  quantum_microseconds: u64_at(bytes, 40)? };
        if decision.generation != self.context.generation
           || decision.round != self.context.round
           || decision.hart != self.context.hart
        {
            return Err(Error::WrongRound);
        }
        if decision.thread == 0
        {
            if !self.is_empty() || decision.quantum_microseconds != 0
            {
                return Err(Error::Ineligible);
            }
        }
        else
        {
            if decision.quantum_microseconds < u64::from(self.context.minimum_quantum_microseconds)
               || decision.quantum_microseconds
                  > u64::from(self.context.maximum_quantum_microseconds)
            {
                return Err(Error::Invalid);
            }
            let mut found = false;
            for index in 0..self.len()
            {
                found |= self.record(index)?.thread == decision.thread;
            }
            if !found
            {
                return Err(Error::Ineligible);
            }
        }
        Ok(decision)
    }
}
#[derive(Clone, Copy)]
pub struct Candidate<'a>
{
    view:  Round<'a>,
    index: usize
}
impl Candidate<'_>
{
    pub fn record(self) -> Result<Record>
    {
        self.view.record(self.index)
    }

    pub fn ready_for(self) -> Result<Duration>
    {
        Ok(Duration::from_micros(self.view.context.now_microseconds
                                 - self.record()?.ready_since_microseconds))
    }

    pub fn select(self, quantum: Duration) -> Result<Decision>
    {
        let micros = quantum.as_micros() + u128::from(quantum.subsec_nanos() % 1000 != 0);
        let micros = u64::try_from(micros).map_err(|_| Error::Limit)?;
        let context = self.view.context;
        if micros < u64::from(context.minimum_quantum_microseconds)
           || micros > u64::from(context.maximum_quantum_microseconds)
        {
            return Err(Error::Invalid);
        }
        Ok(Decision { generation:           context.generation,
                      round:                context.round,
                      hart:                 context.hart,
                      thread:               self.record()?.thread,
                      quantum_microseconds: micros })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision
{
    generation:           u64,
    round:                u64,
    hart:                 u64,
    thread:               u64,
    quantum_microseconds: u64
}
impl Decision
{
    pub fn generation(self) -> u64
    {
        self.generation
    }

    pub fn round(self) -> u64
    {
        self.round
    }

    pub fn hart(self) -> u64
    {
        self.hart
    }

    pub fn thread(self) -> Option<u64>
    {
        (self.thread != 0).then_some(self.thread)
    }

    pub fn quantum(self) -> Duration
    {
        Duration::from_micros(self.quantum_microseconds)
    }

    pub fn encode(self) -> [u8; DECISION_BYTES]
    {
        let mut b = [0; DECISION_BYTES];
        b[..4].copy_from_slice(b"SCD1");
        b[4..8].copy_from_slice(&VERSION.to_le_bytes());
        for (at, value) in [(8, self.generation),
                            (16, self.round),
                            (24, self.hart),
                            (32, self.thread),
                            (40, self.quantum_microseconds)]
        {
            b[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        b
    }
}
fn validate_context(c: Context) -> Result<()>
{
    if c.generation == 0
       || c.round == 0
       || c.minimum_quantum_microseconds == 0
       || c.maximum_quantum_microseconds < c.minimum_quantum_microseconds
    {
        Err(Error::Invalid)
    }
    else
    {
        Ok(())
    }
}
/// Core-side producer for a complete immutable decision snapshot. No truncation
/// or identity generation happens here; the caller supplies canonical records.
pub fn encode_round<'a>(c: Context, records: &[Record], output: &'a mut [u8]) -> Result<&'a [u8]>
{
    validate_context(c)?;
    if records.len() > MAX_CANDIDATES
    {
        return Err(Error::Limit);
    }
    let length = ROUND_HEADER + records.len() * CANDIDATE_BYTES;
    let b = output.get_mut(..length).ok_or(Error::Truncated)?;
    b.fill(0);
    b[..4].copy_from_slice(b"SCH1");
    b[4..8].copy_from_slice(&(CANDIDATE_BYTES as u32).to_le_bytes());
    for (at, value) in [(8, c.generation),
                        (16, c.round),
                        (24, c.hart),
                        (32, c.now_microseconds),
                        (40, c.previous_thread)]
    {
        b[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }
    b[48..52].copy_from_slice(&(records.len() as u32).to_le_bytes());
    b[52..56].copy_from_slice(&(c.cause as u32).to_le_bytes());
    b[56..60].copy_from_slice(&c.minimum_quantum_microseconds.to_le_bytes());
    b[60..64].copy_from_slice(&c.maximum_quantum_microseconds.to_le_bytes());
    for (index, r) in records.iter().enumerate()
    {
        if r.thread == 0 || r.ready_since_microseconds > c.now_microseconds
        {
            return Err(Error::Invalid);
        }
        let at = ROUND_HEADER + index * CANDIDATE_BYTES;
        for (offset, value) in [(0, r.thread),
                                (8, r.ready_since_microseconds),
                                (16, r.last_hart.unwrap_or(0)),
                                (24, r.running_microseconds)]
        {
            b[at + offset..at + offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        let flags = u32::from(r.interactive_wake) | if r.last_hart.is_some() { 2 } else { 0 };
        b[at + 32..at + 36].copy_from_slice(&flags.to_le_bytes());
        b[at + 36..at + 40].copy_from_slice(&(r.kind as u32).to_le_bytes());
    }
    Ok(b)
}
