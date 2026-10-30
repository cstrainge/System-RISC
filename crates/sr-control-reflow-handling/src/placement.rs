use mac_abi::controls::reflow as wire;

use crate::{Error, Result};

/// Per-edge attachment within the parent's allocated cell or merged region.
/// No edges center at preferred size; one edge pins; opposite edges stretch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Anchors
{
    pub left:   bool,
    pub right:  bool,
    pub top:    bool,
    pub bottom: bool
}
impl Anchors
{
    pub const ALL: Self = Self { left:   true,
                                 right:  true,
                                 top:    true,
                                 bottom: true };
    pub const NONE: Self = Self { left:   false,
                                  right:  false,
                                  top:    false,
                                  bottom: false };
}
impl TryFrom<u32> for Anchors
{
    type Error = Error;

    fn try_from(bits: u32) -> Result<Self>
    {
        if bits & !15 != 0
        {
            return Err(Error::InvalidEncoding);
        }
        Ok(Self { left:   bits & 1 != 0,
                  right:  bits & 2 != 0,
                  top:    bits & 4 != 0,
                  bottom: bits & 8 != 0 })
    }
}
impl From<Anchors> for u32
{
    fn from(a: Anchors) -> Self
    {
        u32::from(a.left)
        | u32::from(a.right) << 1
        | u32::from(a.top) << 2
        | u32::from(a.bottom) << 3
    }
}
/// A nonempty rectangular span. Placement checks the exclusive row/column ends
/// and refuses overlap with any other hosted control before changing ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span
{
    rows:    u32,
    columns: u32
}
impl Span
{
    pub const ONE: Self = Self { rows:    1,
                                 columns: 1 };

    pub const fn new(rows: u32, columns: u32) -> Result<Self>
    {
        if rows == 0 || columns == 0
        {
            Err(Error::InvalidEncoding)
        }
        else
        {
            Ok(Self { rows,
                      columns })
        }
    }

    pub const fn rows(self) -> u32
    {
        self.rows
    }

    pub const fn columns(self) -> u32
    {
        self.columns
    }
}
impl Default for Span
{
    fn default() -> Self
    {
        Self::ONE
    }
}
impl From<Span> for wire::Span
{
    fn from(s: Span) -> Self
    {
        Self { rows:    s.rows,
               columns: s.columns }
    }
}
impl TryFrom<wire::Span> for Span
{
    type Error = Error;

    fn try_from(s: wire::Span) -> Result<Self>
    {
        Self::new(s.rows, s.columns)
    }
}
/// Percent targets are hundredths of a percent (10_000 = 100%). Minima take
/// precedence; Fit grows toward preferred before Fill receives the remainder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrackSize
{
    #[default]
    Fit,
    Fill,
    Percent(u16)
}
impl TrackSize
{
    pub fn encode(self) -> Result<(u32, u32)>
    {
        Ok(match self
        {
            Self::Fit => (wire::TRACK_FIT, 0),
            Self::Fill => (wire::TRACK_FILL, 0),
            Self::Percent(p) if p <= 10_000 => (wire::TRACK_PERCENT, u32::from(p)),
            _ => return Err(Error::InvalidEncoding)
        })
    }

    pub fn decode(mode: u32, percent: u32) -> Result<Self>
    {
        match (mode, percent)
        {
            (wire::TRACK_FIT, 0) => Ok(Self::Fit),
            (wire::TRACK_FILL, 0) => Ok(Self::Fill),
            (wire::TRACK_PERCENT, 0..=10_000) => Ok(Self::Percent(percent as u16)),
            _ => Err(Error::InvalidEncoding)
        }
    }
}

/// Nonnegative offsets from each owning-window content border. Zero aligns
/// directly to that border. Oversized opposing offsets leave zero usable space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BorderOffsets
{
    left:   i16,
    right:  i16,
    top:    i16,
    bottom: i16
}
impl BorderOffsets
{
    pub const ZERO: Self = Self { left:   0,
                                  right:  0,
                                  top:    0,
                                  bottom: 0 };

    pub const fn new(left: i16, right: i16, top: i16, bottom: i16) -> Result<Self>
    {
        if left < 0 || right < 0 || top < 0 || bottom < 0
        {
            return Err(Error::InvalidEncoding);
        }
        Ok(Self { left,
                  right,
                  top,
                  bottom })
    }

    pub const fn left(self) -> i16
    {
        self.left
    }

    pub const fn right(self) -> i16
    {
        self.right
    }

    pub const fn top(self) -> i16
    {
        self.top
    }

    pub const fn bottom(self) -> i16
    {
        self.bottom
    }
}
impl From<BorderOffsets> for wire::BorderOffsets
{
    fn from(v: BorderOffsets) -> Self
    {
        Self { left:   v.left,
               right:  v.right,
               top:    v.top,
               bottom: v.bottom }
    }
}
impl TryFrom<wire::BorderOffsets> for BorderOffsets
{
    type Error = Error;

    fn try_from(v: wire::BorderOffsets) -> Result<Self>
    {
        Self::new(v.left, v.right, v.top, v.bottom)
    }
}
