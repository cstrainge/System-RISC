//! Edits remain in resident TextEdits until a deliberate transactional commit.
use mac_toolbox::{controls::{Part, StandardKind},
                  textedit::CursorMovement};

use super::*;
impl Info
{
    pub(super) fn commit(&mut self, system: &System<'_>) -> Result<(), Status>
    {
        // Persist the comment before a rename; identity is always the CNID, so
        // a successful rename never redirects the pending comment to another file.
        if self.dirty_comments
        {
            let mut bytes = [0; 1024];
            let text = system.textedit().text(self.comments, &mut bytes).map_err(error)?;
            system.files()
                  .set_comment(self.identity.volume,
                               self.identity.folder,
                               text,
                               system.now().map_err(error)?)
                  .map_err(error)?;
            self.dirty_comments = false;
            system.log("FINDER_INFO_COMMENT_SAVED\n")?;
        }
        if self.dirty_name
        {
            let mut bytes = [0; 512];
            let name = system.textedit().text(self.name, &mut bytes).map_err(error)?;
            if name != self.entry.name()
            {
                self.entry = system.files()
                                   .rename(self.identity.volume,
                                           self.identity.folder,
                                           name,
                                           system.now().map_err(error)?)
                                   .map_err(error)?;
                let title = draw::title(system, self.entry.name())?;
                system.windows().set_title(WindowId(self.window), title.as_str()).map_err(error)?;
                let mut log = Text::<256>::default();
                let _ = writeln!(&mut log,
                                 "FINDER_INFO_RENAMED volume={} cnid={} name={}",
                                 self.identity.volume,
                                 self.identity.folder,
                                 self.entry.name());
                system.log(log.as_str())?;
            }
            self.dirty_name = false;
            self.changed = true;
        }
        self.status = None;
        Ok(())
    }

    fn change_focus(&mut self, system: &System<'_>, focus: Focus) -> Result<bool, Status>
    {
        if self.focus == focus
        {
            return Ok(true);
        }
        if let Err(status) = self.commit(system)
        {
            self.fail(system, status)?;
            return Ok(false);
        }
        self.focus = focus;
        self.activate_editor(system)?;
        Ok(true)
    }

    pub(super) fn key(&mut self, system: &System<'_>, event: Event) -> Result<(), Status>
    {
        let character = event.character();
        let code = (event.message >> 8) as u8;
        if character == b'\t'
        {
            let focus = if self.focus == Focus::Name { Focus::Comments } else { Focus::Name };
            self.change_focus(system, focus)?;
            return Ok(());
        }
        if self.focus == Focus::Name && character == b'\r'
        {
            if let Err(status) = self.commit(system)
            {
                return self.fail(system, status);
            }
            self.focus = Focus::Comments;
            return self.activate_editor(system);
        }
        let editor = self.current_editor();
        let extend = event.modifiers & SHIFT_KEY != 0;
        let control = event.modifiers & (CONTROL_KEY | CMD_KEY) != 0;
        let movement = match code
        {
            0x7b => Some(CursorMovement::Left),
            0x7c => Some(CursorMovement::Right),
            0x7e => Some(CursorMovement::Up),
            0x7d => Some(CursorMovement::Down),
            0x73 =>
            {
                Some(if control
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
                Some(if control { CursorMovement::DocumentEnd } else { CursorMovement::LineEnd })
            }
            _ => None
        };
        if let Some(movement) = movement
        {
            system.textedit().move_cursor(editor, movement, extend).map_err(error)?;
            return Ok(());
        }
        if event.modifiers & CMD_KEY != 0
        {
            let command = match character.to_ascii_lowercase()
            {
                b'a' => Some(*b"sela"),
                b'c' => Some(*b"copy"),
                b'x' => Some(*b"cut "),
                b'v' => Some(*b"past"),
                _ => None
            };
            if let Some(command) = command
            {
                self.edit_command(system, u32::from_be_bytes(command))?;
            }
            return Ok(());
        }
        if !self.editable(self.focus)
        {
            return Ok(());
        }
        if character == 27
        {
            return Ok(());
        }
        let before = system.textedit().info(editor).map_err(error)?.revision;
        // Physical keyboard currently produces MacRoman ASCII; native callers
        // still pass UTF8 through resident TextEdit and clipboard services.
        if character == 8 || character == b'\r' || character >= 32
        {
            system.textedit().key(editor, char::from(character)).map_err(error)?;
            if system.textedit().info(editor).map_err(error)?.revision != before
            {
                self.dirty();
            }
        }
        Ok(())
    }

    fn dirty(&mut self)
    {
        if self.focus == Focus::Name
        {
            self.dirty_name = true;
        }
        else
        {
            self.dirty_comments = true;
        }
        self.status = None;
    }

    pub(super) fn edit_command(&mut self, system: &System<'_>, command: u32)
                               -> Result<bool, Status>
    {
        let editor = self.current_editor();
        if command == u32::from_be_bytes(*b"sela")
        {
            let length = system.textedit().info(editor).map_err(error)?.text_length;
            system.textedit().select(editor, 0, length).map_err(error)?;
            return Ok(true);
        }
        if command == u32::from_be_bytes(*b"copy")
        {
            system.textedit().copy(editor).map_err(error)?;
            return Ok(true);
        }
        if ![u32::from_be_bytes(*b"cut "),
             u32::from_be_bytes(*b"past"),
             u32::from_be_bytes(*b"cler")].contains(&command)
        {
            return Ok(false);
        }
        if !self.editable(self.focus)
        {
            return Ok(true);
        }
        if command == u32::from_be_bytes(*b"cut ")
        {
            system.textedit().cut(editor).map_err(error)?;
        }
        else if command == u32::from_be_bytes(*b"past")
        {
            system.textedit().paste(editor).map_err(error)?;
        }
        else
        {
            system.textedit().delete(editor).map_err(error)?;
        }
        self.dirty();
        Ok(true)
    }

    pub(super) fn click(&mut self, system: &System<'_>, event: Event) -> Result<(), Status>
    {
        let window = system.window(self.window)?;
        let point = Point { h: event.where_.h - window.bounds.left,
                            v: event.where_.v - window.bounds.top };
        for (focus, bounds) in
            [(Focus::Name, self.layout.name), (Focus::Comments, self.layout.comments)]
        {
            if layout::contains(bounds, point)
            {
                if self.change_focus(system, focus)?
                {
                    system.textedit()
                          .click(self.current_editor(), point, event.modifiers & SHIFT_KEY != 0)
                          .map_err(error)?;
                }
                return self.draw(system);
            }
        }
        for check in [Check::Locked, Check::Stationery]
        {
            let state = self.control(system, check, false)?;
            if state.bounds == Rect::ZERO
            {
                continue;
            }
            if system.controls()
                     .standard_hit(StandardKind::WindowFontCheckbox, state, point)
                     .map_err(error)?
               == Part::Check
            {
                if let Err(status) = self.commit(system)
                {
                    return self.fail(system, status);
                }
                self.pressed = Some(Pressed { check,
                                              inside: true });
                return self.draw(system);
            }
        }
        Ok(())
    }

    pub(super) fn track(&mut self, system: &System<'_>, event: Event) -> Result<(), Status>
    {
        let Some(pressed) = self.pressed
        else
        {
            return Ok(());
        };
        let window = system.window(self.window)?;
        let point = if event.kind == EventKind::Null
        {
            system.events().mouse().map_err(error)?
        }
        else
        {
            event.where_
        };
        let point = Point { h: point.h - window.bounds.left,
                            v: point.v - window.bounds.top };
        let state = self.control(system, pressed.check, false)?;
        let inside = system.controls()
                           .standard_hit(StandardKind::WindowFontCheckbox, state, point)
                           .map_err(error)?
                     == Part::Check;
        if event.kind == EventKind::MouseUp
        {
            self.pressed = None;
            if inside
            {
                let when = system.now().map_err(error)?;
                let result = match pressed.check
                {
                    Check::Locked =>
                    {
                        system.files().set_locked(self.identity.volume,
                                                  self.identity.folder,
                                                  !self.entry.is_locked(),
                                                  when)
                    }
                    Check::Stationery =>
                    {
                        system.files().set_stationery(self.identity.volume,
                                                      self.identity.folder,
                                                      !self.entry.is_stationery(),
                                                      when)
                    }
                };
                match result
                {
                    Ok(entry) =>
                    {
                        self.entry = entry;
                        let volume = system.files().volume(self.identity.volume).map_err(error)?;
                        self.icon = crate::browser::entry_icon(system, &volume, &self.entry);
                        self.status = None;
                        self.changed = true;
                        self.activate_editor(system)?;
                    }
                    Err(error) => return self.fail(system, error.0)
                }
                let mut log = Text::<128>::default();
                let _ = writeln!(&mut log,
                                 "FINDER_INFO_FLAGS locked={} stationery={}",
                                 u8::from(self.entry.is_locked()),
                                 u8::from(self.entry.is_stationery()));
                system.log(log.as_str())?;
            }
            return self.draw(system);
        }
        if inside != pressed.inside
        {
            self.pressed = Some(Pressed { inside,
                                          ..pressed });
            self.draw(system)?;
        }
        Ok(())
    }
}
