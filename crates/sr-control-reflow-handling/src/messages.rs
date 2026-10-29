use mac_abi::controls::reflow as wire;

use crate::{Constraints, Error, LayoutRect, Result, Size, SizePreferences};

/// Explicit decoding; unknown wire values never become a valid enum variant.
#[repr(i16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation
{
    AskSize            = wire::ASK_SIZE,
    AssignRect         = wire::ASSIGN_RECT,
    InvalidateLayout   = wire::INVALIDATE_LAYOUT,
    SetVisibleRect     = wire::SET_VISIBLE_RECT,
    SetChild           = wire::SET_CHILD,
    SetChildIndexed    = wire::SET_CHILD_INDEXED,
    RemoveChild        = wire::REMOVE_CHILD,
    Reparent           = wire::REPARENT,
    SetMinimum         = wire::SET_MINIMUM,
    ClearMinimum       = wire::CLEAR_MINIMUM,
    SetPreferred       = wire::SET_PREFERRED,
    ClearPreferred     = wire::CLEAR_PREFERRED,
    SetSizePreferences = wire::SET_SIZE_PREFERENCES
}
impl TryFrom<i16> for Operation
{
    type Error = Error;

    fn try_from(value: i16) -> Result<Self>
    {
        match value
        {
            wire::ASK_SIZE => Ok(Self::AskSize),
            wire::ASSIGN_RECT => Ok(Self::AssignRect),
            wire::INVALIDATE_LAYOUT => Ok(Self::InvalidateLayout),
            wire::SET_VISIBLE_RECT => Ok(Self::SetVisibleRect),
            wire::SET_CHILD => Ok(Self::SetChild),
            wire::SET_CHILD_INDEXED => Ok(Self::SetChildIndexed),
            wire::REMOVE_CHILD => Ok(Self::RemoveChild),
            wire::REPARENT => Ok(Self::Reparent),
            wire::SET_MINIMUM => Ok(Self::SetMinimum),
            wire::CLEAR_MINIMUM => Ok(Self::ClearMinimum),
            wire::SET_PREFERRED => Ok(Self::SetPreferred),
            wire::CLEAR_PREFERRED => Ok(Self::ClearPreferred),
            wire::SET_SIZE_PREFERENCES => Ok(Self::SetSizePreferences),
            _ => Err(Error::UnsupportedMessage(value))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalidation
{
    Measurement,
    Arrangement,
    MeasurementAndArrangement
}
impl TryFrom<u32> for Invalidation
{
    type Error = Error;

    fn try_from(value: u32) -> Result<Self>
    {
        match value
        {
            wire::INVALIDATE_MEASUREMENT => Ok(Self::Measurement),
            wire::INVALIDATE_ARRANGEMENT => Ok(Self::Arrangement),
            value if value == wire::INVALIDATE_MEASUREMENT | wire::INVALIDATE_ARRANGEMENT =>
            {
                Ok(Self::MeasurementAndArrangement)
            }
            _ => Err(Error::InvalidEncoding)
        }
    }
}
impl From<Invalidation> for u32
{
    fn from(value: Invalidation) -> Self
    {
        match value
        {
            Invalidation::Measurement => wire::INVALIDATE_MEASUREMENT,
            Invalidation::Arrangement => wire::INVALIDATE_ARRANGEMENT,
            Invalidation::MeasurementAndArrangement =>
            {
                wire::INVALIDATE_MEASUREMENT | wire::INVALIDATE_ARRANGEMENT
            }
        }
    }
}

/// Typed layout vocabulary. O and R are supplied by the resident/wrapper layer,
/// not a new ID scheme: O is an owning control consumed by adoption, while R is
/// a validated non-owning ref used for removal/reparenting. S is the container's
/// typed slot. This crate neither decodes pointers nor issues object IDs.
///
/// Ownership operations are manager transactions, not shared sizing mutations.
/// Failed adoption must return the original owner. Reparenting must preserve the
/// old edge on failure. This enum neither performs nor authorizes those changes.
pub enum Message<O = core::convert::Infallible,
 R = core::convert::Infallible,
 S = core::convert::Infallible>
{
    AskSize(Constraints),
    /// Preparation request only. A valid forced size cannot be vetoed on
    /// preference; operational failure leaves old geometry published.
    AssignRect(LayoutRect),
    InvalidateLayout(Invalidation),
    /// Local visible clip, independent of assigned geometry. Wider logical
    /// scrolling coordinates must be projected by the resident view layer.
    SetVisibleRect(LayoutRect),
    SetChild(O),
    SetChildIndexed
    {
        child: O,
        slot:  S
    },
    RemoveChild(R),
    Reparent
    {
        child:  R,
        parent: R,
        slot:   Option<S>
    },
    SetMinimum(Size),
    ClearMinimum,
    SetPreferred(Size),
    ClearPreferred,
    SetSizePreferences(SizePreferences)
}
impl<O, R, S> Message<O, R, S>
{
    pub const fn operation(&self) -> Operation
    {
        match self
        {
            Self::AskSize(_) => Operation::AskSize,
            Self::AssignRect(_) => Operation::AssignRect,
            Self::InvalidateLayout(_) => Operation::InvalidateLayout,
            Self::SetVisibleRect(_) => Operation::SetVisibleRect,
            Self::SetChild(_) => Operation::SetChild,
            Self::SetChildIndexed { .. } => Operation::SetChildIndexed,
            Self::RemoveChild(_) => Operation::RemoveChild,
            Self::Reparent { .. } => Operation::Reparent,
            Self::SetMinimum(_) => Operation::SetMinimum,
            Self::ClearMinimum => Operation::ClearMinimum,
            Self::SetPreferred(_) => Operation::SetPreferred,
            Self::ClearPreferred => Operation::ClearPreferred,
            Self::SetSizePreferences(_) => Operation::SetSizePreferences
        }
    }
}

/// Effects are applied by System after admission/publication. Measurement,
/// arrangement, paint damage and visibility must not be conflated. A true paint
/// flag requires the corresponding bounded damage from the publishing manager;
/// it never means invalidate the desktop or an entire parent window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Effects
{
    pub measurement: bool,
    pub arrangement: bool,
    pub paint:       bool,
    pub visibility:  bool
}

/// Only Unhandled may fall through. M retains the original request on both
/// refusal and pass-through, including any owning child awaiting adoption.
/// Successful ownership transfer belongs in R. Effects are not notification;
/// any author change hook after commit must be infallible deferred bookkeeping.
#[must_use]
pub enum Outcome<M, R, E>
{
    Unhandled(M),
    Handled
    {
        reply:   R,
        effects: Effects
    },
    Failed
    {
        request: M,
        cause:   E
    }
}

/// Decode the implemented leaf vocabulary. Relationship messages are refused
/// until the resident ownership transaction supplies its typed owner/ref payload.
pub fn decode_leaf(payload: &wire::Payload) -> Result<Message>
{
    if payload.reserved != 0
    {
        return Err(Error::InvalidEncoding);
    }
    Ok(match Operation::try_from(payload.message)?
    {
        Operation::AskSize => Message::AskSize(payload.constraints.try_into()?),
        Operation::AssignRect => Message::AssignRect(payload.rect.try_into()?),
        Operation::SetVisibleRect => Message::SetVisibleRect(payload.rect.try_into()?),
        Operation::InvalidateLayout => Message::InvalidateLayout(payload.invalidation.try_into()?),
        Operation::SetMinimum => Message::SetMinimum(payload.size.try_into()?),
        Operation::ClearMinimum => Message::ClearMinimum,
        Operation::SetPreferred => Message::SetPreferred(payload.size.try_into()?),
        Operation::ClearPreferred => Message::ClearPreferred,
        Operation::SetSizePreferences =>
        {
            Message::SetSizePreferences(payload.preferences.try_into()?)
        }
        _ => return Err(Error::UnsupportedMessage(payload.message))
    })
}
