//! Cursor visibility and image changes are resident services.
pub use mac_abi::quickdraw::MonoCursor;
use mac_abi::quickdraw::*;

use crate::{Client, Point, Rect, Result};
#[derive(Clone, Copy)]
pub struct Cursors<'a>
{
    client: Client<'a>
}
impl<'a> Cursors<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
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
