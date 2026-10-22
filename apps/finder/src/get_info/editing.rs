//! Edits remain in resident TextEdits until a deliberate transactional commit.
use mac_toolbox::textedit::CursorMovement;

use super::*;
impl Info<'_>
{
    pub(super) fn commit(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        // Persist the comment before a rename; identity is always the CNID, so
        // a successful rename never redirects the pending comment to another file.
        if self.dirty_comments
        {
            let mut bytes = [0; 1024];
            let text = system.textedit().text(self.comments.id(), &mut bytes)?;
            system.files().set_comment(self.identity.volume, self.identity.folder, text)?;
            self.dirty_comments = false;
            system.log("FINDER_INFO_COMMENT_SAVED\n")?;
        }
        if self.dirty_name
        {
            let mut bytes = [0; 512];
            let name = system.textedit().text(self.name.id(), &mut bytes)?;
            if name != self.entry.name()
            {
                self.entry =
                    system.files().rename(self.identity.volume, self.identity.folder, name)?;
                let title = draw::title(system, self.entry.name())?;
                system.windows().set_title(self.window, title.as_str())?;
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

    fn change_focus(&mut self, system: &System<'_>, focus: Focus) -> Result<bool, Error>
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

    pub(super) fn key(&mut self, system: &System<'_>, event: &Event) -> Result<(), Error>
    {
        let character = event.key().map_or('\0', |key| key.character());
        let code = event.key().map_or(0, |key| key.code());
        if character == '\t'
        {
            let focus = if self.focus == Focus::Name { Focus::Comments } else { Focus::Name };
            self.change_focus(system, focus)?;
            return Ok(());
        }
        if self.focus == Focus::Name && character == '\r'
        {
            if let Err(status) = self.commit(system)
            {
                return self.fail(system, status);
            }
            self.focus = Focus::Comments;
            return self.activate_editor(system);
        }
        let editor = self.current_editor();
        let extend = event.input().modifiers.contains(mac_toolbox::events::Modifiers::SHIFT);
        let control = event.input().modifiers.intersects(mac_toolbox::events::Modifiers::CONTROL
                                                         | mac_toolbox::events::Modifiers::COMMAND);
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
            system.textedit().move_cursor(editor, movement, extend)?;
            return Ok(());
        }
        if event.input().modifiers.contains(mac_toolbox::events::Modifiers::COMMAND)
        {
            let command = match character.to_ascii_lowercase()
            {
                'a' => Some(*b"sela"),
                'c' => Some(*b"copy"),
                'x' => Some(*b"cut "),
                'v' => Some(*b"past"),
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
        if character == '\u{1b}'
        {
            return Ok(());
        }
        let before = system.textedit().info(editor)?.revision;
        // Physical keyboard currently produces MacRoman ASCII; native callers
        // still pass UTF8 through resident TextEdit and clipboard services.
        if character == '\u{8}' || character == '\r' || !character.is_control()
        {
            system.textedit().key(editor, character)?;
            if system.textedit().info(editor)?.revision != before
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
                               -> Result<bool, Error>
    {
        let editor = self.current_editor();
        if command == u32::from_be_bytes(*b"sela")
        {
            let length = system.textedit().info(editor)?.text_length;
            system.textedit().select(editor, 0, length)?;
            return Ok(true);
        }
        if command == u32::from_be_bytes(*b"copy")
        {
            system.textedit().copy(editor)?;
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
            system.textedit().cut(editor)?;
        }
        else if command == u32::from_be_bytes(*b"past")
        {
            system.textedit().paste(editor)?;
        }
        else
        {
            system.textedit().delete(editor)?;
        }
        self.dirty();
        Ok(true)
    }

    pub(super) fn click(&mut self, system: &System<'_>, event: &Event) -> Result<(), Error>
    {
        let window = system.window(self.window)?;
        let point = Point { h: event.input().at.h - window.bounds.left,
                            v: event.input().at.v - window.bounds.top };
        for (focus, bounds) in
            [(Focus::Name, self.layout.name), (Focus::Comments, self.layout.comments)]
        {
            if layout::contains(bounds, point)
            {
                if self.change_focus(system, focus)?
                {
                    system.textedit()
                          .click(self.current_editor(),
                                 point,
                                 event.input()
                                      .modifiers
                                      .contains(mac_toolbox::events::Modifiers::SHIFT))?;
                }
                return self.draw(system);
            }
        }
        for (index, check) in [Check::Locked, Check::Stationery].into_iter().enumerate()
        {
            let Some(id) = self.controls[index]
            else
            {
                continue;
            };
            let handled = system.controls().control(id).track(point, |_| {
                                                            self.commit(system)?;
                                                            self.toggle(system, check)
                                                        })?;
            if handled
            {
                return Ok(());
            }
        }
        Ok(())
    }

    fn toggle(&mut self, system: &System<'_>, check: Check) -> Result<(), Error>
    {
        let result = match check
        {
            Check::Locked =>
            {
                system.files().set_locked(self.identity.volume,
                                          self.identity.folder,
                                          !self.entry.is_locked())
            }
            Check::Stationery =>
            {
                system.files().set_stationery(self.identity.volume,
                                              self.identity.folder,
                                              !self.entry.is_stationery())
            }
        };
        match result
        {
            Ok(entry) =>
            {
                self.entry = entry;
                let volume = system.files().volume(self.identity.volume)?;
                self.icon = crate::browser::entry_icon(system, &volume, &self.entry);
                self.status = None;
                self.changed = true;
                self.activate_editor(system)?;
            }
            Err(error) => return self.fail(system, error)
        }
        let mut log = Text::<128>::default();
        let _ = writeln!(&mut log,
                         "FINDER_INFO_FLAGS locked={} stationery={}",
                         u8::from(self.entry.is_locked()),
                         u8::from(self.entry.is_stationery()));
        system.log(log.as_str())?;
        self.draw(system)
    }
}
