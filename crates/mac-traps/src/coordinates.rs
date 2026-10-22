//! Explicit coordinate spaces for native input. Conversion to geometry is deliberate.
#![forbid(unsafe_code)]
use mac_abi::{Point, WindowId, events::EventPosition};

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScreenPoint(Point);
impl ScreenPoint
{
    pub const fn new(h: i16, v: i16) -> Self
    {
        Self(Point { h,
                     v })
    }

    pub const fn point(self) -> Point
    {
        self.0
    }
}

/// A content-relative position tied to one complete generation-tagged WindowId.
/// It can lie outside the content rectangle during captured pointer tracking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowPoint(EventPosition);
impl WindowPoint
{
    pub const fn window(self) -> WindowId
    {
        WindowId(self.0.window)
    }

    pub const fn h(self) -> i32
    {
        self.0.h
    }

    pub const fn v(self) -> i32
    {
        self.0.v
    }

    /// Checked conversion to QuickDraw's signed 16-bit geometry.
    pub fn point(self) -> Result<Point>
    {
        Ok(Point { h: self.0.h.try_into().map_err(|_| Error::BadArgument)?,
                   v: self.0.v.try_into().map_err(|_| Error::BadArgument)? })
    }
}

/// Positions captured by System in the same operation as the associated event.
/// No implicit conversion exists between screen and window coordinate spaces.
///
/// ```compile_fail
/// use mac_traps::coordinates::{ScreenPoint, WindowPoint};
/// fn window_input(_: WindowPoint) {}
/// window_input(ScreenPoint::new(20, 30));
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PointerPosition
{
    screen: ScreenPoint,
    local:  Option<WindowPoint>
}
impl PointerPosition
{
    /// Explicit ABI escape hatch. Ordinary callers obtain this from Event Manager.
    #[doc(hidden)]
    pub const fn from_record(screen: Point, position: EventPosition) -> Self
    {
        Self { screen: ScreenPoint(screen),
               local:  if position.window == 0 { None } else { Some(WindowPoint(position)) } }
    }

    pub const fn screen(self) -> ScreenPoint
    {
        self.screen
    }

    pub const fn window(self) -> Option<WindowPoint>
    {
        self.local
    }

    /// Reject a point belonging to another window, including an earlier generation.
    pub fn in_window(self, window: WindowId) -> Result<WindowPoint>
    {
        self.local.filter(|point| point.window() == window).ok_or(Error::NotFound)
    }
}
