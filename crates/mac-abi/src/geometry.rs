//! QuickDraw rectangle subset. Coordinates remain signed 16-bit values.
//! Host layout is native endian; explicit codecs define the 68k memory boundary.

/// Pixel dimensions on the native wire, width first. Negative values are
/// invalid for layout and must be checked before entering control author code.
/// This is a finite local size, not a scrollable document extent or an
/// unbounded measurement constraint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Dimensions
{
    pub width:  i16,
    pub height: i16
}
impl Dimensions
{
    pub const ZERO: Self = Self { width:  0,
                                  height: 0 };
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Point
{
    pub v: i16,
    pub h: i16
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Rect
{
    pub top:    i16,
    pub left:   i16,
    pub bottom: i16,
    pub right:  i16
}

impl Rect
{
    pub const ZERO: Self = Self::new(0, 0, 0, 0);

    /// SetRect argument order is horizontal, vertical; storage is vertical first.
    pub const fn new(left: i16, top: i16, right: i16, bottom: i16) -> Self
    {
        Self { top,
               left,
               bottom,
               right }
    }

    pub fn is_empty(self) -> bool
    {
        self.left >= self.right || self.top >= self.bottom
    }

    pub fn offset(&mut self, dh: i16, dv: i16)
    {
        self.left = self.left.wrapping_add(dh);
        self.right = self.right.wrapping_add(dh);
        self.top = self.top.wrapping_add(dv);
        self.bottom = self.bottom.wrapping_add(dv);
    }

    pub fn inset(&mut self, dh: i16, dv: i16)
    {
        self.left = self.left.wrapping_add(dh);
        self.right = self.right.wrapping_sub(dh);
        self.top = self.top.wrapping_add(dv);
        self.bottom = self.bottom.wrapping_sub(dv);
        if self.is_empty()
        {
            *self = Self::ZERO;
        }
    }

    pub fn contains(self, point: Point) -> bool
    {
        point.h >= self.left && point.h < self.right && point.v >= self.top && point.v < self.bottom
    }

    pub fn from_be_bytes(bytes: [u8; 8]) -> Self
    {
        Self { top:    i16::from_be_bytes([bytes[0], bytes[1]]),
               left:   i16::from_be_bytes([bytes[2], bytes[3]]),
               bottom: i16::from_be_bytes([bytes[4], bytes[5]]),
               right:  i16::from_be_bytes([bytes[6], bytes[7]]) }
    }

    pub fn to_be_bytes(self) -> [u8; 8]
    {
        let [t0, t1] = self.top.to_be_bytes();
        let [l0, l1] = self.left.to_be_bytes();
        let [b0, b1] = self.bottom.to_be_bytes();
        let [r0, r1] = self.right.to_be_bytes();
        [t0, t1, l0, l1, b0, b1, r0, r1]
    }
}
