//! Lease the independently usable resources of one block.
//!
//! A device claim is exclusive per register page, which is right for a block and
//! wrong for a block whose page holds fifteen DMA channels or dozens of GPIO
//! pins: a driver needing one channel would have to own the whole controller and
//! exclude every other driver. A pooled block therefore has one owner, which is
//! the resident arbiter, and its resources are leased from it.
//!
//! This contract is deliberately ignorant of what a resource is. A DMA channel
//! and a GPIO pin are both a bit in a mask the platform record declares, and the
//! same operations serve both.
//!
//! The owner of a lease is the calling module, established by the request's own
//! admission rather than by a field the caller supplies. When that module's
//! admission ends, every lease it held is released, so a driver that dies cannot
//! strand a channel.
//!
//! All integers are little-endian.
#![no_std]
#![forbid(unsafe_code)]

use mac_module::ClassId;

pub const CLASS: ClassId = ClassId(*b"POOL");
pub const VERSION: u32 = 1;

pub const LEASE: u32 = 1;
pub const RELEASE: u32 = 2;
pub const AVAILABLE: u32 = 3;

/// LEASE input. `requirement` is a resource number, or `ANY_RESOURCE` when the
/// caller needs a resource and does not care which. Preferring `ANY` is what lets
/// two serialisers coexist on one controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request_
{
    pub key:          [u8; 16],
    pub requirement:  u32
}
impl Request_
{
    /// The value meaning "any free resource in this pool".
    pub const ANY_RESOURCE: u32 = u32::MAX;
    pub const BYTES: usize = 20;

    pub fn encode(&self) -> [u8; 24]
    {
        let mut bytes = [0; 24];
        bytes[..16].copy_from_slice(&self.key);
        bytes[16..20].copy_from_slice(&self.requirement.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Option<Self>
    {
        if bytes.len() != Self::BYTES
        {
            return None;
        }
        Some(Self { key: bytes[..16].try_into().ok()?,
                    requirement: u32::from_le_bytes(bytes[16..20].try_into().ok()?) })
    }
}

/// LEASE reply: the resource and the handle that names this lease when it is
/// released. The handle is opaque and is refused if it belongs to another owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lease
{
    pub handle:   u64,
    pub resource: u32
}
impl Lease
{
    pub const BYTES: usize = 12;

    pub fn encode(&self) -> [u8; 16]
    {
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&self.handle.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.resource.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Option<Self>
    {
        if bytes.len() != Self::BYTES
        {
            return None;
        }
        Some(Self { handle: u64::from_le_bytes(bytes[..8].try_into().ok()?),
                    resource: u32::from_le_bytes(bytes[8..12].try_into().ok()?) })
    }
}

pub const AVAILABLE_BYTES: usize = 8;
pub const MAX_REPLY: usize = 16;
