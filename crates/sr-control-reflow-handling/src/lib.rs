//! Shared control layout contracts and allocation-free sizing rules.
//!
//! System owns instance storage, object identities, child lifetime, invocation
//! pins, scheduling and publication. This crate owns neither a registry nor a
//! second layout tree. Definition bindings decode wire values before passing
//! these safe types to control authors. Native leaf dispatch is opt-in per
//! instance. Relationship transactions, root scheduling and wider scrolling
//! extents remain separate integration gates; leaf handling refuses adoption.
#![no_std]
#![forbid(unsafe_code)]

/// Immutable native creation choice, independent of legacy procID/variation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reflow
{
    Yes,
    #[default]
    No
}
impl TryFrom<u32> for Reflow
{
    type Error = Error;

    fn try_from(value: u32) -> Result<Self>
    {
        match value
        {
            0 => Ok(Self::No),
            1 => Ok(Self::Yes),
            _ => Err(Error::InvalidEncoding)
        }
    }
}
impl From<Reflow> for u32
{
    fn from(value: Reflow) -> Self
    {
        u32::from(value == Reflow::Yes)
    }
}

mod geometry;
mod messages;
mod sizing;
pub use geometry::{AxisConstraint, Constraints, Extent, LayoutRect, Size};
pub use mac_abi::{Dimensions, Point, Rect};
pub use messages::{Effects, Invalidation, Message, Operation, Outcome, decode_leaf};
pub use sizing::{Measurement, PreparedSettings, Requirements, SettingsChange, SizePreferences,
                 SizingState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    NegativeDimension,
    ExtentOverflow,
    InvertedRectangle,
    InvalidEncoding,
    InvalidRequirements,
    UnsupportedMessage(i16)
}
impl Error
{
    pub const fn status(self) -> mac_abi::Status
    {
        match self
        {
            Self::UnsupportedMessage(_) => mac_abi::NOT_SUPPORTED,
            _ => mac_abi::BAD_ARGUMENT
        }
    }
}
impl core::fmt::Display for Error
{
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::NegativeDimension => formatter.write_str("layout dimensions must be nonnegative"),
            Self::ExtentOverflow =>
            {
                formatter.write_str("layout extent exceeds the local i16 pixel range")
            }
            Self::InvertedRectangle =>
            {
                formatter.write_str("layout rectangle has reversed endpoints")
            }
            Self::InvalidEncoding => formatter.write_str("malformed control reflow payload"),
            Self::InvalidRequirements =>
            {
                formatter.write_str("effective preferred size is below minimum")
            }
            Self::UnsupportedMessage(number) =>
            {
                write!(formatter, "unsupported control reflow message {number}")
            }
        }
    }
}
pub type Result<T> = core::result::Result<T, Error>;
