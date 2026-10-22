//! Additive native delivery. Original Event/Input shapes remain source compatible.
use mac_abi::{WindowId, events as raw};
pub use mac_traps::coordinates::{PointerPosition, ScreenPoint, WindowPoint};

use crate::{Event, EventMask, EventSnapshot, Events, Result};

/// Owns the ordinary event (including drag guards) and its resident coordinates.
#[must_use]
#[derive(Debug)]
pub struct PositionedEvent<'a>
{
    event:    Event<'a>,
    position: PointerPosition
}
impl<'a> PositionedEvent<'a>
{
    pub fn event(&self) -> &Event<'a>
    {
        &self.event
    }

    pub const fn position(&self) -> PointerPosition
    {
        self.position
    }

    pub fn into_parts(self) -> (Event<'a>, PointerPosition)
    {
        (self.event, self.position)
    }
}

/// Peeking never acquires a drag response or drop-completion guard.
#[derive(Clone, Copy, Debug)]
pub struct PositionedSnapshot
{
    event:    EventSnapshot,
    position: PointerPosition
}
impl PositionedSnapshot
{
    pub const fn event(self) -> EventSnapshot
    {
        self.event
    }

    pub const fn position(self) -> PointerPosition
    {
        self.position
    }
}

impl<'a> Events<'a>
{
    /// Poll and capture window-local input in one trap. Screen coordinates and
    /// the existing event's input remain available without another service call.
    pub fn next_positioned(self, mask: EventMask) -> Result<PositionedEvent<'a>>
    {
        self.deliver_positioned(raw::POSITION_NEXT, mask, 0)
    }

    /// Scheduler-backed wait with the same captured-coordinate contract as poll.
    pub fn wait_positioned(self, mask: EventMask, timeout_ticks: u32)
                           -> Result<PositionedEvent<'a>>
    {
        self.deliver_positioned(raw::POSITION_WAIT, mask, timeout_ticks)
    }

    pub fn peek_positioned(self, mask: EventMask) -> Result<Option<PositionedSnapshot>>
    {
        let mut call = raw::PositionedEventCall { operation: raw::POSITION_PEEK,
                                                  mask: mask.bits(),
                                                  ..Default::default() };
        self.client.positioned_event(&mut call)?;
        Ok((call.present != 0).then(|| {
                                  PositionedSnapshot { event: EventSnapshot::from(call.event),
            position: PointerPosition::from_record(call.event.where_, call.position) }
                              }))
    }

    /// Sample physical input relative to an explicitly owned live window. Useful
    /// during captured tracking, even when the pointer is outside its rectangle.
    pub fn input_in(self, window: WindowId) -> Result<PositionedEvent<'a>>
    {
        let mut call = raw::PositionedEventCall { operation: raw::POSITION_INPUT,
                                                  window: window.0,
                                                  ..Default::default() };
        self.client.positioned_event(&mut call)?;
        Ok(self.decode_positioned(call))
    }

    /// Explicit bridge for existing/synthetic events. This projects against the
    /// current geometry; use next/wait_positioned for queued input-time positions.
    pub fn locate(self, event: &Event<'_>) -> Result<PointerPosition>
    {
        let mut call = raw::PositionedEventCall { operation: raw::POSITION_LOCATE,
                                                  event: event.as_record(),
                                                  ..Default::default() };
        self.client.positioned_event(&mut call)?;
        Ok(PointerPosition::from_record(call.event.where_, call.position))
    }

    fn deliver_positioned(self, operation: u32, mask: EventMask, timeout_ticks: u32)
                          -> Result<PositionedEvent<'a>>
    {
        let mut call = raw::PositionedEventCall { operation,
                                                  mask: mask.bits(),
                                                  timeout_ticks,
                                                  ..Default::default() };
        self.client.positioned_event(&mut call)?;
        Ok(self.decode_positioned(call))
    }

    fn decode_positioned(self, call: raw::PositionedEventCall) -> PositionedEvent<'a>
    {
        PositionedEvent { position: PointerPosition::from_record(call.event.where_, call.position),
                          event:    Event::decode(self.client, call.event) }
    }
}
