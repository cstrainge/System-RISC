//! The report window is serviced by System even while its application has
//! stopped executing. Input outside its content still reaches the desktop.
use alloc::{format, string::String};

use mac_abi::{crashes::*, quickdraw::*, *};

use crate::toolbox::{crashes::RESOURCE_ID,
                     events::input::Input,
                     quickdraw::{fonts_service, icons_service, service},
                     windows::WindowPart};

const REPORT: Rect = Rect::new(14, 91, 566, 347);
const BUTTONS: [Rect; 6] = [Rect::new(14, 369, 55, 391),
                            Rect::new(61, 369, 102, 391),
                            Rect::new(108, 369, 160, 391),
                            Rect::new(166, 369, 225, 391),
                            Rect::new(302, 369, 421, 391),
                            Rect::new(435, 369, 566, 391)];
const LABELS: [u32; 6] = [3, 4, 5, 6, 2, 1];
const MONACO: FontSelection = FontSelection { family: 4,
                                              size:   9,
                                              face:   0 };

fn checked(status: Status) -> Result<(), Status>
{
    if status == OK { Ok(()) } else { Err(status) }
}

fn label(index: u32) -> Result<String, Status>
{
    let mut bytes = [0; 256];
    let mut call = resources::ResourceCall { operation: resources::STRING,
                                             scope: resources::SYSTEM,
                                             key: ResourceKey { kind: *b"STR#",
                                                                id:   RESOURCE_ID },
                                             index,
                                             ..Default::default() };
    crate::kernel::resources().ok_or(NOT_SUPPORTED)?.request(&mut call, "", &mut bytes)?;
    let mut text = String::new();
    let value = core::str::from_utf8(&bytes[..call.transferred]).map_err(|_| BAD_ARGUMENT)?;
    text.try_reserve(value.len()).map_err(|_| NO_MEMORY)?;
    text.push_str(value);
    Ok(text)
}

fn window_request(id: WindowId) -> WindowRequest
{
    WindowRequest { id:             id.0,
                    bounds:         Rect::ZERO,
                    title:          [0; WINDOW_TITLE_BYTES],
                    title_length:   0,
                    procedure:      1,
                    minimum_width:  580,
                    minimum_height: 404,
                    reserved:       0 }
}

impl super::Desktop
{
    pub fn crash_request(&mut self, call: &mut CrashCall, input: &str, output: &mut [u8])
                         -> Result<(), Status>
    {
        let previous = self.crash.info();
        let thread = if call.operation == BEGIN || previous.app == 0
        {
            let identity = crate::context::current_thread_context().identity();
            if identity.app.0 == self.owner
            {
                identity.thread.0
            }
            else
            {
                self.processes.get(self.owner)?.info.thread
            }
        }
        else
        {
            previous.thread
        };
        self.crash.set_context(self.owner, thread);
        if call.operation == BEGIN
        {
            let old = self.crash.info().window;
            if old.0 != 0 && self.windows.window(old).is_some()
            {
                checked(self.window(CLOSE_WINDOW, &mut window_request(old)))?;
            }
        }
        let changed = self.crash.request(call, input, output)?;
        if call.operation == SHOW
        {
            self.show_crash_report()?;
        }
        else if call.operation == DISMISS
        {
            let window = self.crash.info().window;
            if window.0 != 0 && self.windows.window(window).is_some()
            {
                checked(self.window(CLOSE_WINDOW, &mut window_request(window)))?;
            }
            self.crash.set_window(WindowId(0));
            serial!("CRASH_REPORT_DISMISSED app={}", self.owner);
        }
        else if changed && self.crash.info().window.0 != 0
        {
            self.draw_crash_report()?;
        }
        call.info = self.crash.info();
        Ok(())
    }

    fn show_crash_report(&mut self) -> Result<(), Status>
    {
        if self.dialogs.active()
        {
            let old = WindowId(self.dialogs.window);
            let _ = self.window(CLOSE_WINDOW, &mut window_request(old));
            self.dialogs = Default::default();
        }
        self.menus = crate::toolbox::menus::Manager::new(self.mdef, crate::machine::traps::gateway);
        self.cursors = Default::default();
        // An application can panic between BeginUpdate and EndUpdate. Its
        // uncommitted pixels must never leak into a later presentation.
        while let Some(index) =
            self.pending_content
                .iter()
                .position(|content| content.id.window().is_ok_and(|id| self.owns(id)))
        {
            let id = self.pending_content.remove(index).id.window()?;
            self.windows.end_update(id);
            self.textedit.begin_cursor_update(id);
        }
        let fonts = crate::kernel::fonts().ok_or(NOT_SUPPORTED)?;
        let metrics = fonts.metrics(fonts_service::selection(MONACO))?;
        let advance =
            metrics.ascent.saturating_add(metrics.descent).saturating_add(metrics.leading).max(1);
        self.crash
            .set_columns(((REPORT.right - REPORT.left - 8) / metrics.max_width.max(1)).max(1)
                         as usize)?;
        self.crash.set_page_lines(((REPORT.bottom - REPORT.top - 8) / advance).max(1) as u32);
        let old = self.crash.info().window;
        if old.0 != 0 && self.windows.window(old).is_some()
        {
            checked(self.window(SELECT_WINDOW, &mut window_request(old)))?;
            return self.draw_crash_report();
        }
        let resources = crate::kernel::resources().ok_or(NOT_SUPPORTED)?;
        let (_, resource) = resources.get(resources::SYSTEM,
                                          ResourceKey { kind: *b"WIND",
                                                        id:   RESOURCE_ID })?;
        let bytes = &resource.data;
        let word = |at: usize| -> Result<i16, Status> {
            Ok(i16::from_be_bytes(bytes.get(at..at + 2)
                                       .ok_or(BAD_ARGUMENT)?
                                       .try_into()
                                       .map_err(|_| BAD_ARGUMENT)?))
        };
        let mut request = window_request(WindowId(0));
        request.bounds = Rect::new(word(2)?, word(0)?, word(6)?, word(4)?);
        request.procedure = word(8)?;
        let title = label(0)?;
        request.title[..title.len()].copy_from_slice(title.as_bytes());
        request.title_length = title.len() as u32;
        checked(self.window(NEW_WINDOW, &mut request))?;
        self.crash.set_window(WindowId(request.id));
        request.bounds = Rect::ZERO;
        request.reserved = 0;
        checked(self.window(SET_GROW_ICON_CLIP, &mut request))?;
        checked(self.window(SELECT_WINDOW, &mut request))?;
        self.draw_crash_report()?;
        let info = self.crash.info();
        serial!("CRASH_REPORT_READY app={} window={} bytes={} lines={} page_lines={} flags={}",
                self.owner, info.window.0, info.report_bytes, info.line_count, info.page_lines,
                info.flags);
        Ok(())
    }

    fn crash_text(&mut self, window: u64, font: FontSelection, x: i16, baseline: i16, text: &str)
                  -> Result<(), Status>
    {
        self.font_draw(&mut FontCall { target:
                                           mac_abi::quickdraw::Target::window(WindowId(window)),
                                       selection: font,
                                       baseline: Point { h: x,
                                                         v: baseline },
                                       ..Default::default() },
                       text,
                       crate::kernel::fonts().ok_or(NOT_SUPPORTED)?)
    }

    fn crash_button_enabled(&self, index: usize) -> bool
    {
        let info = self.crash.info();
        match index
        {
            0 | 1 => info.first_line != 0,
            2 | 3 => info.first_line.saturating_add(info.page_lines) < info.line_count,
            4 => info.report_bytes.saturating_add(8) <= mac_abi::scrap::MAX_BYTES,
            5 => self.owner != crate::context::finder_app().0,
            _ => false
        }
    }

    fn draw_crash_report(&mut self) -> Result<(), Status>
    {
        let info = self.crash.info();
        if info.window.0 == 0
        {
            return Err(NOT_FOUND);
        }
        let window = info.window.0;
        let mut request = window_request(info.window);
        checked(self.window(BEGIN_UPDATE, &mut request))?;
        let result = (|| {
            self.graphics(&mut GraphicsCall { operation: QD_RESET,
                                              target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                              ..Default::default() },
                          &[],
                          &[])?;
            self.graphics(&mut GraphicsCall { operation: QD_SHAPE,
                                              target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                              bounds: Rect::new(0, 0, 580, 404),
                                              shape: SHAPE_RECT,
                                              verb: VERB_ERASE,
                                              ..Default::default() },
                          &[],
                          &[])?;
            let call = IconCall { target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                  scope: resources::SYSTEM,
                                  id: RESOURCE_ID,
                                  destination: Rect::new(18, 14, 50, 46),
                                  ..Default::default() };
            let drawing =
                icons_service::Drawing::new(crate::kernel::resources().ok_or(NOT_SUPPORTED)?,
                                            &call,
                                            service::Port::default())?;
            self.icon(&call, drawing)?;
            self.crash_text(window, FontSelection::default(), 64, 29, &label(0)?)?;
            self.crash_text(window,
                            MONACO,
                            64,
                            48,
                            &format!("{} {}   {} {}",
                                     label(12)?,
                                     info.app,
                                     label(13)?,
                                     info.thread))?;
            let description =
                if self.owner == crate::context::finder_app().0 { label(8)? } else { label(11)? };
            self.crash_text(window, MONACO, 14, 72, &description)?;
            let mut warnings = String::new();
            warnings.try_reserve(256).map_err(|_| NO_MEMORY)?;
            for (enabled, index) in [(info.flags & TRUNCATED != 0, 9),
                                     (info.flags & FORMAT_FAILED != 0, 10),
                                     (self.crash.copy_failed, 17)]
            {
                if enabled
                {
                    if !warnings.is_empty()
                    {
                        warnings.push_str("   ");
                    }
                    warnings.push_str(&label(index)?);
                }
            }
            self.crash_text(window, MONACO, 14, 84, &warnings)?;
            self.graphics(&mut GraphicsCall { operation: QD_SHAPE,
                                              target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                              bounds: REPORT,
                                              shape: SHAPE_RECT,
                                              verb: VERB_FRAME,
                                              ..Default::default() },
                          &[],
                          &[])?;
            self.graphics(&mut GraphicsCall { operation: QD_CLIP_RECT,
                                              target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                              bounds: Rect::new(REPORT.left + 3,
                                                                REPORT.top + 3,
                                                                REPORT.right - 3,
                                                                REPORT.bottom - 3),
                                              ..Default::default() },
                          &[],
                          &[])?;
            let fonts = crate::kernel::fonts().ok_or(NOT_SUPPORTED)?;
            let metrics = fonts.metrics(fonts_service::selection(MONACO))?;
            let advance = metrics.ascent
                                 .saturating_add(metrics.descent)
                                 .saturating_add(metrics.leading)
                                 .max(1);
            for visible in 0..info.page_lines
            {
                let Some(line) = self.crash.line((info.first_line + visible) as usize)
                else
                {
                    break;
                };
                let mut text = String::new();
                text.try_reserve(line.len().saturating_mul(4)).map_err(|_| NO_MEMORY)?;
                let mut column = 0;
                for character in line.chars()
                {
                    if character == '\t'
                    {
                        let spaces = 4 - (column % 4);
                        for _ in 0..spaces
                        {
                            text.push(' ');
                        }
                        column += spaces;
                    }
                    else
                    {
                        text.push(character);
                        column += 1;
                    }
                }
                self.crash_text(window,
                                MONACO,
                                REPORT.left + 4,
                                REPORT.top + 4 + metrics.ascent + visible as i16 * advance,
                                &text)?;
            }
            self.graphics(&mut GraphicsCall { operation: QD_CLIP_RECT,
                                              target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                              bounds: Rect::new(0, 0, 580, 404),
                                              ..Default::default() },
                          &[],
                          &[])?;
            let coverage =
                format!("{} {}-{} {} {}   {} {}",
                        label(14)?,
                        if info.line_count == 0 { 0 } else { info.first_line + 1 },
                        info.first_line.saturating_add(info.page_lines).min(info.line_count),
                        label(15)?,
                        info.line_count,
                        info.report_bytes,
                        label(16)?);
            self.crash_text(window, MONACO, 14, 360, &coverage)?;
            for (index, bounds) in BUTTONS.iter().enumerate()
            {
                let title = if index == 5 && self.owner == crate::context::finder_app().0
                {
                    label(7)?
                }
                else
                {
                    label(LABELS[index])?
                };
                let mut standard = definitions::ControlDefinitionState::new(*bounds, &title);
                standard.enabled = u32::from(self.crash_button_enabled(index));
                standard.highlighted =
                    u32::from(self.crash.pressed == Some(index) && self.crash.pressed_hot);
                checked(self.control(&mut mac_abi::controls::ControlCall {
                    operation: mac_abi::controls::DRAW_STANDARD, window, standard,
                    ..Default::default()
                }))?;
            }
            Ok(())
        })();
        checked(self.window(END_UPDATE, &mut request))?;
        result
    }

    fn crash_button(&mut self, index: usize) -> Result<(), Status>
    {
        if !self.crash_button_enabled(index)
        {
            return Ok(());
        }
        let delta = match index
        {
            0 => i32::MIN,
            1 => -(self.crash.info().page_lines as i32),
            2 => self.crash.info().page_lines as i32,
            3 => i32::MAX,
            4 =>
            {
                let copied = crate::kernel::with_scrap(|scrap| {
                                 scrap.replace_text(self.crash.report())
                             }).map_err(|error| error.status())
                               .and_then(|result| result);
                self.crash.copy_failed = copied.is_err();
                self.draw_crash_report()?;
                if let Err(status) = copied
                {
                    serial!("CRASH_REPORT_COPY_FAILED app={} status={}", self.owner, status);
                    return Err(status);
                }
                serial!("CRASH_REPORT_COPIED app={} bytes={}",
                        self.owner,
                        self.crash.info().report_bytes);
                return Ok(());
            }
            5 =>
            {
                return self.crash_request(&mut CrashCall { operation: DISMISS,
                                                           ..Default::default() },
                                          "",
                                          &mut []);
            }
            _ => return Err(BAD_ARGUMENT)
        };
        self.crash.scroll(delta);
        self.draw_crash_report()?;
        self.log_crash_scroll();
        Ok(())
    }

    fn log_crash_scroll(&self)
    {
        let info = self.crash.info();
        serial!("CRASH_REPORT_SCROLL app={} first={} lines={} page_lines={}", self.owner,
                info.first_line, info.line_count, info.page_lines);
    }

    pub(super) fn crash_input(&mut self, input: Input, _when: u32) -> bool
    {
        let info = self.crash.info();
        if self.owner != self.foreground
           || info.window.0 == 0
           || info.dismissed != 0
           || self.windows.window(info.window).is_none()
           || self.app_menu.is_tracking()
           || self.menus.is_tracking()
           || self.windows.is_tracking()
        {
            return false;
        }
        match input
        {
            Input::Key { mac_code,
                         character,
                         down,
                         modifiers,
                         .. } if self.windows.active_window() == Some(info.window) =>
            {
                if !down
                {
                    return true;
                }
                let delta = match mac_code
                {
                    0x7e => -1,
                    0x7d => 1,
                    0x74 => -(info.page_lines as i32),
                    0x79 => info.page_lines as i32,
                    0x73 => i32::MIN,
                    0x77 => i32::MAX,
                    _ => 0
                };
                if delta != 0 && self.crash.scroll(delta) && self.draw_crash_report().is_ok()
                {
                    self.log_crash_scroll();
                }
                if modifiers & CMD_KEY != 0 && character.eq_ignore_ascii_case(&b'c')
                {
                    let _ = self.crash_button(4);
                }
                if modifiers & CMD_KEY != 0 && character.eq_ignore_ascii_case(&b'w')
                {
                    let _ = self.crash_button(5);
                }
                true
            }
            Input::Button { point,
                            down } =>
            {
                let hit = self.windows.find_window(point);
                if hit.window != Some(info.window) || hit.part != WindowPart::Content
                {
                    let was_pressed = self.crash.pressed.take().is_some();
                    self.crash.pressed_hot = false;
                    if was_pressed
                    {
                        let _ = self.draw_crash_report();
                    }
                    return was_pressed && !down;
                }
                let Some(window) = self.windows.window(info.window)
                else
                {
                    return false;
                };
                let local = Point { h: point.h - window.bounds.left,
                                    v: point.v - window.bounds.top };
                let button = BUTTONS.iter().position(|bounds| bounds.contains(local));
                if down
                {
                    let _ = self.window(SELECT_WINDOW, &mut window_request(info.window));
                    self.crash.pressed = button.filter(|index| self.crash_button_enabled(*index));
                    self.crash.pressed_hot = self.crash.pressed.is_some();
                    let _ = self.draw_crash_report();
                }
                else
                {
                    let pressed = self.crash.pressed.take();
                    self.crash.pressed_hot = false;
                    if let Some(index) = pressed.filter(|index| Some(*index) == button)
                    {
                        let _ = self.crash_button(index);
                    }
                    else
                    {
                        let _ = self.draw_crash_report();
                    }
                }
                true
            }
            Input::Move(point) if self.crash.pressed.is_some() =>
            {
                let Some(window) = self.windows.window(info.window)
                else
                {
                    return false;
                };
                let local = Point { h: point.h - window.bounds.left,
                                    v: point.v - window.bounds.top };
                let hot = self.crash.pressed.is_some_and(|index| BUTTONS[index].contains(local));
                if hot != self.crash.pressed_hot
                {
                    self.crash.pressed_hot = hot;
                    let _ = self.draw_crash_report();
                }
                true
            }
            _ => false
        }
    }
}
