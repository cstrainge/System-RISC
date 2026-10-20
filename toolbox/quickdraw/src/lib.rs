//! QuickDraw client bindings; all rasterization and font data live in System.
#![no_std]
#![forbid(unsafe_code)]
pub mod graphics;
pub mod desktop;
pub mod fonts;
pub mod regions;
pub mod bitmaps;
pub mod worlds;
pub mod cursors;
pub mod icons;
pub mod pictures;
pub mod images;
use mac_abi::*;
pub use mac_abi::{DrawCommand, Point, Rect, WindowId};
pub use mac_traps::{Client, Error, Result};
pub const BLACK: u32 = 0;
pub const WHITE: u32 = 0xffffff;
#[derive(Clone, Copy)]
pub struct QuickDraw<'a>
{
    client: Client<'a>
}
impl<'a> QuickDraw<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    pub fn port(self, window: WindowId) -> Port<'a>
    {
        Port { client: self.client,
               window }
    }

    pub fn intersect(self, a: Rect, b: Rect) -> Result<Option<Rect>>
    {
        let mut call = traps::GeometryCall { operation: 0,
                                             a,
                                             b,
                                             ..Default::default() };
        self.client.geometry(&mut call)?;
        Ok(if call.result != 0 { Some(call.output) } else { None })
    }

    pub fn union(self, a: Rect, b: Rect) -> Result<Rect>
    {
        let mut call = traps::GeometryCall { operation: 1,
                                             a,
                                             b,
                                             ..Default::default() };
        self.client.geometry(&mut call)?;
        Ok(call.output)
    }
}
#[derive(Clone, Copy)]
pub struct Port<'a>
{
    client: Client<'a>,
    window: WindowId
}
impl Port<'_>
{
    pub fn paint_rect(self, bounds: Rect, color: u32) -> Result<()>
    {
        self.command(PAINT_RECT, bounds, color, "")
    }

    pub fn frame_rect(self, bounds: Rect, color: u32) -> Result<()>
    {
        self.command(FRAME_RECT, bounds, color, "")
    }

    pub fn text(self, point: Point, text: &str, color: u32) -> Result<()>
    {
        self.command(DRAW_TEXT, Rect::new(point.h, point.v, point.h, point.v), color, text)
    }

    pub fn command(self, kind: u32, bounds: Rect, color: u32, text: &str) -> Result<()>
    {
        let mut command = DrawCommand { kind,
                                        bounds,
                                        color,
                                        text_length: 0,
                                        text: [0; mac_abi::INLINE_STRING_BYTES] };
        let n = text.len();
        if n > command.text.len()
        {
            return Err(Error::BadArgument);
        }
        command.text[..n].copy_from_slice(&text.as_bytes()[..n]);
        command.text_length = n as u32;
        self.client.draw(self.window.0, command)
    }
}

/// A borrowed definition drawing surface; drawing and fonts are OS services.
pub struct SurfacePort<'a>
{
    connection: mac_traps::SurfaceConnection<'a>
}
impl<'a> SurfacePort<'a>
{
    pub const fn new(connection: mac_traps::SurfaceConnection<'a>) -> Self
    {
        Self { connection }
    }

    /// Draw a resource icon through the resident Icon Utilities service.
    pub fn icon_resource(&mut self, scope: i16, id: i16, bounds: Rect, flags: u16) -> Result<()>
    {
        self.connection.icon(scope, id, bounds, flags)
    }

    pub fn paint_rect(&mut self, bounds: Rect, color: u32) -> Result<()>
    {
        self.connection
            .perform(mac_traps::SurfaceCommand { operation: 0,
                                                 bounds,
                                                 foreground: color,
                                                 ..Default::default() })
            .map(|_| ())
    }

    pub fn fill_rect(&mut self, bounds: Rect, pattern: [u8; 8], foreground: u32, background: u32)
                     -> Result<()>
    {
        self.connection
            .perform(mac_traps::SurfaceCommand { operation: 2,
                                                 bounds,
                                                 pattern,
                                                 foreground,
                                                 background,
                                                 ..Default::default() })
            .map(|_| ())
    }

    pub fn frame_rect(&mut self, bounds: Rect, color: u32) -> Result<()>
    {
        self.connection
            .perform(mac_traps::SurfaceCommand { operation: 1,
                                                 bounds,
                                                 foreground: color,
                                                 ..Default::default() })
            .map(|_| ())
    }

    pub fn invert_rect(&mut self, bounds: Rect) -> Result<()>
    {
        self.connection
            .perform(mac_traps::SurfaceCommand { operation: 3,
                                                 bounds,
                                                 ..Default::default() })
            .map(|_| ())
    }

    pub fn styled_width(&mut self, style: u16, text: &str) -> Result<i16>
    {
        self.connection.font_width(mac_abi::quickdraw::FontSelection { family: 0,
                                                                       size:   12,
                                                                       face:   style },
                                   text)
    }

    pub fn styled_text(&mut self, style: u16, x: i16, baseline: i16, text: &str, color: u32)
                       -> Result<i16>
    {
        self.connection.font_draw(mac_abi::quickdraw::FontSelection { family: 0,
                                                                      size:   12,
                                                                      face:   style },
                                  Point { h: x,
                                          v: baseline },
                                  text,
                                  color)
    }

    pub fn text_width(&mut self, font: u32, text: &str) -> Result<i16>
    {
        Ok(self.connection
               .perform(mac_traps::SurfaceCommand { operation: 4,
                                                    font,
                                                    text,
                                                    ..Default::default() })?
               .width)
    }

    pub fn font_metrics(&mut self, font: u32) -> Result<FontMetrics>
    {
        let reply = self.connection.perform(mac_traps::SurfaceCommand { operation: 5,
                                                                         font,
                                                                         ..Default::default() })?;
        Ok(FontMetrics { ascent:  reply.ascent,
                         descent: reply.descent,
                         leading: reply.leading })
    }

    pub fn draw_text(&mut self, font: u32, x: i16, baseline: i16, text: &str, limit: i16,
                     color: u32)
                     -> Result<()>
    {
        self.connection
            .perform(mac_traps::SurfaceCommand { operation: 6,
                                                 font,
                                                 x,
                                                 y: baseline,
                                                 text,
                                                 limit,
                                                 foreground: color,
                                                 ..Default::default() })
            .map(|_| ())
    }
}
#[derive(Clone, Copy)]
pub struct FontMetrics
{
    pub ascent:  i16,
    pub descent: i16,
    pub leading: i16
}
pub mod font
{
    pub const CHICAGO_12: u32 = 0;
    pub const GENEVA_9: u32 = 1;
}

impl QuickDraw<'_>
{
    pub fn map_point(self, point: Point, source: Rect, destination: Rect) -> Result<Point>
    {
        self.client.map_point(point, source, destination, false)
    }

    pub fn scale_point(self, point: Point, numerator: Point, denominator: Point) -> Result<Point>
    {
        self.client.map_point(point,
                              Rect::new(0, 0, denominator.h, denominator.v),
                              Rect::new(0, 0, numerator.h, numerator.v),
                              true)
    }

    pub fn map_rect(self, rect: Rect, source: Rect, destination: Rect) -> Result<Rect>
    {
        self.client.map_rect(rect, source, destination)
    }

    pub fn map_polygon(self, points: &mut [Point], source: Rect, destination: Rect) -> Result<()>
    {
        self.client.map_points(points, source, destination)
    }
}
