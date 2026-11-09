//! Standard File's CDEF25 control delegates popup geometry, hit testing and
//! drawing to the ordinary Menu Manager. Only destination policy stays here.
use super::*;
impl super::super::Toolbox
{
    pub(super) fn sf_draw_location(&mut self, id: u64, title: &str) -> Result<(), Status>
    {
        let dialog = self.standard_file.get(self.owner, id)?;
        let control = dialog.main_widgets.buttons[8];
        let frozen_caption = dialog.popup.is_some();
        let mut call = controls::ControlCall { operation: controls::INFO_OBJECT,
                                               id: control,
                                               ..Default::default() };
        self.managed_control(&mut call, &mut None)?;
        let current =
            call.standard.title.get(..call.standard.title_length as usize).ok_or(BAD_ABI)?;
        if !frozen_caption && current != title.as_bytes()
        {
            let caption = definitions::ControlDefinitionState::new(call.standard.bounds, title);
            call.standard.title = caption.title;
            call.standard.title_length = caption.title_length;
            call.operation = controls::CONFIGURE_OBJECT;
            self.managed_control(&mut call, &mut None)?;
        }
        call.operation = controls::DRAW_OBJECT;
        self.managed_control(&mut call, &mut None)
    }

    pub(super) fn sf_open_popup(&mut self, id: u64) -> Result<(), Status>
    {
        let dialog = self.standard_file.get(self.owner, id)?;
        if dialog.popup.is_some()
        {
            return Err(SERVICE_BUSY);
        }
        let count = dialog.ancestors.len();
        if count > standard_file::page::ANCESTORS
        {
            return Err(BAD_ABI);
        }
        let mut titles = [Text::default(); standard_file::page::ANCESTORS + 1];
        let mut destinations = [(0, 0); standard_file::page::ANCESTORS + 1];
        for (index, entry) in dialog.ancestors.iter().enumerate()
        {
            titles[index] = Text::new(entry.name())?;
            destinations[index] = (entry.volume, entry.id);
        }
        titles[count] = Text::new(label(0)?)?;
        let mut labels = [""; standard_file::page::ANCESTORS + 1];
        for (output, title) in labels.iter_mut().zip(&titles)
        {
            *output = title.text()?;
        }
        let window = self.windows.window(dialog.window).ok_or(NOT_FOUND)?;
        let mut anchor = layout(dialog.mode, Sheet::Main)?.item(8);
        anchor.offset(window.bounds.left, window.bounds.top);
        let (_, storage) = self.service_owners()?;
        let usable = Rect::new(self.screen.left,
                               self.screen.top + mac_abi::menus::MENU_BAR_HEIGHT,
                               self.screen.right,
                               self.screen.bottom);
        let dialog = self.standard_file.get_mut(self.owner, id)?;
        let handle = self.menus.open_popup(&storage, &labels[..count + 1], 0, anchor, usable)?;
        // No fallible work follows publication. The real Dialog retains both
        // the same owning authority and the original immutable row mapping.
        dialog.popup = Some(standard_file::Popup { handle,
                                                   storage,
                                                   destinations,
                                                   count: count + 1,
                                                   pressed: true });
        self.presentation.redraw = true;
        Ok(())
    }

    pub(super) fn sf_close_popup(&mut self, owner: u64, id: u64) -> Result<(), Status>
    {
        let dialog = self.standard_file.get(owner, id)?;
        let Some(popup) = dialog.popup.as_ref()
        else
        {
            return Ok(());
        };
        let handle = popup.handle;
        let storage = popup.storage.clone();
        let control = dialog.main_widgets.buttons[8];
        let mut cancel = controls::ControlCall { operation: controls::CANCEL_TRACKING,
                                                 id: control,
                                                 ..Default::default() };
        // The genuine window/control owner may already be stopping. Its normal
        // window teardown owns capture retirement in that case.
        if self.owner == owner && self.processes.ledger(owner).is_ok_and(|o| o.accepting())
        {
            self.managed_control(&mut cancel, &mut None)?;
        }
        self.menus.close_popup(&storage, handle)?;
        // Dispose only after the canonical MENU has actually been removed.
        let dialog = self.standard_file.get_mut(owner, id)?;
        dialog.popup = None;
        dialog.main_widgets.tracking = None;
        self.presentation.redraw = true;
        Ok(())
    }

    pub(super) fn sf_popup_input(&mut self, id: u64, input: Input, when: u32)
                                 -> Result<bool, Status>
    {
        let dialog = self.standard_file.get(self.owner, id)?;
        let Some(popup) = dialog.popup.as_ref()
        else
        {
            return Ok(false);
        };
        let handle = popup.handle;
        let pressed = popup.pressed;
        let mut choose = None;
        let mut close = false;
        match input
        {
            Input::Move(point) =>
            {
                self.presentation.redraw |=
                    self.menus.hover(point, &mut self.system_bar, &mut self.app_menu);
            }
            Input::Button { point,
                            down } =>
            {
                self.presentation.redraw |=
                    self.menus.hover(point, &mut self.system_bar, &mut self.app_menu);
                if down
                {
                    if self.menus.popup_choice(handle).is_none()
                    {
                        close = true;
                    }
                    else
                    {
                        self.standard_file
                            .get_mut(self.owner, id)?
                            .popup
                            .as_mut()
                            .ok_or(BAD_ABI)?
                            .pressed = true;
                    }
                }
                else if pressed
                {
                    choose = self.menus.popup_choice(handle).map(|s| s.item as usize - 1);
                    close = true;
                }
            }
            Input::Key { down: true,
                         mac_code,
                         character,
                         modifiers,
                         .. } =>
            {
                match mac_code
                {
                    0x35 => close = true,
                    0x24 | 0x4c =>
                    {
                        choose = self.menus.popup_choice(handle).map(|s| s.item as usize - 1);
                        close = true;
                    }
                    0x7e | 0x7d =>
                    {
                        self.menus.popup_key(handle, if mac_code == 0x7e { -1 } else { 1 })?;
                        self.presentation.redraw = true;
                    }
                    _ if modifiers & CMD_KEY != 0 && character == b'.' => close = true,
                    _ => ()
                }
            }
            Input::Key { .. } => ()
        }
        if close
        {
            let dialog = self.standard_file.get(self.owner, id)?;
            let popup = dialog.popup.as_ref().ok_or(BAD_ABI)?;
            let destination = choose.filter(|&i| i < popup.count).map(|i| popup.destinations[i]);
            let original_location = dialog.location;
            self.sf_close_popup(self.owner, id)?;
            if let Some(destination) = destination.filter(|&d| d != original_location)
            {
                // Files validates real mount/directory authority and exposes
                // any provider error through the existing informative sheet.
                if let Err(status) = self.standard_file.navigate_to(self.owner, id, destination)
                {
                    self.standard_file.get_mut(self.owner, id)?.fail(status);
                }
                WORK_READY.store(true, Ordering::Release);
                self.sf_sync_sheet(id)?;
                self.standard_file_draw(id, Damage::full())?;
            }
            else
            {
                let mode = self.standard_file.get(self.owner, id)?.mode;
                let mut damage = Damage::new();
                damage.add(layout(mode, Sheet::Main)?.item(8));
                self.standard_file_draw(id, damage)?;
            }
        }
        let _ = when;
        Ok(true)
    }
}
