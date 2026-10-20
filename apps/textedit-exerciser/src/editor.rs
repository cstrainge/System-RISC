//! Interactive editor using only resident TextEdit and the public Toolbox SDK.
use core::fmt::Write;

use mac_toolbox::{textedit::{CursorMovement, EditorId},
                  *};


#[derive(Default)]
pub struct Editor
{
    pub window:     u32,
    editor:         EditorId,
    size:           Point,
    tracking:       bool,
    last_mouse:     Point,
    ticks:          u32,
    defer_drawing:  bool,
    draw_pending:   bool,
    report_pending: bool
}
impl Editor
{
    pub fn close(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if self.window != 0
        {
            system.textedit().dispose(self.editor)?;
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
        let info = system.textedit().info(self.editor)?;
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
        self.window != 0
        && system.processes().current().is_ok_and(|process| process.foreground != 0)
        && system.window(0).is_ok_and(|w| w.id == self.window)
    }

    pub fn selected(&self, system: &System<'_>) -> bool
    {
        self.active(system)
        && system.textedit()
                 .info(self.editor)
                 .is_ok_and(|info| info.selection_start != info.selection_end)
    }

    pub fn open(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        self.prune(system)?;
        if self.window == 0
        {
            let resource =
                system.resources().application().get(resources::ResourceKey { kind: *b"WIND",
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
            let editor = match system.textedit().create(view, view)
            {
                Ok(id) => id,
                Err(e) =>
                {
                    system.close_window(window)?;
                    return Err(e);
                }
            };
            if let Err(e) = system.textedit().set_text(editor, sample)
            {
                let _ = system.textedit().dispose(editor);
                system.close_window(window)?;
                return Err(e);
            }
            self.window = window;
            self.editor = editor;
        }
        system.select_window(self.window)?;
        system.textedit().activate(self.editor, true)?;
        self.ticks = system.events().ticks()?;
        self.draw(system)?;
        system.log("FINDER_TEXTEDIT_READY\n")?;
        self.report(system)
    }

    fn prune(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if self.window == 0
        {
            return Ok(());
        }
        match system.window(self.window)
        {
            Ok(_) =>
            {}
            Err(Error::NotFound) =>
            {
                system.textedit().dispose(self.editor)?;
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
            system.textedit().set_rects(self.editor, view, view)?;
            system.windows().set_grow_icon_clip(WindowId(self.window),
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
        system.textedit().idle(self.editor, self.ticks)?;
        system.begin_update(self.window)?;
        let result = (|| {
            let port = system.quickdraw().graphics(WindowId(self.window));
            port.reset()?;
            port.colors(0xffffff, 0xffffff)?;
            port.paint_rect(Rect::new(0, 0, size.h, size.v))?;
            system.textedit().draw(self.editor, WindowId(self.window))?;
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
                edit.cut(self.editor)?;
            }
            crate::menus::COPY =>
            {
                edit.copy(self.editor)?;
            }
            crate::menus::PASTE =>
            {
                edit.paste(self.editor)?;
            }
            crate::menus::CLEAR =>
            {
                edit.delete(self.editor)?;
            }
            crate::menus::SELECT_ALL =>
            {
                edit.select(self.editor, 0, u32::MAX)?;
            }
            _ => return Ok(false)
        }
        self.draw(system)?;
        system.log("FINDER_TEXTEDIT_EDIT\n")?;
        self.report(system)?;
        Ok(true)
    }

    pub fn event(&mut self, system: &System<'_>, event: Event) -> Result<bool, Error>
    {
        self.ticks = event.input().when;
        self.prune(system)?;
        if self.window == 0
        {
            return Ok(false);
        }
        let edit = system.textedit();
        if matches!(event, Event::Activate { .. })
        {
            edit.activate(self.editor, self.active(system))?;
            self.draw(system)?;
        }
        if matches!(event, Event::Update { .. })
           && event.window().map_or(0, |window| window.0) == self.window
        {
            self.draw(system)?;
            return Ok(true);
        }
        if !self.active(system)
        {
            self.tracking = false;
            return Ok(false);
        }
        let window = self.window;
        let local = |p: Point| -> Result<Point, Error> {
            let bounds = system.window(window)?.bounds;
            Ok(Point { h: p.h - bounds.left,
                       v: p.v - bounds.top })
        };
        match event
        {
            Event::MouseDown { .. }
                if event.window().map_or(0, |window| window.0) == self.window =>
            {
                self.last_mouse = event.input().at;
                edit.click(self.editor,
                           local(event.input().at)?,
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
                    edit.move_cursor(self.editor,
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
                let key = char::from(event.key().map_or('\0', |key| key.character()));
                if key == '\r'
                   || key == '\u{8}'
                   || ('\u{1c}'..='\u{1f}').contains(&key)
                   || !key.is_control()
                {
                    edit.key(self.editor, key)?;
                    self.draw(system)?;
                    self.report(system)?;
                }
                Ok(true)
            }
            Event::Idle { .. } =>
            {
                let before = edit.info(self.editor)?;
                if self.tracking && event.input().at != self.last_mouse
                {
                    edit.click(self.editor, local(event.input().at)?, true)?;
                    self.last_mouse = event.input().at;
                }
                let after = edit.idle(self.editor, event.input().when)?;
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
