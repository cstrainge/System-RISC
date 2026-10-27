//! Minimal terminal failure UI. This deliberately enters no Window/Control
//! Manager, allocator, context, registry or service while System is failed.
//! Machine code owns exclusive, boot-pinned pixels/input and terminal Restart.
//! This is one consumer, not an installed general emergency-service reserve.
#![forbid(unsafe_code)]
use core::fmt::{self, Write};

use mac_abi::{Point, Rect, definitions::DefinitionPanic, quickdraw::MonoCursor, theme::Palette};

use crate::toolbox::{events::input::Input,
                     quickdraw::{Canvas, font::CHICAGO_12}};

const BOMB: &[u8; 128] = include_bytes!("../assets/crashes/classic-bomb.icon");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind
{
    SystemPanic,
    OutOfMemory,
    PrivilegedFault
}
impl Kind
{
    fn title(self) -> &'static str
    {
        match self
        {
            Self::SystemPanic => "System Panic",
            Self::OutOfMemory => "System Out of Memory",
            Self::PrivilegedFault => "System Privileged Fault"
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
        let notice = if record.flags()
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

#[cfg(all(test, feature = "test-support"))]
mod tests
{
    use std::{string::ToString, vec};

    use mac_abi::Point;

    use super::*;
    use crate::testing::allocations::Observe;

    fn screen() -> Screen
    {
        Screen::new(800, 600, crate::toolbox::theme::palette()).unwrap()
    }
    fn key(code: u8, down: bool, repeat: bool) -> Input
    {
        Input::Key { mac_code: code,
                     character: 0,
                     down,
                     repeat,
                     modifiers: 0 }
    }
    #[test]
    fn diagnostic_retains_utf8_prefix_and_reports_both_failure_modes()
    {
        struct Overflow;
        impl fmt::Display for Overflow
        {
            fn fmt(&self, writer: &mut fmt::Formatter<'_>) -> fmt::Result
            {
                writer.write_str(core::str::from_utf8(&[b'x'; 1023]).unwrap())?;
                writer.write_str("é")?;
                writer.write_str("later fragments must not fill the gap")
            }
        }
        let record = Record::format(Kind::SystemPanic, format_args!("{Overflow}"));
        assert_eq!(record.kind(), Kind::SystemPanic);
        assert_eq!(record.text().len(), 1023);
        assert_eq!(record.flags(), DefinitionPanic::TRUNCATED);
        assert!(record.text().bytes().all(|byte| byte == b'x'));
        struct Broken;
        impl fmt::Display for Broken
        {
            fn fmt(&self, writer: &mut fmt::Formatter<'_>) -> fmt::Result
            {
                writer.write_str("partial 🦀 diagnostic")?;
                Err(fmt::Error)
            }
        }
        let record = Record::format(Kind::PrivilegedFault, format_args!("{Broken}"));
        assert_eq!(record.text(), "partial 🦀 diagnostic");
        assert_eq!(record.flags(), DefinitionPanic::FORMAT_FAILED);
        let both = Record::format(Kind::SystemPanic, format_args!("{Overflow}{Broken}"));
        assert_eq!(both.flags(), DefinitionPanic::TRUNCATED | DefinitionPanic::FORMAT_FAILED);
        assert_eq!(both.text().len(), 1023);
        let mut pixels = vec![0; 800 * 600];
        screen().draw(&record, &mut pixels).unwrap();
    }
    #[test]
    fn geometry_and_pixel_refusals_are_informative_and_leave_pixels_untouched()
    {
        for (width, height) in
            [(0, 0), (359, 600), (800, 219), (usize::MAX, 600), (800, usize::MAX)]
        {
            let error = Screen::new(width, height, crate::toolbox::theme::palette()).err().unwrap();
            assert_eq!(error, GeometryError::Geometry);
            assert!(error.to_string().contains("terminal display requires"));
        }
        let record =
            Record::format(Kind::OutOfMemory, format_args!("request={} align={}", 65536, 4096));
        let mut pixels = [123; 32];
        let error = screen().draw(&record, &mut pixels).unwrap_err();
        assert_eq!(error, GeometryError::Pixels);
        assert!(error.to_string().contains("admitted display geometry"));
        assert_eq!(pixels, [123; 32]);
        let mut pixels = vec![0; 360 * 220];
        Screen::new(360, 220, crate::toolbox::theme::palette()).unwrap()
                                                               .draw(&record, &mut pixels)
                                                               .unwrap();
    }
    #[test]
    fn pointer_and_default_keys_track_cancel_and_damage_only_restart()
    {
        let mut screen = screen();
        let record = Record::format(Kind::SystemPanic, format_args!("trusted diagnostic"));
        let mut pixels = vec![0; 800 * 600];
        screen.draw(&record, &mut pixels).unwrap();
        let baseline = pixels.clone();
        let bounds = screen.restart_bounds();
        let inside = Point { h: bounds.left + 2,
                             v: bounds.top + 2 };
        let outside = Point { h: bounds.left - 20,
                              v: bounds.top };
        assert_eq!(screen.handle(Input::Move(inside)), Update::default());
        assert_eq!(screen.handle(Input::Button { point: inside,
                                                 down:  false }),
                   Update::default());
        let update = screen.handle(Input::Button { point: inside,
                                                   down:  true });
        assert_eq!(update,
                   Update { damage: Some(bounds),
                            action: None });
        screen.draw_damage(&record, &mut pixels, update.damage.unwrap()).unwrap();
        assert!(pixels.iter().zip(&baseline).any(|(a, b)| a != b));
        for (index, (before, after)) in baseline.iter().zip(&pixels).enumerate()
        {
            if !bounds.contains(Point { h: (index % 800) as i16,
                                        v: (index / 800) as i16 })
            {
                assert_eq!(before, after);
            }
        }
        assert_eq!(screen.handle(Input::Move(inside)), Update::default());
        let update = screen.handle(Input::Move(outside));
        assert_eq!(update.damage, Some(bounds));
        screen.draw_damage(&record, &mut pixels, bounds).unwrap();
        assert_eq!(pixels, baseline);
        assert_eq!(screen.handle(Input::Button { point: outside,
                                                 down:  false }),
                   Update::default());
        screen.handle(Input::Button { point: inside,
                                      down:  true });
        assert_eq!(screen.handle(key(0x35, true, false)).action, None);
        assert_eq!(screen.handle(Input::Button { point: inside,
                                                 down:  false }),
                   Update::default());
        for code in [0x24, 0x4c, 0x31]
        {
            assert_eq!(screen.handle(key(code, true, false)).damage, Some(bounds));
            assert_eq!(screen.handle(key(code, true, true)), Update::default());
            assert_eq!(screen.handle(key(code, false, false)),
                       Update { damage: Some(bounds),
                                action: Some(Action::Restart) });
            assert_eq!(screen.handle(key(code, false, false)), Update::default());
        }
        screen.handle(Input::Button { point: inside,
                                      down:  true });
        assert_eq!(screen.handle(Input::Button { point: inside,
                                                 down:  false })
                         .action,
                   Some(Action::Restart));
    }

    #[test]
    fn retiring_a_pressed_device_cancels_only_button_pixels_and_unblocks_the_other_device()
    {
        for retire_keyboard in [true, false]
        {
            let mut screen = screen();
            let record = Record::format(Kind::SystemPanic, format_args!("trusted diagnostic"));
            let mut pixels = vec![0; 800 * 600];
            screen.draw(&record, &mut pixels).unwrap();
            let baseline = pixels.clone();
            let bounds = screen.restart_bounds();
            let inside = Point { h: bounds.left + 2,
                                 v: bounds.top + 2 };
            let pointer = |down| {
                Input::Button { point: inside,
                                down }
            };
            let observed = Observe::begin(true, None);
            let idle = screen.cancel_tracking();
            let pressed =
                screen.handle(if retire_keyboard { key(0x24, true, false) } else { pointer(true) });
            screen.draw_damage(&record, &mut pixels, bounds).unwrap();
            let cancelled = screen.cancel_tracking();
            screen.draw_damage(&record, &mut pixels, bounds).unwrap();
            let cancelled_again = screen.cancel_tracking();
            let late_release = screen.handle(if retire_keyboard
                                             {
                                                 key(0x24, false, false)
                                             }
                                             else
                                             {
                                                 pointer(false)
                                             });
            let survivor_pressed =
                screen.handle(if retire_keyboard { pointer(true) } else { key(0x24, true, false) });
            let survivor_released = screen.handle(if retire_keyboard
                                                  {
                                                      pointer(false)
                                                  }
                                                  else
                                                  {
                                                      key(0x24, false, false)
                                                  });
            let allocations = observed.finish();
            assert_eq!(allocations, 0);
            assert_eq!(idle, Update::default());
            assert_eq!(pressed,
                       Update { damage: Some(bounds),
                                action: None });
            assert_eq!(cancelled,
                       Update { damage: Some(bounds),
                                action: None });
            assert_eq!(cancelled_again, Update::default());
            assert_eq!(late_release, Update::default(), "a lost press must never fire later");
            assert_eq!(pixels, baseline,
                       "cancellation must restore the original button and all unrelated pixels");
            assert_eq!(survivor_pressed,
                       Update { damage: Some(bounds),
                                action: None });
            assert_eq!(survivor_released,
                       Update { damage: Some(bounds),
                                action: Some(Action::Restart) });
        }
    }

    #[test]
    fn cancelling_an_unpressed_pointer_claim_does_no_paint_and_allows_keyboard_restart()
    {
        let mut screen = screen();
        let bounds = screen.restart_bounds();
        let inside = Point { h: bounds.left + 2,
                             v: bounds.top + 2 };
        let outside = Point { h: bounds.left - 20,
                              v: bounds.top };
        let observed = Observe::begin(true, None);
        screen.handle(Input::Button { point: inside,
                                      down:  true });
        let exited = screen.handle(Input::Move(outside));
        let cancelled = screen.cancel_tracking();
        let pressed = screen.handle(key(0x24, true, false));
        let released = screen.handle(key(0x24, false, false));
        let allocations = observed.finish();
        assert_eq!(allocations, 0);
        assert_eq!(exited.damage, Some(bounds));
        assert_eq!(cancelled, Update::default(), "clearing an invisible claim changes no pixels");
        assert_eq!(pressed.damage, Some(bounds));
        assert_eq!(released.action, Some(Action::Restart));
    }

    #[test]
    fn renderer_exposes_unicode_codepoints_without_destroying_the_record()
    {
        let unicode = Record::format(Kind::SystemPanic, format_args!("é 🦀\r\nnext\tline"));
        let escaped = Record::format(Kind::SystemPanic, format_args!("U+00E9 U+1F980\nnext\tline"));
        let mut left = vec![0; 800 * 600];
        let mut right = vec![0; 800 * 600];
        screen().draw(&unicode, &mut left).unwrap();
        screen().draw(&escaped, &mut right).unwrap();
        assert_eq!(left, right);
        assert_eq!(unicode.text(), "é 🦀\r\nnext\tline");
    }
    #[test]
    fn concrete_terminal_format_render_and_input_need_no_allocation()
    {
        let mut pixels = vec![0; 800 * 600];
        let observed = Observe::begin(true, None);
        let mut screen = screen();
        let mut cursor = Cursor::new(800, 600, crate::toolbox::theme::palette()).unwrap();
        let record =
            Record::format(Kind::OutOfMemory,
                           format_args!("request={} align={}\nowner={}\n{}",
                                        65536, 4096, 4294967297u64, "é🦀\tallocation refused"));
        let full = screen.draw(&record, &mut pixels);
        let point = Point { h: 790,
                            v: 590 };
        let moved = cursor.move_to(&mut pixels, point);
        let unchanged = cursor.move_to(&mut pixels, point);
        let hidden = cursor.hide(&mut pixels);
        let update = screen.handle(key(0x24, true, false));
        let local = screen.draw_damage(&record, &mut pixels, update.damage.unwrap());
        let action = screen.handle(key(0x24, false, false)).action;
        let allocations = observed.finish();
        assert_eq!(allocations, 0);
        assert_eq!(full, Ok(()));
        assert_eq!(local, Ok(()));
        assert_eq!(action, Some(Action::Restart));
        assert_eq!((moved, unchanged, hidden), (Ok(true), Ok(false), Ok(true)));
        std::println!("EMERGENCY_SIZES record={} screen={} cursor={} bomb={} font=3626 scanout={}",
                      core::mem::size_of::<Record>(),
                      core::mem::size_of::<Screen>(),
                      core::mem::size_of::<Cursor>(),
                      BOMB.len(),
                      800 * 600 * core::mem::size_of::<u32>());
    }

    #[test]
    fn terminal_cursor_matches_real_rom_compositor_and_restores_every_edge()
    {
        use crate::toolbox::quickdraw::{cursors::Cursors, presentation::present_rect};
        let (width, height) = (360, 220);
        let scene: std::vec::Vec<u32> =
            (0..width * height).map(|index| index as u32 * 17).collect();
        let mut pixels = scene.clone();
        let mut expected = scene.clone();
        let mut cursor = Cursor::new(width, height, crate::toolbox::theme::palette()).unwrap();
        let mut ordinary = Cursors::default();
        for point in [Point { h: 40,
                              v: 50 },
                      Point { h: 0,
                              v: 0 },
                      Point { h: -8,
                              v: -7 },
                      Point { h: 359,
                              v: 219 },
                      Point { h: i16::MIN,
                              v: i16::MIN },
                      Point { h: i16::MAX,
                              v: i16::MAX }]
        {
            assert_eq!(cursor.move_to(&mut pixels, point), Ok(true));
            assert_eq!(cursor.position(), Some(point));
            expected.copy_from_slice(&scene);
            present_rect(&scene,
                         &mut expected,
                         width,
                         height,
                         Rect::new(0, 0, width as i16, height as i16),
                         ordinary.sprite(point, None)).unwrap();
            assert_eq!(pixels, expected, "actual ROM data/mask/hotspot differ at {point:?}");
            assert_eq!(cursor.hide(&mut pixels), Ok(true));
            assert_eq!(cursor.position(), None);
            assert_eq!(pixels, scene, "save-under failed at {point:?}");
            assert_eq!(cursor.hide(&mut pixels), Ok(false));
        }
    }

    #[test]
    fn terminal_cursor_no_change_refusal_and_button_repaint_preserve_underlying_pixels()
    {
        let mut screen = screen();
        let record = Record::format(Kind::SystemPanic, format_args!("terminal cursor"));
        let mut pixels = vec![0; 800 * 600];
        screen.draw(&record, &mut pixels).unwrap();
        let baseline = pixels.clone();
        let mut cursor = Cursor::new(800, 600, crate::toolbox::theme::palette()).unwrap();
        let button = screen.restart_bounds();
        let point = Point { h: button.left + 8,
                            v: button.top + 8 };
        cursor.move_to(&mut pixels, point).unwrap();
        // A write at the pointer distinguishes a no-op from recomposition or
        // restore/repaint to the same position.
        let at = point.v as usize * 800 + point.h as usize;
        pixels[at] = 0x123456;
        assert_eq!(cursor.move_to(&mut pixels, point), Ok(false));
        assert_eq!(pixels[at], 0x123456);
        let mut invalid = [0; 8];
        assert_eq!(cursor.hide(&mut invalid), Err(GeometryError::Pixels));
        assert_eq!(cursor.move_to(&mut invalid,
                                  Point { h: 0,
                                          v: 0 }),
                   Err(GeometryError::Pixels));
        assert_eq!(cursor.position(), Some(point));
        assert_eq!(invalid, [0; 8]);
        cursor.hide(&mut pixels).unwrap();
        assert_eq!(pixels, baseline);
        let damage = screen.handle(Input::Button { point,
                                                   down: true })
                           .damage
                           .unwrap();
        screen.draw_damage(&record, &mut pixels, damage).unwrap();
        let pressed = pixels.clone();
        cursor.move_to(&mut pixels, point).unwrap();
        cursor.hide(&mut pixels).unwrap();
        assert_eq!(pixels, pressed,
                   "cursor restored pre-button rather than freshly committed pixels");
        let damage = screen.handle(key(0x35, true, false)).damage.unwrap();
        screen.draw_damage(&record, &mut pixels, damage).unwrap();
        cursor.move_to(&mut pixels, point).unwrap();
        cursor.hide(&mut pixels).unwrap();
        assert_eq!(pixels, baseline);
    }

    #[test]
    fn terminal_cursor_uses_theme_roles_and_retains_monochrome_inversion()
    {
        let mut palette = crate::toolbox::theme::palette();
        palette.text = 0x102030;
        palette.content = 0xaabbcc;
        let mut cursor = Cursor::new(360, 220, palette).unwrap();
        // Exercise all four classic image/mask cases, including inversion which
        // need not occur in the particular bundled ROM arrow silhouette.
        cursor.arrow = MonoCursor::default();
        cursor.arrow.image[0] = 0xa000;
        cursor.arrow.mask[0] = 0xc000;
        let mut pixels = vec![0x246810; 360 * 220];
        cursor.move_to(&mut pixels,
                       Point { h: 0,
                               v: 0 })
              .unwrap();
        assert_eq!(pixels[0], palette.text);
        assert_eq!(pixels[1], palette.content);
        assert_eq!(pixels[2], 0x246810 ^ palette.text ^ palette.content);
        assert_eq!(pixels[3], 0x246810);
        cursor.hide(&mut pixels).unwrap();
        assert!(pixels.iter().all(|&pixel| pixel == 0x246810));
    }
}
