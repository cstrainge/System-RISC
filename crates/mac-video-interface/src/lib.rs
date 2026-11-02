//! Device-neutral video-output contract for external kernel drivers.
//!
//! System owns the framebuffer memory and every policy decision (limits,
//! accepted modes, repaint). A driver only presents that memory on a display,
//! reports the host's preferred size and, after its own debounce, tells System
//! when that size has settled. System polls; the driver never calls upward.
//! All integers are little-endian.
#![no_std]
#![forbid(unsafe_code)]
use mac_module::ClassId;
pub const CLASS: ClassId = ClassId(*b"VIDO");
pub const VERSION: u32 = 1;
/// Capability: the driver can change mode after ATTACH (APPLY).
pub const RESIZE: u64 = 1;
pub const ATTACH: u32 = 1;
pub const SERVICE: u32 = 2;
pub const APPLY: u32 = 3;

fn word(bytes: &[u8], at: usize) -> Option<u32>
{
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// ATTACH input: System's initial mode and the limits it enforces. The driver
/// owns the scan-out memory inside its own DMA-capable image; it cannot map
/// System memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attach
{
    pub width:      u32,
    pub height:     u32,
    pub min_width:  u32,
    pub min_height: u32,
    pub max_width:  u32,
    pub max_height: u32
}
impl Attach
{
    pub const BYTES: usize = 24;

    pub fn encode(&self) -> [u8; Self::BYTES]
    {
        let mut bytes = [0; Self::BYTES];
        for (index, value) in [self.width,
                               self.height,
                               self.min_width,
                               self.min_height,
                               self.max_width,
                               self.max_height].into_iter().enumerate()
        {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Option<Self>
    {
        if bytes.len() != Self::BYTES
        {
            return None;
        }
        Some(Self { width:      word(bytes, 0)?,
                    height:     word(bytes, 4)?,
                    min_width:  word(bytes, 8)?,
                    min_height: word(bytes, 12)?,
                    max_width:  word(bytes, 16)?,
                    max_height: word(bytes, 20)? })
    }
}

/// ATTACH reply: the driver-owned framebuffer System draws into (XRGB32,
/// stride = width * 4, valid for the driver's lifetime) and the host's
/// preferred size (0x0 when it expressed none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Surface
{
    pub address:   u64,
    pub capacity:  u64,
    pub preferred: Size
}
impl Surface
{
    pub const BYTES: usize = 24;

    pub fn encode(&self) -> [u8; Self::BYTES]
    {
        let mut bytes = [0; Self::BYTES];
        bytes[0..8].copy_from_slice(&self.address.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.capacity.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.preferred.encode());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Option<Self>
    {
        if bytes.len() != Self::BYTES
        {
            return None;
        }
        Some(Self { address:   u64::from_le_bytes(bytes[0..8].try_into().ok()?),
                    capacity:  u64::from_le_bytes(bytes[8..16].try_into().ok()?),
                    preferred: Size::decode(&bytes[16..24])? })
    }
}
/// A 0x0 size means "unknown".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size
{
    pub width:  u32,
    pub height: u32
}
impl Size
{
    pub const BYTES: usize = 8;

    pub fn encode(&self) -> [u8; Self::BYTES]
    {
        let mut bytes = [0; Self::BYTES];
        bytes[0..4].copy_from_slice(&self.width.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.height.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Option<Self>
    {
        if bytes.len() != Self::BYTES
        {
            return None;
        }
        Some(Self { width:  word(bytes, 0)?,
                    height: word(bytes, 4)? })
    }
}

/// SERVICE input: the framebuffer rectangle System changed since the last
/// call. An empty rectangle (zero width or height) means nothing to present.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Damage
{
    pub x:      u32,
    pub y:      u32,
    pub width:  u32,
    pub height: u32
}
impl Damage
{
    pub const BYTES: usize = 16;

    pub fn encode(&self) -> [u8; Self::BYTES]
    {
        let mut bytes = [0; Self::BYTES];
        for (index, value) in [self.x, self.y, self.width, self.height].into_iter().enumerate()
        {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Option<Self>
    {
        if bytes.len() != Self::BYTES
        {
            return None;
        }
        Some(Self { x:      word(bytes, 0)?,
                    y:      word(bytes, 4)?,
                    width:  word(bytes, 8)?,
                    height: word(bytes, 12)? })
    }
}

/// SERVICE reply: the presented damage was submitted and, if the host's
/// preferred size changed and then stayed unchanged for the driver's quiet
/// period, the size that System should consider applying.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Poll
{
    pub settled: Option<Size>
}
impl Poll
{
    pub const BYTES: usize = 12;

    pub fn encode(&self) -> [u8; Self::BYTES]
    {
        let mut bytes = [0; Self::BYTES];
        if let Some(size) = self.settled
        {
            bytes[0..4].copy_from_slice(&1u32.to_le_bytes());
            bytes[4..12].copy_from_slice(&size.encode());
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Option<Self>
    {
        if bytes.len() != Self::BYTES
        {
            return None;
        }
        match word(bytes, 0)?
        {
            0 => Some(Self { settled: None }),
            1 => Some(Self { settled: Some(Size::decode(&bytes[4..12])?) }),
            _ => None
        }
    }
}

/// Typed result of APPLY. Anything other than `Applied` leaves the previous
/// mode scanned out and still valid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome
{
    Applied,
    TooSmall,
    TooLarge,
    /// The mode needs more bytes than System's framebuffer reservation.
    ExceedsReservation,
    /// The device rejected or failed to complete the mode change.
    DeviceRefused
}
impl Outcome
{
    pub const BYTES: usize = 4;

    pub const fn code(self) -> u32
    {
        match self
        {
            Self::Applied => 0,
            Self::TooSmall => 1,
            Self::TooLarge => 2,
            Self::ExceedsReservation => 3,
            Self::DeviceRefused => 4
        }
    }

    pub const fn from_code(code: u32) -> Option<Self>
    {
        match code
        {
            0 => Some(Self::Applied),
            1 => Some(Self::TooSmall),
            2 => Some(Self::TooLarge),
            3 => Some(Self::ExceedsReservation),
            4 => Some(Self::DeviceRefused),
            _ => None
        }
    }

    pub const fn message(self) -> &'static str
    {
        match self
        {
            Self::Applied => "display mode applied",
            Self::TooSmall => "requested display mode is below the minimum size",
            Self::TooLarge => "requested display mode exceeds the maximum size",
            Self::ExceedsReservation =>
            {
                "requested display mode needs more memory than the framebuffer reservation"
            }
            Self::DeviceRefused => "the video device refused or failed the mode change"
        }
    }
}
