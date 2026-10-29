//! SCTL is resident scheduler mechanism, never a module substitute. Feature
//! bits describe exposed operations, not proof of SMP safety. The core owns
//! canonical registrations, image/Instance pins, real admission and retirement.
use super::{Error, Result, u32_at, u64_at};
pub const LIMITS: u32 = 1;
pub const PREPARE: u32 = 2;
pub const ARM: u32 = 3;
pub const HARTS: u32 = 4;
pub const STATUS: u32 = 5;
pub const RETIRE: u32 = 6;
pub const TAKEOVER: u32 = 1;
pub const MULTI_HART: u32 = 2;
pub const PARK: u32 = 4;
pub const FEATURES: u32 = TAKEOVER | MULTI_HART | PARK;
pub const MAX_HARTS: u32 = 64;
pub const LIMITS_BYTES: usize = 64;
pub const REGISTRATION_BYTES: usize = 32;
pub const PREPARE_BYTES: usize = 16;
pub const HART_REQUEST_BYTES: usize = 16;
pub const MIN_REPLY: usize = 256;
pub const MAX_REPLY: usize = 2048;
pub const MAX_CAUSE: usize = 240;
/// POLICY is a private, synchronous Instance dispatch phase. It is not a
/// general broker operation: no DREQ or pending ticket is created. Call.ticket
/// carries the checked per-hart round sequence; Core retains actual authority.
pub const POLICY_PHASE: u32 = 6;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits
{
    pub features:                     u32,
    pub maximum_candidates:           u32,
    pub maximum_active_harts:         u32,
    pub minimum_quantum_microseconds: u64,
    pub maximum_quantum_microseconds: u64,
    pub callback_budget_microseconds: u64,
    pub physical_harts:               u32,
    pub active_harts:                 u32,
    pub boot_hart:                    u64,
    pub topology_epoch:               u64
}
impl Limits
{
    pub fn validate(self) -> Result<()>
    {
        if self.features & !FEATURES != 0
           || self.features & TAKEOVER == 0
           || self.maximum_candidates == 0
           || self.maximum_candidates as usize > super::MAX_CANDIDATES
           || self.maximum_active_harts == 0
           || self.maximum_active_harts > self.physical_harts
           || self.physical_harts > MAX_HARTS
           || self.active_harts == 0
           || self.active_harts > self.maximum_active_harts
           || self.minimum_quantum_microseconds == 0
           || self.maximum_quantum_microseconds < self.minimum_quantum_microseconds
           || self.maximum_quantum_microseconds > u64::from(u32::MAX)
           || self.callback_budget_microseconds == 0
           || self.topology_epoch == 0
           || self.features & MULTI_HART == 0 && self.maximum_active_harts != 1
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    pub fn decode(b: &[u8]) -> Result<Self>
    {
        if b.len() != LIMITS_BYTES || u32_at(b, 0)? != super::VERSION
        {
            return Err(Error::Version);
        }
        let v = Self { features:                     u32_at(b, 4)?,
                       maximum_candidates:           u32_at(b, 8)?,
                       maximum_active_harts:         u32_at(b, 12)?,
                       minimum_quantum_microseconds: u64_at(b, 16)?,
                       maximum_quantum_microseconds: u64_at(b, 24)?,
                       callback_budget_microseconds: u64_at(b, 32)?,
                       physical_harts:               u32_at(b, 40)?,
                       active_harts:                 u32_at(b, 44)?,
                       boot_hart:                    u64_at(b, 48)?,
                       topology_epoch:               u64_at(b, 56)? };
        v.validate()?;
        Ok(v)
    }

    pub fn encode(self) -> Result<[u8; LIMITS_BYTES]>
    {
        self.validate()?;
        let mut b = [0; LIMITS_BYTES];
        for (at, v) in [(0, super::VERSION),
                        (4, self.features),
                        (8, self.maximum_candidates),
                        (12, self.maximum_active_harts),
                        (40, self.physical_harts),
                        (44, self.active_harts)]
        {
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        for (at, v) in [(16, self.minimum_quantum_microseconds),
                        (24, self.maximum_quantum_microseconds),
                        (32, self.callback_budget_microseconds),
                        (48, self.boot_hart),
                        (56, self.topology_epoch)]
        {
            b[at..at + 8].copy_from_slice(&v.to_le_bytes());
        }
        Ok(b)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requirements
{
    pub features:           u32,
    pub candidate_capacity: u32,
    pub topology_epoch:     u64
}
impl Requirements
{
    pub fn validate(self, limits: Limits) -> Result<()>
    {
        limits.validate()?;
        if self.features & TAKEOVER == 0
           || self.features & !limits.features != 0
           || self.candidate_capacity != limits.maximum_candidates
           || self.topology_epoch != limits.topology_epoch
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    pub fn encode(self) -> [u8; PREPARE_BYTES]
    {
        let mut b = [0; PREPARE_BYTES];
        b[..4].copy_from_slice(&self.features.to_le_bytes());
        b[4..8].copy_from_slice(&self.candidate_capacity.to_le_bytes());
        b[8..].copy_from_slice(&self.topology_epoch.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8], limits: Limits) -> Result<Self>
    {
        if b.len() != PREPARE_BYTES
        {
            return Err(Error::Truncated);
        }
        let v = Self { features:           u32_at(b, 0)?,
                       candidate_capacity: u32_at(b, 4)?,
                       topology_epoch:     u64_at(b, 8)? };
        v.validate(limits)?;
        Ok(v)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum State
{
    Prepared    = 1,
    Armed       = 2,
    Active      = 3,
    Retiring    = 4,
    Retired     = 5,
    Quarantined = 6
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registration
{
    /// Canonical shared-facility registration incarnation, never caller-issued.
    pub generation:   u64,
    pub state:        State,
    pub last_status:  i32,
    pub active_harts: u32,
    pub callbacks:    u32,
    pub transition:   u64
}
impl Registration
{
    pub fn validate(self) -> Result<()>
    {
        if self.last_status > 0
           || self.active_harts == 0
           || self.active_harts > MAX_HARTS
           || self.callbacks > MAX_HARTS
           || self.state != State::Retired && (self.generation == 0 || self.transition == 0)
           || self.state == State::Retired && (self.callbacks != 0 || self.active_harts != 1)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    pub fn decode(b: &[u8]) -> Result<Self>
    {
        if b.len() != REGISTRATION_BYTES
        {
            return Err(Error::Truncated);
        }
        let state = match u32_at(b, 8)?
        {
            1 => State::Prepared,
            2 => State::Armed,
            3 => State::Active,
            4 => State::Retiring,
            5 => State::Retired,
            6 => State::Quarantined,
            _ => return Err(Error::Invalid)
        };
        let v = Self { generation: u64_at(b, 0)?,
                       state,
                       last_status: i32::from_le_bytes(super::word(b, 12)?),
                       active_harts: u32_at(b, 16)?,
                       callbacks: u32_at(b, 20)?,
                       transition: u64_at(b, 24)? };
        v.validate()?;
        Ok(v)
    }

    pub fn encode(self) -> Result<[u8; REGISTRATION_BYTES]>
    {
        self.validate()?;
        let mut b = [0; REGISTRATION_BYTES];
        b[..8].copy_from_slice(&self.generation.to_le_bytes());
        b[8..12].copy_from_slice(&(self.state as u32).to_le_bytes());
        b[12..16].copy_from_slice(&self.last_status.to_le_bytes());
        b[16..20].copy_from_slice(&self.active_harts.to_le_bytes());
        b[20..24].copy_from_slice(&self.callbacks.to_le_bytes());
        b[24..32].copy_from_slice(&self.transition.to_le_bytes());
        Ok(b)
    }
}
/// Explicit package policy: request the first N eligible hardware IDs in
/// ascending order, including the primary. Core must not silently substitute
/// fewer CPUs or claim completed admission after partial transition failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HartRequest
{
    pub generation: u64,
    pub count:      u32
}
impl HartRequest
{
    pub fn encode(self) -> Result<[u8; HART_REQUEST_BYTES]>
    {
        if self.generation == 0 || self.count == 0 || self.count > MAX_HARTS
        {
            return Err(Error::Invalid);
        }
        let mut b = [0; HART_REQUEST_BYTES];
        b[..8].copy_from_slice(&self.generation.to_le_bytes());
        b[8..12].copy_from_slice(&self.count.to_le_bytes());
        b[12..16].copy_from_slice(&1u32.to_le_bytes());
        Ok(b)
    }

    pub fn decode(b: &[u8]) -> Result<Self>
    {
        if b.len() != HART_REQUEST_BYTES || u32_at(b, 12)? != 1
        {
            return Err(Error::Version);
        }
        let v = Self { generation: u64_at(b, 0)?,
                       count:      u32_at(b, 8)? };
        v.encode()?;
        Ok(v)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply<'a>
{
    Success(&'a [u8]),
    Failure
    {
        status: i32,
        cause:  &'a str
    }
}
impl<'a> Reply<'a>
{
    pub fn decode(b: &'a [u8]) -> Result<Self>
    {
        if b.len() < 8 || b.len() > MAX_REPLY
        {
            return Err(Error::Truncated);
        }
        let status = i32::from_le_bytes(super::word(b, 0)?);
        let length = u32_at(b, 4)? as usize;
        if status > 0 || b.len() != 8 + length
        {
            return Err(Error::Invalid);
        }
        if status == 0
        {
            return Ok(Self::Success(&b[8..]));
        }
        if length == 0 || length > MAX_CAUSE
        {
            return Err(Error::Limit);
        }
        let cause = core::str::from_utf8(&b[8..]).map_err(|_| Error::Invalid)?;
        if cause.chars().any(char::is_control)
        {
            return Err(Error::Invalid);
        }
        Ok(Self::Failure { status,
                           cause })
    }
}
/// Writes no effect itself. Resident operations validate MIN_REPLY before any
/// preparation, activation or retirement intent can occur.
pub fn reply<'a>(value: Reply<'_>, out: &'a mut [u8]) -> Result<&'a [u8]>
{
    let (status, payload) = match value
    {
        Reply::Success(p) => (0, p),
        Reply::Failure { status,
                         cause } =>
        {
            if status >= 0
               || cause.is_empty()
               || cause.len() > MAX_CAUSE
               || cause.chars().any(char::is_control)
            {
                return Err(Error::Invalid);
            }
            (status, cause.as_bytes())
        }
    };
    if out.len() < MIN_REPLY || payload.len() > MAX_REPLY - 8
    {
        return Err(Error::Limit);
    }
    let b = out.get_mut(..8 + payload.len()).ok_or(Error::Truncated)?;
    b[..4].copy_from_slice(&status.to_le_bytes());
    b[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    b[8..].copy_from_slice(payload);
    Ok(b)
}
