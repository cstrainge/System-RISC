//! Cursor visibility and image changes are resident services.
pub use mac_abi::quickdraw::{CURSOR_ANIMATED, CURSOR_AUTOMATIC, CURSOR_COLOR,
                             CURSOR_DEFERRED_HIDE, CURSOR_OBSCURED, CURSOR_VISIBLE,
                             CursorSnapshot, MonoCursor};
use mac_abi::resources::{APPLICATION, CHAIN, SYSTEM};

/// Original System cursor resources. Arrow restores automatic editor cursor selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum StandardCursor
{
    Arrow            = 0,
    IBeam            = 1,
    Crosshair        = 2,
    Plus             = 3,
    Watch            = 4,
    /// Native resource using the Appearance horizontal-resize cursor identity.
    ResizeHorizontal = 17,
    /// Native resource using the later Carbon vertical-resize cursor identity.
    ResizeVertical   = 21
}
use mac_abi::quickdraw::*;

use crate::{Client, Point, Rect, Result};
#[derive(Clone, Copy)]
pub struct Cursors<'a>
{
    client: Client<'a>,
    scope:  mac_abi::resources::Scope
}
impl<'a> Cursors<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client,
               scope: CHAIN }
    }

    pub const fn system(self) -> Self
    {
        Self { scope: SYSTEM,
               ..self }
    }

    pub const fn application(self) -> Self
    {
        Self { scope: APPLICATION,
               ..self }
    }

    pub const fn search_chain(self) -> Self
    {
        Self { scope: CHAIN,
               ..self }
    }

    pub fn set_standard(self, cursor: StandardCursor) -> Result<()>
    {
        self.client
            .cursor_resource(CursorResourceCall { operation: CURSOR_RESOURCE_BUILTIN,
                                                  builtin: cursor as u32,
                                                  ..Default::default() })
            .map(|_| ())
    }

    /// Select an original 68-byte CURS resource from the selected resource scope.
    pub fn set_resource(self, id: i16) -> Result<()>
    {
        self.resource(CURSOR_RESOURCE_SET, id, 0).map(|_| ())
    }

    /// Select an indexed monochrome/color crsr resource.
    pub fn set_color_resource(self, id: i16) -> Result<()>
    {
        self.resource(CURSOR_RESOURCE_COLOR, id, 0).map(|_| ())
    }

    /// Load an acur resource atomically and display its first frame. An interval of
    /// zero advances only when advance_animation is called; other intervals use 60 Hz ticks.
    /// The selected scope locates the acur resource; its frames resolve there and then
    /// in System resources. Color crsr frames take precedence over monochrome CURS frames.
    pub fn animate_resource(self, id: i16, interval_ticks: u32) -> Result<()>
    {
        self.resource(CURSOR_RESOURCE_ANIMATE, id, interval_ticks).map(|_| ())
    }

    pub fn advance_animation(self) -> Result<()>
    {
        self.resource(CURSOR_RESOURCE_ADVANCE, 0, 0).map(|_| ())
    }

    /// Inspect the selected image, visibility nesting, and animation state without redrawing.
    pub fn info(self) -> Result<CursorSnapshot>
    {
        self.resource(CURSOR_RESOURCE_STATE, 0, 0)
    }

    fn resource(self, operation: u32, id: i16, interval: u32) -> Result<CursorSnapshot>
    {
        self.client.cursor_resource(CursorResourceCall { operation,
                                                         scope: self.scope,
                                                         id,
                                                         interval,
                                                         ..Default::default() })
    }

    pub fn set(self, cursor: MonoCursor) -> Result<()>
    {
        self.client.cursor(CURSOR_SET, cursor, Rect::ZERO, Point::default(), &[]).map(|_| ())
    }

    pub fn set_color(self, cursor: MonoCursor, colors: &[u32]) -> Result<()>
    {
        self.client.cursor(CURSOR_SET, cursor, Rect::ZERO, Point::default(), colors).map(|_| ())
    }

    pub fn init(self) -> Result<()>
    {
        self.operation(CURSOR_INIT).map(|_| ())
    }

    fn operation(self, operation: u32) -> Result<i32>
    {
        self.client.cursor(operation, Default::default(), Rect::ZERO, Point::default(), &[])
    }

    pub fn hide(self) -> Result<()>
    {
        self.operation(CURSOR_HIDE).map(|_| ())
    }

    pub fn show(self) -> Result<()>
    {
        self.operation(CURSOR_SHOW).map(|_| ())
    }

    pub fn obscure(self) -> Result<()>
    {
        self.operation(CURSOR_OBSCURE).map(|_| ())
    }

    pub fn visibility(self) -> Result<i32>
    {
        self.operation(CURSOR_STATE)
    }

    pub fn shield(self, bounds: Rect, offset: Point) -> Result<()>
    {
        self.client.cursor(CURSOR_SHIELD, Default::default(), bounds, offset, &[]).map(|_| ())
    }

    pub fn from_resource(bytes: &[u8]) -> Option<MonoCursor>
    {
        if bytes.len() != 68
        {
            return None;
        }
        let mut c = MonoCursor::default();
        for i in 0..16
        {
            c.image[i] = u16::from_be_bytes([bytes[2 * i], bytes[2 * i + 1]]);
            c.mask[i] = u16::from_be_bytes([bytes[32 + 2 * i], bytes[33 + 2 * i]]);
        }
        c.hotspot = Point { v: i16::from_be_bytes([bytes[64], bytes[65]]),
                            h: i16::from_be_bytes([bytes[66], bytes[67]]) };
        if !(0..16).contains(&c.hotspot.h) || !(0..16).contains(&c.hotspot.v)
        {
            return None;
        }
        Some(c)
    }
}
impl<'a> crate::QuickDraw<'a>
{
    pub fn cursors(self) -> Cursors<'a>
    {
        Cursors::new(self.client)
    }
}
