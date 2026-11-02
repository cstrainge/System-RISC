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
/// A finite, nonnegative size. Percentages use 100.0 for the whole offered
/// Table axis (after splitter gutters); resolved fractional pixels round down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SizeUnit
{
    Pixels(i16),
    Percentage(f32)
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fill
{
    /// Content ideal clamped by minimum/maximum; receives no fill distribution.
    #[default]
    None,
    /// Share remaining space, subject to minimum/maximum.
    Fill
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SizeBounds
{
    min:  Option<SizeUnit>,
    max:  Option<SizeUnit>,
    fill: Fill
}
impl SizeUnit
{
    fn validate(self) -> Result<()>
    {
        match self
        {
            Self::Pixels(n) if n >= 0 => Ok(()),
            Self::Percentage(p) if p.is_finite() && p >= 0.0 => Ok(()),
            _ => Err(Error::InvalidRequirements)
        }
    }

    pub fn resolve(self, basis: Option<i64>) -> Result<Option<i64>>
    {
        self.validate()?;
        match self
        {
            Self::Pixels(n) => Ok(Some(i64::from(n))),
            Self::Percentage(p) =>
            {
                let Some(basis) = basis
                else
                {
                    return Ok(None);
                };
                if basis < 0
                {
                    return Err(Error::InvalidEncoding);
                }
                let value = basis as f64 * f64::from(p) / 100.0;
                if !value.is_finite() || value >= i64::MAX as f64
                {
                    return Err(Error::ExtentOverflow);
                }
                Ok(Some(value as i64))
            }
        }
    }
}
impl SizeBounds
{
    pub const CONTENT: Self = Self { min:  None,
                                     max:  None,
                                     fill: Fill::None };
    pub const FILL: Self = Self { min:  None,
                                  max:  None,
                                  fill: Fill::Fill };

    pub fn new(min: Option<SizeUnit>, max: Option<SizeUnit>, fill: Fill) -> Result<Self>
    {
        let value = Self { min,
                           max,
                           fill };
        value.validate()?;
        Ok(value)
    }

    pub const fn minimum(self) -> Option<SizeUnit>
    {
        self.min
    }

    pub const fn maximum(self) -> Option<SizeUnit>
    {
        self.max
    }

    pub const fn fill(self) -> Fill
    {
        self.fill
    }

    pub const fn has_percentage(self) -> bool
    {
        matches!(self.min, Some(SizeUnit::Percentage(_)))
        || matches!(self.max, Some(SizeUnit::Percentage(_)))
    }

    fn validate(self) -> Result<()>
    {
        if let Some(v) = self.min
        {
            v.validate()?;
        }
        if let Some(v) = self.max
        {
            v.validate()?;
        }
        if matches!((self.min,self.max),(Some(SizeUnit::Pixels(a)),Some(SizeUnit::Pixels(b))) if a>b)
           || matches!((self.min,self.max),(Some(SizeUnit::Percentage(a)),Some(SizeUnit::Percentage(b))) if a>b)
        {
            return Err(Error::InvalidRequirements);
        }
        Ok(())
    }

    pub fn encode(self) -> Result<wire::SizeBounds>
    {
        self.validate()?;
        let unit = |v: Option<SizeUnit>| {
            match v
            {
                None => wire::SizeUnit::default(),
                Some(SizeUnit::Pixels(n)) =>
                {
                    wire::SizeUnit { kind:  1,
                                     value: n as u32 }
                }
                Some(SizeUnit::Percentage(p)) =>
                {
                    wire::SizeUnit { kind:  2,
                                     value: if p == 0.0 { 0.0f32.to_bits() } else { p.to_bits() } }
                }
            }
        };
        Ok(wire::SizeBounds { min:      unit(self.min),
                              max:      unit(self.max),
                              fill:     u32::from(self.fill == Fill::Fill),
                              reserved: 0 })
    }

    pub fn decode(v: wire::SizeBounds) -> Result<Self>
    {
        fn unit(v: wire::SizeUnit) -> Result<Option<SizeUnit>>
        {
            Ok(match (v.kind, v.value)
            {
                (0, 0) => None,
                (1, 0..=32767) => Some(SizeUnit::Pixels(v.value as i16)),
                (2, bits) => Some(SizeUnit::Percentage(f32::from_bits(bits))),
                _ => return Err(Error::InvalidEncoding)
            })
        }
        if v.fill > 1 || v.reserved != 0
        {
            return Err(Error::InvalidEncoding);
        }
        Self::new(unit(v.min)?, unit(v.max)?, if v.fill == 0 { Fill::None } else { Fill::Fill })
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
