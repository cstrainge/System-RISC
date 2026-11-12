//! Device-neutral input provider contract: pointers and keyboards a driver
//! found, as a stream of events System asks for.
//!
//! POLL has empty input and replies with zero or more events, each
//! [`EVENT_BYTES`] long, at most [`MAX_EVENTS`] of them. An empty reply means
//! nothing happened, and is the common answer. A provider answers from what
//! it already has or can learn in one bounded step; it never waits for the
//! device, because System asks again on its next turn.
//!
//! The vocabulary is the one System's input decoder already speaks, which is
//! the evdev one a virtio input device uses: a kind, a code within the kind,
//! and a value. A provider translates its device's own reports into it, so
//! the decoder, the keymap and everything above them are shared with every
//! other source of input. A group of events that describe one moment ends
//! with a [`SYNCHRONIZE`] event; System acts on the group, not on its parts.
#![no_std]
#![forbid(unsafe_code)]

use mac_module::ClassId;

pub const CLASS: ClassId = ClassId(*b"INPT");
pub const VERSION: u32 = 1;
pub const POLL: u32 = 1;
pub const EVENT_BYTES: usize = 8;
pub const MAX_EVENTS: usize = 32;

/// Event kinds.
pub const SYNCHRONIZE: u16 = 0;
pub const KEY: u16 = 1;
pub const RELATIVE: u16 = 2;
pub const ABSOLUTE: u16 = 3;
/// Codes within [`RELATIVE`]: movement since the last report, in the
/// device's own counts, as a two's-complement value.
pub const RELATIVE_X: u16 = 0;
pub const RELATIVE_Y: u16 = 1;
/// Codes within [`KEY`] for a pointer's buttons. The value is 1 for down and
/// 0 for up.
pub const BUTTON_LEFT: u16 = 0x110;
pub const BUTTON_RIGHT: u16 = 0x111;
pub const BUTTON_MIDDLE: u16 = 0x112;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event
{
    pub kind:  u16,
    pub code:  u16,
    pub value: u32
}

impl Event
{
    pub const fn synchronize() -> Self
    {
        Self { kind:  SYNCHRONIZE,
               code:  0,
               value: 0 }
    }

    pub const fn relative(code: u16, delta: i32) -> Self
    {
        Self { kind: RELATIVE,
               code,
               value: delta as u32 }
    }

    pub const fn key(code: u16, down: bool) -> Self
    {
        Self { kind: KEY,
               code,
               value: down as u32 }
    }

    pub fn encode(self) -> [u8; EVENT_BYTES]
    {
        let mut bytes = [0; EVENT_BYTES];
        bytes[..2].copy_from_slice(&self.kind.to_le_bytes());
        bytes[2..4].copy_from_slice(&self.code.to_le_bytes());
        bytes[4..].copy_from_slice(&self.value.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Option<Self>
    {
        if bytes.len() != EVENT_BYTES
        {
            return None;
        }
        Some(Self { kind:  u16::from_le_bytes([bytes[0], bytes[1]]),
                    code:  u16::from_le_bytes([bytes[2], bytes[3]]),
                    value: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) })
    }
}
