//! Finder-owned desktop graphics with complete or masked atomic updates.
use mac_abi::quickdraw::*;

use crate::{Client, QuickDraw, Rect, Result, WindowId, graphics::GraphicsPort};

#[derive(Clone, Copy)]
pub struct DesktopPort<'a>
{
    client: Client<'a>
}
impl<'a> DesktopPort<'a>
{
    /// Bounds use screen coordinates; the menu bar is outside this port.
    pub fn bounds(self) -> Result<Rect>
    {
        self.client.desktop(QD_DESKTOP_BOUNDS)
    }

    /// Begin a fresh transparent scene and reset its graphics state.
    /// The previous committed scene stays visible until end_update succeeds.
    pub fn begin_update(self) -> Result<Rect>
    {
        self.client.desktop(QD_DESKTOP_BEGIN)
    }

    pub fn end_update(self) -> Result<()>
    {
        self.client.desktop(QD_DESKTOP_END).map(|_| ())
    }

    pub fn cancel_update(self) -> Result<()>
    {
        self.client.desktop(QD_DESKTOP_CANCEL).map(|_| ())
    }

    pub const fn window_id(self) -> WindowId
    {
        WindowId(DESKTOP_TARGET)
    }

    /// Reuse drawing, masked updates and transient outlines with this target ID.
    /// Coordinates remain global, including for masks. System clips publication
    /// beneath visible window structures and preserves the desktop background.
    pub fn graphics(self) -> GraphicsPort<'a>
    {
        GraphicsPort::new(self.client, self.window_id())
    }
}
impl<'a> QuickDraw<'a>
{
    pub fn desktop(self) -> DesktopPort<'a>
    {
        DesktopPort { client: self.client }
    }
}
