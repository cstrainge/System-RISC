//! Interactive editor using only resident TextEdit and the public Toolbox SDK.
use core::fmt::Write;

use mac_toolbox::{events::{CMD_KEY, SHIFT_KEY},
                  textedit::{CursorMovement, EditorId},
                  *};

fn error(error: Error) -> Status
{
    error.0
}

#[derive(Default)]
pub struct Editor
{
    pub window: u32,
    editor:     EditorId,
    size:       Point,
    tracking:   bool,
    last_mouse: Point
}
impl Editor
{
    pub fn close(&mut self, system: &System<'_>) -> Result<(), Status>
    {
        if self.window != 0
        {
            system.textedit().dispose(self.editor).map_err(error)?;
            system.close_window(self.window)?;
            *self = Self::default();
        }
        Ok(())
    }

    fn report(&self, system: &System<'_>) -> Result<(), Status>
    {
        let info = system.textedit().info(self.editor).map_err(error)?;
        let mut line = crate::Text::<128>::default();
        let _ = writeln!(&mut line,
                         "FINDER_TEXTEDIT_STATE length={} selection={}..{}",
                         info.text_length, info.selection_start, info.selection_end);
        system.log(line.as_str())
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

    pub fn open(&mut self, system: &System<'_>) -> Result<(), Status>
    {
        self.prune(system)?;
        if self.window == 0
        {
            let resource = system.resources()
                                 .application()
                                 .get(resources::ResourceKey { kind: *b"WIND",
                                                               id:   2003 })
                                 .map_err(error)?;
            let mut bytes = [0; 20];
            if resource.size() != bytes.len()
               || resource.read(0, &mut bytes).map_err(error)? != bytes.len()
            {
                return Err(BAD_ARGUMENT);
            }
            resource.release().map_err(error)?;
            let word = |at| i16::from_be_bytes([bytes[at], bytes[at + 1]]);
            let bounds = Rect::new(word(2), word(0), word(6), word(4));
            if word(8) != 8 || bounds.is_empty()
            {
                return Err(BAD_ARGUMENT);
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
                    return Err(e.0);
                }
            };
            if let Err(e) = system.textedit().set_text(editor, sample)
            {
                let _ = system.textedit().dispose(editor);
                system.close_window(window)?;
                return Err(e.0);
            }
            self.window = window;
            self.editor = editor;
        }
        system.select_window(self.window)?;
        system.textedit().activate(self.editor, true).map_err(error)?;
        self.draw(system)?;
        system.log("FINDER_TEXTEDIT_READY\n")?;
        self.report(system)
    }

    fn prune(&mut self, system: &System<'_>) -> Result<(), Status>
    {
        if self.window == 0
        {
            return Ok(());
        }
        match system.window(self.window)
        {
            Ok(_) =>
            {}
            Err(NOT_FOUND) =>
            {
                system.textedit().dispose(self.editor).map_err(error)?;
                *self = Self::default();
            }
            Err(status) => return Err(status)
        }
        Ok(())
    }

    pub fn draw(&mut self, system: &System<'_>) -> Result<(), Status>
    {
        let window = system.window(self.window)?;
        let size = Point { h: window.bounds.right - window.bounds.left,
                           v: window.bounds.bottom - window.bounds.top };
        if size != self.size
        {
            let view = Rect::new(8, 8, size.h - 8, size.v - 24);
            system.textedit().set_rects(self.editor, view, view).map_err(error)?;
            self.size = size;
            let mut line = crate::Text::<96>::default();
            let _ =
                writeln!(&mut line, "FINDER_TEXTEDIT_LAYOUT width={} height={}", size.h, size.v);
            system.log(line.as_str())?;
        }
        system.windows()
              .set_grow_icon_clip(WindowId(self.window),
                                  Some(Rect::new(size.h - 15, size.v - 15, size.h, size.v)))
              .map_err(error)?;
        system.begin_update(self.window)?;
        let result = (|| {
            let port = system.quickdraw().graphics(WindowId(self.window));
            port.reset().map_err(error)?;
            port.colors(0xffffff, 0xffffff).map_err(error)?;
            port.paint_rect(Rect::new(0, 0, size.h, size.v)).map_err(error)?;
            system.textedit().draw(self.editor, WindowId(self.window)).map_err(error)?;
            port.colors(0, 0xffffff).map_err(error)?;
            let mut bytes = [0; 768];
            let footer = crate::text(system, 2, &mut bytes)?;
            port.clip_rect(Rect::new(0, size.v - 20, size.h - 16, size.v)).map_err(error)?;
            port.text(quickdraw::fonts::FontSelection { family: 3,
                                                        size:   9,
                                                        face:   0 },
                      Point { h: 8,
                              v: size.v - 7 },
                      footer,
                      0)
                .map_err(error)?;
            Ok(())
        })();
        system.end_update(self.window)?;
        result
    }

    pub fn command(&mut self, system: &System<'_>, command: u32) -> Result<bool, Status>
    {
        if !self.active(system)
        {
            return Ok(false);
        }
        let edit = system.textedit();
        match command
        {
            crate::menus::CUT =>
            {
                edit.cut(self.editor).map_err(error)?;
            }
            crate::menus::COPY =>
            {
                edit.copy(self.editor).map_err(error)?;
            }
            crate::menus::PASTE =>
            {
                edit.paste(self.editor).map_err(error)?;
            }
            crate::menus::CLEAR =>
            {
                edit.delete(self.editor).map_err(error)?;
            }
            crate::menus::SELECT_ALL =>
            {
                edit.select(self.editor, 0, u32::MAX).map_err(error)?;
            }
            _ => return Ok(false)
        }
        self.draw(system)?;
        system.log("FINDER_TEXTEDIT_EDIT\n")?;
        self.report(system)?;
        Ok(true)
    }

    pub fn event(&mut self, system: &System<'_>, event: Event) -> Result<bool, Status>
    {
        self.prune(system)?;
        if self.window == 0
        {
            return Ok(false);
        }
        let edit = system.textedit();
        if event.kind == EventKind::Activate
        {
            edit.activate(self.editor, self.active(system)).map_err(error)?;
            self.draw(system)?;
        }
        if event.kind == EventKind::Update && event.message == self.window
        {
            self.draw(system)?;
            return Ok(true);
        }
        if !self.active(system)
        {
            self.tracking = false;
            return Ok(false);
        }
        let bounds = system.window(self.window)?.bounds;
        let local = |p: Point| {
            Point { h: p.h - bounds.left,
                    v: p.v - bounds.top }
        };
        match event.kind
        {
            EventKind::MouseDown if event.message == self.window =>
            {
                self.last_mouse = event.where_;
                edit.click(self.editor, local(event.where_), event.modifiers & SHIFT_KEY != 0)
                    .map_err(error)?;
                self.tracking = true;
                self.draw(system)?;
                Ok(true)
            }
            EventKind::MouseUp if self.tracking =>
            {
                self.tracking = false;
                Ok(true)
            }
            // A key release does not change browser policy or the filesystem.
            EventKind::KeyUp => Ok(true),
            EventKind::KeyDown | EventKind::AutoKey =>
            {
                let command = event.modifiers & CMD_KEY != 0;
                let movement = match (event.message >> 8) as u8
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
                    edit.move_cursor(self.editor, movement, event.modifiers & SHIFT_KEY != 0)
                        .map_err(error)?;
                    self.draw(system)?;
                    self.report(system)?;
                    return Ok(true);
                }
                if command
                {
                    return Ok(true);
                }
                let key = char::from(event.character());
                if key == '\r'
                   || key == '\u{8}'
                   || ('\u{1c}'..='\u{1f}').contains(&key)
                   || !key.is_control()
                {
                    edit.key(self.editor, key).map_err(error)?;
                    self.draw(system)?;
                    self.report(system)?;
                }
                Ok(true)
            }
            EventKind::Null =>
            {
                let state = system.events().input_state().map_err(error)?;
                let before = edit.info(self.editor).map_err(error)?;
                if self.tracking && state.mouse != self.last_mouse
                {
                    edit.click(self.editor, local(state.mouse), true).map_err(error)?;
                    self.last_mouse = state.mouse;
                }
                edit.idle(self.editor, state.ticks).map_err(error)?;
                if before != edit.info(self.editor).map_err(error)?
                {
                    self.draw(system)?;
                }
                Ok(self.tracking)
            }
            _ => Ok(false)
        }
    }
}
