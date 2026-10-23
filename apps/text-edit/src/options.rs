//! Resource-defined document options. CDEF controls own tracking and their pixels;
//! the numeric limit is another owned resident TextEdit, never an app-local field.
use core::fmt::Write;

use mac_toolbox::{controls::{Control, StandardKind, StandardState},
                  *};

use crate::{Text, preferences::Settings};
const ITEMS: usize = 28;

pub struct Options<'a>
{
    controls:   [Option<Control<'a>>; ITEMS],
    states:     [StandardState; ITEMS],
    limit:      textedit::Editor<'a>,
    window:     windows::Window<'a>,
    font:       quickdraw::fonts::FontSelection,
    display:    textedit::DisplayOptions,
    mode:       usize,
    tracking:   bool,
    last_mouse: Point
}
pub enum Outcome
{
    Pending,
    Cancel,
    Apply(Settings)
}
fn resource(system: &System<'_>, kind: [u8; 4], output: &mut [u8]) -> Result<usize, Error>
{
    let resource =
        system.resources().application().get(resources::ResourceKey { kind: TypeCode::new(kind),
                                                                       id:   2004 })?;
    let length = resource.size();
    if length > output.len() || resource.read(0, &mut output[..length])? != length
    {
        return Err(Error::CorruptData);
    }
    resource.release()?;
    Ok(length)
}
impl<'a> Options<'a>
{
    pub fn open(system: &System<'a>, font: quickdraw::fonts::FontSelection,
                display: textedit::DisplayOptions)
                -> Result<Self, Error>
    {
        let mut bytes = [0; 2048];
        let length = resource(system, *b"WIND", &mut bytes)?;
        if length != 20
        {
            return Err(Error::CorruptData);
        }
        let word = |at| i16::from_be_bytes([bytes[at], bytes[at + 1]]);
        let bounds = Rect::new(word(2), word(0), word(6), word(4));
        let window = system.windows().create_owned(windows::WindowSpec::new(bounds, crate::text(system, 9, &mut [0;128])?).procedure(word(8)))?;
        system.windows().set_grow_icon_clip(window.id(), Some(Rect::ZERO))?;
        let length = resource(system, *b"DITL", &mut bytes)?;
        if length < 2 || u16::from_be_bytes([bytes[0], bytes[1]]) as usize + 1 != ITEMS
        {
            return Err(Error::CorruptData);
        }
        let mut controls = core::array::from_fn(|_| None);
        let mut states = [StandardState::default(); ITEMS];
        let mut at = 2;
        for (index, state) in states.iter_mut().enumerate()
        {
            let header = bytes.get(at..at + 14).ok_or(Error::CorruptData)?;
            let word = |n| i16::from_be_bytes([header[n], header[n + 1]]);
            let bounds = Rect::new(word(6), word(4), word(10), word(8));
            let kind = header[12];
            let count = header[13] as usize;
            let label = core::str::from_utf8(bytes.get(at + 14..at + 14 + count)
                                                  .ok_or(Error::CorruptData)?).map_err(|_| {
                                                                                  Error::CorruptData
                                                                              })?;
            *state = StandardState::new(bounds, label);
            if kind == 8
            {
                state.maximum = 0;
            }
            let procedure = match kind
            {
                4 => Some(StandardKind::Button),
                5 => Some(StandardKind::Checkbox),
                6 => Some(StandardKind::Radio),
                8 => Some(StandardKind::StaticText),
                16 if index == 20 => None,
                _ => return Err(Error::CorruptData)
            };
            if let Some(procedure) = procedure
            {
                controls[index] =
                    Some(system.controls().create_standard(window.id(), procedure, *state)?);
            }
            at += 14 + count + (count & 1);
        }
        if at != length
        {
            return Err(Error::CorruptData);
        }
        let rect = states[20].bounds;
        let limit = system.textedit().create_owned(rect, rect)?;
        limit.set_font(quickdraw::fonts::FontSelection { family: 4,
                                                         size:   12,
                                                         face:   0 })?;
        let (mode, value) = match display.ruler
        {
            textedit::Ruler::Off => (0, 80),
            textedit::Ruler::Column(n) => (1, i32::from(n)),
            textedit::Ruler::X(x) => (2, i32::from(x))
        };
        let mut value_text = Text::<16>::default();
        write!(value_text, "{value}").map_err(|_| Error::NoMemory)?;
        limit.set_text(value_text.as_str())?;
        limit.select(0, u32::MAX)?;
        limit.activate(true)?;
        let mut dialog = Self { controls,
                                states,
                                limit,
                                window,
                                font,
                                display,
                                mode,
                                tracking: false,
                                last_mouse: Point::default() };
        dialog.refresh()?;
        dialog.draw(system)?;
        system.select_window(dialog.window.id())?;
        system.log("TEXT_EDIT_OPTIONS_READY\n")?;
        Ok(dialog)
    }

    fn kind(index: usize) -> StandardKind
    {
        match index
        {
            0..=14 | 17..=19 => StandardKind::Radio,
            15 | 16 => StandardKind::Checkbox,
            26 | 27 => StandardKind::Button,
            _ => StandardKind::StaticText
        }
    }

    fn refresh(&mut self) -> Result<(), Error>
    {
        for index in 0..20
        {
            let value = match index
            {
                0..=8 => self.font.family == crate::preferences::FAMILIES[index],
                9..=14 => self.font.size == crate::preferences::SIZES[index - 9],
                15 => self.display.wrap,
                16 => self.display.line_numbers,
                17..=19 => self.mode == index - 17,
                _ => false
            };
            let next = i16::from(value);
            if self.states[index].value != next
            {
                self.states[index].value = next;
                self.controls[index].as_ref()
                                    .ok_or(Error::BadAbi)?
                                    .control()
                                    .configure(Self::kind(index), self.states[index])?;
            }
        }
        Ok(())
    }

    fn draw_limit(&self, system: &System<'a>) -> Result<(), Error>
    {
        let port = system.quickdraw().graphics(self.window.id());
        let bounds = self.states[20].bounds;
        let frame = Rect::new(bounds.left - 2, bounds.top - 2, bounds.right + 2, bounds.bottom + 2);
        let update = port.masked_rectangles(&[frame])?;
        port.reset()?;
        port.colors(0xffffff, 0xffffff)?;
        port.paint_rect(frame)?;
        port.colors(0, 0xffffff)?;
        port.frame_rect(frame)?;
        self.limit.idle(system.events().ticks()?)?;
        self.limit.draw(self.window.id())?;
        update.commit()
    }

    fn draw(&mut self, system: &System<'a>) -> Result<(), Error>
    {
        let update = system.windows().update(self.window.id())?;
        let port = system.quickdraw().graphics(self.window.id());
        port.reset()?;
        port.colors(0xffffff, 0xffffff)?;
        port.paint_rect(update.bounds())?;
        port.colors(0, 0xffffff)?;
        for control in self.controls.iter().flatten()
        {
            control.control().draw()?;
        }
        self.limit.idle(system.events().ticks()?)?;
        self.limit.draw(self.window.id())?;
        let bounds = self.states[20].bounds;
        port.frame_rect(Rect::new(bounds.left - 2,
                                  bounds.top - 2,
                                  bounds.right + 2,
                                  bounds.bottom + 2))?;
        update.finish()
    }

    fn settings(&self) -> Result<Settings, Error>
    {
        let ruler = if self.mode == 0
        {
            textedit::Ruler::Off
        }
        else
        {
            let mut bytes = [0; 32];
            let value =
                self.limit.text(&mut bytes)?.parse::<u32>().map_err(|_| Error::BadArgument)?;
            match self.mode
            {
                1 if value > 0 && value <= u32::from(u16::MAX) =>
                {
                    textedit::Ruler::Column(value as u16)
                }
                2 if value <= i16::MAX as u32 => textedit::Ruler::X(value as i16),
                _ => return Err(Error::BadArgument)
            }
        };
        Ok(Settings::from_font(self.font).with_options(textedit::DisplayOptions { ruler,
                                                                                  ..self.display }))
    }

    pub fn event(&mut self, system: &System<'a>, event: &Event, position: PointerPosition)
                 -> Result<Outcome, Error>
    {
        match event
        {
            Event::Activate { .. } =>
            {
                let active = system.processes().current()?.foreground
                             && system.windows().front()? == Some(self.window.id());
                self.limit.activate(active)?;
                if !active
                {
                    self.tracking = false;
                }
                self.draw_limit(system)?;
            }
            Event::CloseRequested { .. } => return Ok(Outcome::Cancel),
            Event::Update { .. } | Event::WindowGeometryChanged { .. }
                if event.window() == Some(self.window.id()) =>
            {
                self.draw(system)?
            }
            Event::MouseDown { .. } if event.window() == Some(self.window.id()) =>
            {
                let point = position.in_window(self.window.id())?;
                for index in 0..ITEMS
                {
                    if matches!(index, 20..=25)
                    {
                        continue;
                    }
                    let control = self.controls[index].as_ref().ok_or(Error::BadAbi)?.control();
                    let mut action = false;
                    if control.track_at(point, |state| {
                                  action |= state.action;
                                  Ok(())
                              })?
                    {
                        if !action
                        {
                            return Ok(Outcome::Pending);
                        }
                        match index
                        {
                            0..=8 => self.font.family = crate::preferences::FAMILIES[index],
                            9..=14 => self.font.size = crate::preferences::SIZES[index - 9],
                            15 => self.display.wrap ^= true,
                            16 => self.display.line_numbers ^= true,
                            17..=19 => self.mode = index - 17,
                            26 => return Ok(Outcome::Cancel),
                            27 => return self.accept(system),
                            _ => ()
                        }
                        self.refresh()?;
                        return Ok(Outcome::Pending);
                    }
                }
                if self.states[20].bounds.contains(point.point()?)
                {
                    self.limit.click(point.point()?,
                                      event.input().modifiers.contains(events::Modifiers::SHIFT))?;
                    self.tracking = true;
                    self.last_mouse = event.input().at;
                    self.draw_limit(system)?;
                }
            }
            Event::MouseUp { .. } => self.tracking = false,
            Event::KeyDown { .. } =>
            {
                let key = event.key().map_or('\0', |key| key.character());
                if key == '\u{1b}'
                {
                    return Ok(Outcome::Cancel);
                }
                if key == '\r'
                {
                    return self.accept(system);
                }
                if key.is_ascii_digit() || key == '\u{8}'
                {
                    self.limit.key(key)?;
                    self.draw_limit(system)?;
                }
                else if event.input().modifiers.contains(events::Modifiers::COMMAND) && key == 'a'
                {
                    self.limit.select(0, u32::MAX)?;
                    self.draw_limit(system)?;
                }
            }
            Event::Idle { .. } =>
            {
                if self.tracking && self.last_mouse != event.input().at
                {
                    self.limit.click(position.in_window(self.window.id())?.point()?, true)?;
                    self.last_mouse = event.input().at;
                    self.draw_limit(system)?;
                }
                if self.limit.blink(self.window.id(), event.input().when)?
                {
                    self.draw_limit(system)?;
                }
            }
            _ => ()
        }
        Ok(Outcome::Pending)
    }

    fn accept(&self, system: &System<'a>) -> Result<Outcome, Error>
    {
        match self.settings()
        {
            Ok(settings) => Ok(Outcome::Apply(settings)),
            Err(Error::BadArgument) =>
            {
                system.log("TEXT_EDIT_OPTIONS_INVALID_LIMIT\n")?;
                // Keep the original dialog and draft intact while the System
                // presents an informative validation alert.
                let mut alert = system.dialogs()
                                      .alert_owned(resources::APPLICATION,
                                                   2001,
                                                   crate::text(system, 10, &mut [0; 256])?)?;
                loop
                {
                    let event = system.wait_event(events::EventMask::ALL, 30)?;
                    if alert.handle_event(&event)?.is_some()
                    {
                        alert.close()?;
                        break;
                    }
                }
                Ok(Outcome::Pending)
            }
            Err(error) => Err(error)
        }
    }
}
