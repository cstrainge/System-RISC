//! Minimal terminal failure UI. This deliberately enters no Window/Control
//! Manager, allocator, context, registry or service while System is failed.
//! Machine code owns exclusive, boot-pinned pixels/input and terminal Restart.
//! This is one consumer, not an installed general emergency-service reserve.
#![forbid(unsafe_code)]
use core::fmt::{self, Write};

use mac_abi::{geometry::{Point, Rect},
              definitions::DefinitionPanic, quickdraw::MonoCursor, theme::Palette};

use crate::toolbox::{events::input::Input,
                     quickdraw::{Canvas, font::CHICAGO_12}};

const BOMB: &[u8; 128] = include_bytes!("../assets/crashes/classic-bomb.icon");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind
{
    SystemPanic,
    StartupRefused,
    OutOfMemory,
    PrivilegedFault,
    /// The machine has no power-management device, so System cannot switch it
    /// off. It shows the classic notice instead of pretending to power down.
    SafeToTurnOff
}
impl Kind
{
    fn title(self) -> &'static str
    {
        match self
        {
            Self::SystemPanic => "System Panic",
            Self::StartupRefused => "System Startup Failed",
            Self::OutOfMemory => "System Out of Memory",
            Self::PrivilegedFault => "System Privileged Fault",
            Self::SafeToTurnOff => "Shut Down"
        }
    }
}

/// The existing definition diagnostic capacity is the bound, not a second
/// independently chosen emergency text capacity. Raw UTF-8 survives unchanged.
pub(crate) struct Record
{
    kind:       Kind,
    diagnostic: DefinitionPanic
}
impl Record
{
    /// The supplied formatting implementations must themselves avoid allocation
    /// and service calls; this bounded sink cannot make arbitrary Display code
    /// failure-safe. Native terminal entry guards recursive failure separately.
    pub(crate) fn format(kind: Kind, arguments: fmt::Arguments<'_>) -> Self
    {
        let mut record = Self { kind,
                                diagnostic: DefinitionPanic::default() };
        if fmt::write(&mut record, arguments).is_err()
        {
            record.diagnostic.flags |= DefinitionPanic::FORMAT_FAILED;
        }
        record
    }

    pub(crate) fn kind(&self) -> Kind
    {
        self.kind
    }

    pub(crate) fn flags(&self) -> u32
    {
        self.diagnostic.flags
    }

    pub(crate) fn text(&self) -> &str
    {
        // All writes copy whole UTF-8 prefixes. A fallible decode also keeps a
        // future malformed internal record from recursively panicking here.
        core::str::from_utf8(&self.diagnostic.message[..self.diagnostic.length as usize])
            .unwrap_or("Diagnostic encoding failed")
    }
}
impl Write for Record
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        // Preserve a prefix, never append a later formatter fragment after an
        // earlier fragment was truncated.
        if self.diagnostic.flags & DefinitionPanic::TRUNCATED != 0
        {
            return Ok(());
        }
        let start = self.diagnostic.length as usize;
        let mut count = text.len().min(self.diagnostic.message.len() - start);
        while !text.is_char_boundary(count)
        {
            count -= 1;
        }
        self.diagnostic.message[start..start + count].copy_from_slice(&text.as_bytes()[..count]);
        self.diagnostic.length += count as u32;
        if count != text.len()
        {
            self.diagnostic.flags |= DefinitionPanic::TRUNCATED;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GeometryError
{
    Geometry,
    Pixels,
    Cursor(mac_abi::Status)
}
impl fmt::Display for GeometryError
{
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        if let Self::Cursor(status) = self
        {
            return write!(output,
                          "bundled terminal arrow resource is malformed (status {status})");
        }
        output.write_str(match self
                         {
                             Self::Geometry =>
                             {
                                 "terminal display requires width >= 360, height >= 220, and \
                                  signed 16-bit coordinates"
                             }
                             Self::Pixels =>
                             {
                                 "terminal pixel buffer does not match its admitted display \
                                  geometry"
                             }
                             Self::Cursor(_) => "bundled terminal arrow resource is malformed"
                         })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action
{
    Restart
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Update
{
    pub(crate) damage: Option<Rect>,
    pub(crate) action: Option<Action>
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tracking
{
    None,
    Pointer,
    Key(u8)
}

pub(crate) struct Screen
{
    width:    usize,
    height:   usize,
    pixels:   usize,
    palette:  Palette,
    panel:    Rect,
    body:     Rect,
    restart:  Rect,
    tracking: Tracking,
    pressed:  bool
}
fn geometry(width: usize, height: usize) -> Result<usize, GeometryError>
{
    if width < 360 || height < 220 || width > i16::MAX as usize || height > i16::MAX as usize
    {
        return Err(GeometryError::Geometry);
    }
    width.checked_mul(height).ok_or(GeometryError::Geometry)
}
impl Screen
{
    pub(crate) fn new(width: usize, height: usize, palette: Palette)
                      -> Result<Self, GeometryError>
    {
        let pixels = geometry(width, height)?;
        let w = (width as i16 - 32).min(580);
        let h = (height as i16 - 32).min(404);
        let left = (width as i16 - w) / 2;
        let top = (height as i16 - h) / 2;
        let panel = Rect::new(left, top, left + w, top + h);
        Ok(Self { width,
                  height,
                  pixels,
                  palette,
                  panel,
                  body: Rect::new(left + 20, top + 66, panel.right - 20, panel.bottom - 68),
                  restart: Rect::new(panel.right - 106,
                                     panel.bottom - 34,
                                     panel.right - 22,
                                     panel.bottom - 12),
                  tracking: Tracking::None,
                  pressed: false })
    }

    pub(crate) fn bounds(&self) -> Rect
    {
        self.panel
    }

    pub(crate) fn restart_bounds(&self) -> Rect
    {
        self.restart
    }

    pub(crate) fn draw(&self, record: &Record, pixels: &mut [u32]) -> Result<(), GeometryError>
    {
        self.draw_damage(record, pixels, Rect::new(0, 0, self.width as i16, self.height as i16))
    }

    pub(crate) fn draw_damage(&self, record: &Record, pixels: &mut [u32], damage: Rect)
                              -> Result<(), GeometryError>
    {
        if pixels.len() != self.pixels
        {
            return Err(GeometryError::Pixels);
        }
        // Constructor and exact buffer validation prove Canvas's assertions.
        let mut canvas = Canvas::new(pixels, self.width, self.height);
        canvas.set_clip(damage);
        if intersection(damage, self.restart) == damage
        {
            self.draw_restart(&mut canvas);
            return Ok(());
        }
        let full = Rect::new(0, 0, self.width as i16, self.height as i16);
        canvas.paint_rect(full, self.palette.scrollbar.face);
        canvas.paint_rect(self.panel, self.palette.content);
        canvas.frame_rect(self.panel, self.palette.control_outline);
        let inner = Rect::new(self.panel.left + 2,
                              self.panel.top + 2,
                              self.panel.right - 2,
                              self.panel.bottom - 2);
        canvas.frame_rect(inner, self.palette.control_outline);
        let (left, top) = (self.panel.left + 20, self.panel.top + 20);
        for y in 0..32usize
        {
            for x in 0..32usize
            {
                if BOMB[y * 4 + x / 8] & (0x80 >> (x & 7)) != 0
                {
                    canvas.paint_rect(Rect::new(left + x as i16,
                                                top + y as i16,
                                                left + x as i16 + 1,
                                                top + y as i16 + 1),
                                      self.palette.text);
                }
            }
        }
        CHICAGO_12.draw(&mut canvas, left + 48, top + 16, record.kind.title(), self.palette.text);
        let previous_clip = canvas.clip();
        let body_clip = intersection(previous_clip, self.body);
        canvas.set_clip(body_clip);
        let mut text = Text { canvas:      &mut canvas,
                              bounds:      self.body,
                              x:           self.body.left,
                              baseline:    self.body.top + 12,
                              column:      0,
                              previous_cr: false,
                              omitted:     false,
                              color:       self.palette.text };
        // Our writer cannot fail; excessive viewport text is visibly reported.
        let _ = text.write_str(record.text());
        let omitted = text.omitted;
        canvas.set_clip(previous_clip);
        let notice = if record.kind() == Kind::SafeToTurnOff
        {
            // The machine cannot switch itself off, so it says so plainly and
            // offers Restart rather than claiming a power-down it cannot do.
            "This computer is safe to turn off. Use Restart to start it again."
        }
        else if record.flags()
                        & (DefinitionPanic::FORMAT_FAILED | DefinitionPanic::TRUNCATED)
                        == DefinitionPanic::FORMAT_FAILED | DefinitionPanic::TRUNCATED
        {
            "Formatting failed; diagnostic truncated."
        }
        else if record.flags() & DefinitionPanic::FORMAT_FAILED != 0
        {
            "Formatting failed; partial diagnostic."
        }
        else if record.flags() & DefinitionPanic::TRUNCATED != 0
        {
            "Diagnostic truncated at reserved capacity."
        }
        else if omitted
        {
            "More diagnostic text than fits on screen."
        }
        else
        {
            "The System cannot continue. Restart the computer."
        };
        CHICAGO_12.draw_clipped(&mut canvas,
                                self.body.left,
                                self.panel.bottom - 49,
                                notice,
                                self.body.right - self.body.left,
                                self.palette.secondary_text);
        self.draw_restart(&mut canvas);
        Ok(())
    }

    fn draw_restart(&self, canvas: &mut Canvas<'_>)
    {
        let ring = Rect::new(self.restart.left - 4,
                             self.restart.top - 4,
                             self.restart.right + 4,
                             self.restart.bottom + 4);
        rounded(canvas, ring, self.palette.control_outline);
        rounded(canvas,
                Rect::new(ring.left + 2, ring.top + 2, ring.right - 2, ring.bottom - 2),
                self.palette.content);
        rounded(canvas, self.restart, self.palette.control_outline);
        let inside = Rect::new(self.restart.left + 1,
                               self.restart.top + 1,
                               self.restart.right - 1,
                               self.restart.bottom - 1);
        rounded(canvas,
                inside,
                if self.pressed { self.palette.control_outline } else { self.palette.content });
        let x = self.restart.left
                + (self.restart.right - self.restart.left - CHICAGO_12.width("Restart")) / 2;
        CHICAGO_12.draw(canvas,
                        x,
                        self.restart.top + 15,
                        "Restart",
                        if self.pressed { self.palette.selected_text } else { self.palette.text });
    }

    /// A retired device cannot deliver its matching release. Drop the tracking
    /// claim explicitly so a surviving keyboard or pointer can start afresh.
    pub(crate) fn cancel_tracking(&mut self) -> Update
    {
        let was_pressed = self.pressed;
        self.tracking = Tracking::None;
        self.pressed = false;
        Update { damage: was_pressed.then_some(self.restart),
                 action: None }
    }

    pub(crate) fn handle(&mut self, input: Input) -> Update
    {
        let was_pressed = self.pressed;
        let mut action = None;
        match input
        {
            Input::Move(point) if self.tracking == Tracking::Pointer =>
            {
                self.pressed = self.restart.contains(point)
            }
            Input::Button { point,
                            down: true }
                if self.tracking == Tracking::None =>
            {
                if self.restart.contains(point)
                {
                    self.tracking = Tracking::Pointer;
                    self.pressed = true;
                }
            }
            Input::Button { point,
                            down: false }
                if self.tracking == Tracking::Pointer =>
            {
                if self.pressed && self.restart.contains(point)
                {
                    action = Some(Action::Restart);
                }
                self.tracking = Tracking::None;
                self.pressed = false;
            }
            Input::Key { mac_code,
                         down: true,
                         repeat: false,
                         .. } if self.tracking == Tracking::None && matches!(mac_code, 0x24 | 0x4c | 0x31) =>
            {
                self.tracking = Tracking::Key(mac_code);
                self.pressed = true;
            }
            Input::Key { mac_code,
                         down: false,
                         .. } if self.tracking == Tracking::Key(mac_code) =>
            {
                self.tracking = Tracking::None;
                self.pressed = false;
                action = Some(Action::Restart);
            }
            Input::Key { mac_code: 0x35,
                         down: true,
                         .. } =>
            {
                self.tracking = Tracking::None;
                self.pressed = false;
            }
            _ =>
            {}
        }
        Update { damage: (was_pressed != self.pressed).then_some(self.restart),
                 action }
    }
}

/// Private terminal overlay. The owner must hide it before changing the pixels
/// underneath, then restore it at the saved position after that bounded paint.
/// Boot validates and copies the actual ROM cursor; motion never decodes it.
pub(crate) struct Cursor
{
    width:    usize,
    height:   usize,
    pixels:   usize,
    palette:  Palette,
    arrow:    MonoCursor,
    saved:    [u32; 16 * 16],
    position: Option<Point>
}
impl Cursor
{
    pub(crate) fn new(width: usize, height: usize, palette: Palette)
                      -> Result<Self, GeometryError>
    {
        let pixels = geometry(width, height)?;
        let arrow = crate::toolbox::quickdraw::cursors::decode_mono(
            include_bytes!("../assets/cursors/arrow.curs")).map_err(GeometryError::Cursor)?;
        Ok(Self { width,
                  height,
                  pixels,
                  palette,
                  arrow,
                  saved: [0; 16 * 16],
                  position: None })
    }

    pub(crate) fn position(&self) -> Option<Point>
    {
        self.position
    }

    fn clipped(&self, point: Point) -> (i32, i32, Rect)
    {
        let left = i32::from(point.h) - i32::from(self.arrow.hotspot.h);
        let top = i32::from(point.v) - i32::from(self.arrow.hotspot.v);
        let x = |value: i32| value.clamp(0, self.width as i32) as i16;
        let y = |value: i32| value.clamp(0, self.height as i32) as i16;
        (left, top, Rect::new(x(left), y(top), x(left + 16), y(top + 16)))
    }

    /// Restores at most 256 pixels. A hidden cursor performs no writes.
    pub(crate) fn hide(&mut self, pixels: &mut [u32]) -> Result<bool, GeometryError>
    {
        if pixels.len() != self.pixels
        {
            return Err(GeometryError::Pixels);
        }
        let Some(point) = self.position.take()
        else
        {
            return Ok(false);
        };
        let (left, top, clip) = self.clipped(point);
        for y in clip.top..clip.bottom
        {
            for x in clip.left..clip.right
            {
                let saved = (i32::from(y) - top) as usize * 16 + (i32::from(x) - left) as usize;
                pixels[y as usize * self.width + x as usize] = self.saved[saved];
            }
        }
        Ok(true)
    }

    /// Restore old pixels, then capture and draw the new clipped 16×16 box.
    /// Identical visible positions perform no reads/writes of framebuffer data.
    pub(crate) fn move_to(&mut self, pixels: &mut [u32], point: Point)
                          -> Result<bool, GeometryError>
    {
        if pixels.len() != self.pixels
        {
            return Err(GeometryError::Pixels);
        }
        if self.position == Some(point)
        {
            return Ok(false);
        }
        self.hide(pixels)?;
        let (left, top, clip) = self.clipped(point);
        for y in clip.top..clip.bottom
        {
            for x in clip.left..clip.right
            {
                let row = (i32::from(y) - top) as usize;
                let column = (i32::from(x) - left) as usize;
                let pixel = &mut pixels[y as usize * self.width + x as usize];
                self.saved[row * 16 + column] = *pixel;
                let bit = 0x8000 >> column;
                let image = self.arrow.image[row] & bit != 0;
                let mask = self.arrow.mask[row] & bit != 0;
                if mask
                {
                    *pixel = if image { self.palette.text } else { self.palette.content };
                }
                else if image
                {
                    *pixel ^= self.palette.text ^ self.palette.content;
                }
            }
        }
        self.position = Some(point);
        Ok(true)
    }
}

fn intersection(a: Rect, b: Rect) -> Rect
{
    Rect::new(a.left.max(b.left), a.top.max(b.top), a.right.min(b.right), a.bottom.min(b.bottom))
}
/// Small fixed-radius button silhouette, independent of loaded CDEF execution.
fn rounded(canvas: &mut Canvas<'_>, rect: Rect, color: u32)
{
    for y in rect.top..rect.bottom
    {
        let edge = (y - rect.top).min(rect.bottom - 1 - y);
        let inset = match edge
        {
            0 => 3,
            1 => 1,
            _ => 0
        };
        canvas.paint_rect(Rect::new(rect.left + inset, y, rect.right - inset, y + 1), color);
    }
}

struct Text<'a, 'pixels>
{
    canvas:      &'a mut Canvas<'pixels>,
    bounds:      Rect,
    x:           i16,
    baseline:    i16,
    column:      usize,
    previous_cr: bool,
    omitted:     bool,
    color:       u32
}
impl Text<'_, '_>
{
    fn newline(&mut self)
    {
        self.x = self.bounds.left;
        self.baseline = self.baseline.saturating_add(16);
        self.column = 0;
    }

    fn ascii(&mut self, character: char)
    {
        let mut bytes = [0; 4];
        let glyph = character.encode_utf8(&mut bytes);
        let width = CHICAGO_12.width(glyph);
        if self.x + width > self.bounds.right
        {
            self.newline();
        }
        if self.baseline + 3 > self.bounds.bottom
        {
            self.omitted = true;
            return;
        }
        CHICAGO_12.draw(self.canvas, self.x, self.baseline, glyph, self.color);
        self.x += width;
        self.column += 1;
    }
}
impl Write for Text<'_, '_>
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        for character in text.chars()
        {
            if self.omitted
            {
                break;
            }
            if character == '\n' && self.previous_cr
            {
                self.previous_cr = false;
                continue;
            }
            self.previous_cr = character == '\r';
            match character
            {
                '\r' | '\n' => self.newline(),
                '\t' =>
                {
                    for _ in 0..4 - self.column % 4
                    {
                        self.ascii(' ');
                    }
                }
                ' '..='~' => self.ascii(character),
                _ =>
                {
                    write!(self, "U+{:04X}", u32::from(character))?;
                }
            }
        }
        Ok(())
    }
}

