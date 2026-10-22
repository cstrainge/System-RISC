//! Interactive editor using only resident TextEdit and the public Toolbox SDK.
use core::fmt::Write;

use mac_toolbox::{textedit::{CursorMovement, EditorId},
                  *};


#[derive(Default)]
pub struct Editor<'a>
{
    pub window:     WindowId,
    editor:         Option<textedit::Editor<'a>>,
    size:           Point,
    tracking:       bool,
    last_mouse:     Point,
    ticks:          u32,
    defer_drawing:  bool,
    draw_pending:   bool,
    report_pending: bool
}
impl<'a> Editor<'a>
{
    fn id(&self) -> EditorId
    {
        self.editor.as_ref().map_or(EditorId(0), |editor| editor.id())
    }

    pub fn close(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if self.window != WindowId(0)
        {
            if let Some(editor) = &mut self.editor
            {
                editor.dispose()?;
            }
            system.close_window(self.window)?;
            *self = Self::default();
        }
        Ok(())
    }

    fn report(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if self.defer_drawing
        {
            self.report_pending = true;
            return Ok(());
        }
        let info = system.textedit().info(self.id())?;
        let mut line = crate::Text::<128>::default();
        let _ = writeln!(&mut line,
                         "FINDER_TEXTEDIT_STATE length={} selection={}..{}",
                         info.text_length, info.selection_start, info.selection_end);
        system.log(line.as_str())
    }

    /// Apply queued edits immediately, committing their final drawing together.
    pub fn defer_drawing(&mut self)
    {
        self.defer_drawing = true;
    }

    pub fn flush_drawing(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        self.defer_drawing = false;
        if core::mem::take(&mut self.draw_pending)
        {
            self.draw(system)?;
        }
        if core::mem::take(&mut self.report_pending)
        {
            self.report(system)?;
        }
        Ok(())
    }

    pub fn active(&self, system: &System<'_>) -> bool
    {
        self.window != WindowId(0)
        && system.processes().current().is_ok_and(|process| process.foreground)
        && system.window(WindowId(0)).is_ok_and(|w| w.id == self.window)
    }

    pub fn selected(&self, system: &System<'_>) -> bool
    {
        self.active(system)
        && system.textedit()
                 .info(self.id())
                 .is_ok_and(|info| info.selection_start != info.selection_end)
    }

    pub fn open(&mut self, system: &System<'a>) -> Result<(), Error>
    {
        self.prune(system)?;
        if self.window == WindowId(0)
        {
            let resource =
                system.resources()
                      .application()
                      .get(resources::ResourceKey { kind: mac_toolbox::TypeCode::new(*b"WIND"),
                                                    id:   2003 })?;
            let mut bytes = [0; 20];
            if resource.size() != bytes.len() || resource.read(0, &mut bytes)? != bytes.len()
            {
                return Err(Error::BadArgument);
            }
            resource.release()?;
            let word = |at| i16::from_be_bytes([bytes[at], bytes[at + 1]]);
            let bounds = Rect::new(word(2), word(0), word(6), word(4));
            if word(8) != 8 || bounds.is_empty()
            {
                return Err(Error::BadArgument);
            }
            let mut title = [0; 768];
            let title = crate::text(system, 0, &mut title)?;
            let mut sample = [0; 768];
            let sample = crate::text(system, 1, &mut sample)?;
            let window = system.new_window(bounds, title, (240, 120), 8)?;
            let view =
                Rect::new(8, 8, bounds.right - bounds.left - 8, bounds.bottom - bounds.top - 24);
            let editor = match system.textedit().create_owned(view, view)
            {
                Ok(id) => id,
                Err(e) =>
                {
                    system.close_window(window)?;
                    return Err(e);
                }
            };
            if let Err(e) = editor.set_text(sample)
            {
                system.close_window(window)?;
                return Err(e);
            }
            self.window = window;
            self.editor = Some(editor);
        }
        system.select_window(self.window)?;
        system.textedit().activate(self.id(), true)?;
        self.ticks = system.events().ticks()?;
        self.draw(system)?;
        system.log("FINDER_TEXTEDIT_READY\n")?;
        self.report(system)
    }

    fn prune(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if self.window == WindowId(0)
        {
            return Ok(());
        }
        match system.window(self.window)
        {
            Ok(_) =>
            {}
            Err(Error::NotFound) =>
            {
                if let Some(editor) = &mut self.editor
                {
                    editor.dispose()?;
                }
                *self = Self::default();
            }
            Err(status) => return Err(status)
        }
        Ok(())
    }

    pub fn draw(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if self.defer_drawing
        {
            self.draw_pending = true;
            return Ok(());
        }
        let window = system.window(self.window)?;
        let size = Point { h: window.bounds.right - window.bounds.left,
                           v: window.bounds.bottom - window.bounds.top };
        if size != self.size
        {
            let view = Rect::new(8, 8, size.h - 8, size.v - 24);
            system.textedit().set_rects(self.id(), view, view)?;
            system.windows().set_grow_icon_clip(self.window,
                                                 Some(Rect::new(size.h - 15,
                                                                size.v - 15,
                                                                size.h,
                                                                size.v)))?;
            self.size = size;
            let mut line = crate::Text::<96>::default();
            let _ =
                writeln!(&mut line, "FINDER_TEXTEDIT_LAYOUT width={} height={}", size.h, size.v);
            system.log(line.as_str())?;
        }
        // TE edits reset the blink state. Prime the caret before committing the
        // new scene, rather than presenting a hidden caret and repainting the
        // entire window again on the following null event.
        system.textedit().idle(self.id(), self.ticks)?;
        system.begin_update(self.window)?;
        let result = (|| {
            let port = system.quickdraw().graphics(self.window);
            port.reset()?;
            port.colors(0xffffff, 0xffffff)?;
            port.paint_rect(Rect::new(0, 0, size.h, size.v))?;
            system.textedit().draw(self.id(), self.window)?;
            port.colors(0, 0xffffff)?;
            let mut bytes = [0; 768];
            let footer = crate::text(system, 2, &mut bytes)?;
            port.clip_rect(Rect::new(0, size.v - 20, size.h - 16, size.v))?;
            port.text(quickdraw::fonts::FontSelection { family: 3,
                                                        size:   9,
                                                        face:   0 },
                      Point { h: 8,
                              v: size.v - 7 },
                      footer,
                      0)?;
            Ok(())
        })();
        system.end_update(self.window)?;
        result
    }

    pub fn command(&mut self, system: &System<'_>, command: u32, ticks: u32)
                   -> Result<bool, Error>
    {
        self.ticks = ticks;
        if !self.active(system)
        {
            return Ok(false);
        }
        let edit = system.textedit();
        match command
        {
            crate::menus::CUT =>
            {
                edit.cut(self.id())?;
            }
            crate::menus::COPY =>
            {
                edit.copy(self.id())?;
            }
            crate::menus::PASTE =>
            {
                edit.paste(self.id())?;
            }
            crate::menus::CLEAR =>
            {
                edit.delete(self.id())?;
            }
            crate::menus::SELECT_ALL =>
            {
                edit.select(self.id(), 0, u32::MAX)?;
            }
            _ => return Ok(false)
        }
        self.draw(system)?;
        system.log("FINDER_TEXTEDIT_EDIT\n")?;
        self.report(system)?;
        Ok(true)
    }

    pub fn event(&mut self, system: &System<'_>, event: &Event, position: PointerPosition)
                 -> Result<bool, Error>
    {
        self.ticks = event.input().when;
        self.prune(system)?;
        if self.window == WindowId(0)
        {
            return Ok(false);
        }
        let edit = system.textedit();
        if matches!(event, Event::Activate { .. })
        {
            edit.activate(self.id(), self.active(system))?;
            self.draw(system)?;
        }
        if matches!(event, Event::Update { .. })
           && event.window().unwrap_or_default() == self.window
        {
            self.draw(system)?;
            return Ok(true);
        }
        if !self.active(system)
        {
            self.tracking = false;
            return Ok(false);
        }
        match event
        {
            Event::MouseDown { .. } if event.window().unwrap_or_default() == self.window =>
            {
                self.last_mouse = event.input().at;
                edit.click(self.id(),
                           position.in_window(self.window)?.point()?,
                           event.input()
                                .modifiers
                                .contains(mac_toolbox::events::Modifiers::SHIFT))?;
                self.tracking = true;
                self.draw(system)?;
                Ok(true)
            }
            Event::MouseUp { .. } if self.tracking =>
            {
                self.tracking = false;
                Ok(true)
            }
            // A key release does not change browser policy or the filesystem.
            Event::KeyUp { .. } => Ok(true),
            Event::KeyDown { .. } =>
            {
                let command =
                    event.input().modifiers.contains(mac_toolbox::events::Modifiers::COMMAND);
                let movement = match event.key().map_or(0, |key| key.code())
                {
                    0x7b => Some(CursorMovement::Left),
                    0x7c => Some(CursorMovement::Right),
                    0x7e => Some(CursorMovement::Up),
                    0x7d => Some(CursorMovement::Down),
                    0x73 =>
                    {
                        Some(if command
                             {
                                 CursorMovement::DocumentStart
                             }
                             else
                             {
                                 CursorMovement::LineStart
                             })
                    }
                    0x77 =>
                    {
                        Some(if command
                             {
                                 CursorMovement::DocumentEnd
                             }
                             else
                             {
                                 CursorMovement::LineEnd
                             })
                    }
                    _ => None
                };
                if let Some(movement) = movement
                {
                    edit.move_cursor(self.id(),
                                     movement,
                                     event.input()
                                          .modifiers
                                          .contains(mac_toolbox::events::Modifiers::SHIFT))?;
                    self.draw(system)?;
                    self.report(system)?;
                    return Ok(true);
                }
                if command
                {
                    return Ok(true);
                }
                let key = event.key().map_or('\0', |key| key.character());
                if key == '\r'
                   || key == '\u{8}'
                   || ('\u{1c}'..='\u{1f}').contains(&key)
                   || !key.is_control()
                {
                    edit.key(self.id(), key)?;
                    self.draw(system)?;
                    self.report(system)?;
                }
                Ok(true)
            }
            Event::Idle { .. } =>
            {
                let before = edit.info(self.id())?;
                if self.tracking && event.input().at != self.last_mouse
                {
                    edit.click(self.id(), position.in_window(self.window)?.point()?, true)?;
                    self.last_mouse = event.input().at;
                }
                let after = edit.idle(self.id(), event.input().when)?;
                if before != after
                {
                    self.draw(system)?;
                }
                Ok(self.tracking)
            }
            _ => Ok(false)
        }
    }
}
