use mac_abi::controls::reflow as wire;

use crate::{Dimensions, Error, Rect, Result};

/// A finite nonnegative local pixel extent. Unbounded constraints are a
/// separate variant; logical document extents must use a wider representation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Extent(i16);
impl Extent
{
    pub const ZERO: Self = Self(0);

    pub const fn new(value: i16) -> Result<Self>
    {
        if value < 0 { Err(Error::NegativeDimension) } else { Ok(Self(value)) }
    }

    pub const fn from_wide(value: i64) -> Result<Self>
    {
        if value < 0
        {
            Err(Error::NegativeDimension)
        }
        else if value > i16::MAX as i64
        {
            Err(Error::ExtentOverflow)
        }
        else
        {
            Ok(Self(value as i16))
        }
    }

    pub const fn pixels(self) -> i16
    {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size
{
    width:  Extent,
    height: Extent
}
impl Size
{
    pub const ZERO: Self = Self { width:  Extent::ZERO,
                                  height: Extent::ZERO };

    pub const fn new(width: Extent, height: Extent) -> Self
    {
        Self { width,
               height }
    }

    pub const fn width(self) -> Extent
    {
        self.width
    }

    pub const fn height(self) -> Extent
    {
        self.height
    }

    pub const fn dimensions(self) -> Dimensions
    {
        Dimensions { width:  self.width.0,
                     height: self.height.0 }
    }

    pub(crate) fn max(self, other: Self) -> Self
    {
        Self { width:  self.width.max(other.width),
               height: self.height.max(other.height) }
    }
}
impl TryFrom<Dimensions> for Size
{
    type Error = Error;

    fn try_from(value: Dimensions) -> Result<Self>
    {
        Ok(Self::new(Extent::new(value.width)?, Extent::new(value.height)?))
    }
}
impl From<Size> for Dimensions
{
    fn from(value: Size) -> Self
    {
        value.dimensions()
    }
}

/// Valid local rectangle, including zero area. Differences are checked in
/// wider arithmetic; two valid i16 endpoints need not form a valid local size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutRect(Rect);
impl LayoutRect
{
    pub const fn rect(self) -> Rect
    {
        self.0
    }

    pub fn size(self) -> Size
    {
        // Construction checked both wider differences before this narrowing.
        Size::new(Extent((i32::from(self.0.right) - i32::from(self.0.left)) as i16),
                  Extent((i32::from(self.0.bottom) - i32::from(self.0.top)) as i16))
    }
}
impl TryFrom<Rect> for LayoutRect
{
    type Error = Error;

    fn try_from(value: Rect) -> Result<Self>
    {
        let width = i64::from(value.right) - i64::from(value.left);
        let height = i64::from(value.bottom) - i64::from(value.top);
        if width < 0 || height < 0
        {
            return Err(Error::InvertedRectangle);
        }
        Extent::from_wide(width)?;
        Extent::from_wide(height)?;
        Ok(Self(value))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AxisConstraint
{
    #[default]
    Unbounded,
    AtMost(Extent),
    Exact(Extent)
}
impl TryFrom<wire::AxisConstraint> for AxisConstraint
{
    type Error = Error;

    fn try_from(value: wire::AxisConstraint) -> Result<Self>
    {
        if value.reserved != 0
        {
            return Err(Error::InvalidEncoding);
        }
        match value.kind
        {
            wire::UNBOUNDED if value.extent == 0 => Ok(Self::Unbounded),
            wire::AT_MOST => Ok(Self::AtMost(Extent::new(value.extent)?)),
            wire::EXACT => Ok(Self::Exact(Extent::new(value.extent)?)),
            _ => Err(Error::InvalidEncoding)
        }
    }
}
impl From<AxisConstraint> for wire::AxisConstraint
{
    fn from(value: AxisConstraint) -> Self
    {
        let (kind, extent) = match value
        {
            AxisConstraint::Unbounded => (wire::UNBOUNDED, 0),
            AxisConstraint::AtMost(extent) => (wire::AT_MOST, extent.pixels()),
            AxisConstraint::Exact(extent) => (wire::EXACT, extent.pixels())
        };
        Self { kind,
               extent,
               reserved: 0 }
    }
}

/// Intrinsic queries use two unbounded axes. Width-dependent measurement uses
/// an explicit resolved width; it does not change intrinsic requirements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Constraints
{
    pub width:  AxisConstraint,
    pub height: AxisConstraint
}
impl TryFrom<wire::Constraints> for Constraints
{
    type Error = Error;

    fn try_from(value: wire::Constraints) -> Result<Self>
    {
        Ok(Self { width:  value.width.try_into()?,
                  height: value.height.try_into()? })
    }
}
impl From<Constraints> for wire::Constraints
{
    fn from(value: Constraints) -> Self
    {
        Self { width:  value.width.into(),
               height: value.height.into() }
    }
}
