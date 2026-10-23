//! Document ownership and modal policy. System owns dialog drawing/tracking;
//! cancelling or failing an operation leaves every unsaved buffer owned here.
use core::fmt::Write;

use super::*;
const MAX_DOCUMENTS: usize = 8;
#[derive(Clone, Copy)]
enum AlertPurpose
{
    Error,
    Close(usize)
}
pub(super) struct Application<'a>
{
    editors:           [Option<editor::Editor<'a>>; MAX_DOCUMENTS],
    alert:             Option<dialogs::Alert<'a>>,
    purpose:           AlertPurpose,
    documents_pending: bool,
    quitting:          bool,
    welcome:           Option<usize>,
    untitled:          u32,
    settings:          preferences::Settings,
    menu_policy:       menus::Policy,
    location:          standard_file::Location
}
impl<'a> Application<'a>
{
    pub fn new(system: &System<'a>) -> Result<Self, Error>
    {
        let files = system.files();
        let mut location = standard_file::Location { volume:    files.volume_at(0)?.id,
                                                     directory: files::ROOT_ID };
        for at in 0..files.volume_count()?
        {
            let volume = files.volume_at(at)?;
            if volume.startup
            {
                location.volume = volume.id;
                break;
            }
        }
        match files.lookup(location.volume, location.directory, "Documents")
        {
            Ok(directory) => location.directory = directory.id,
            Err(Error::NotFound) => (),
            Err(error) => return Err(error)
        }
        let loaded = preferences::Settings::load(system);
        let mut app = Self { editors: core::array::from_fn(|_| None),
                             alert: None,
                             purpose: AlertPurpose::Error,
                             documents_pending: true,
                             quitting: false,
                             welcome: None,
                             untitled: 0,
                             settings: loaded.unwrap_or_default(),
                             location,
                             menu_policy: menus::Policy::default() };
        app.receive(system)?;
        if app.editors.iter().all(Option::is_none) && app.alert.is_none()
        {
            app.welcome = Some(app.new_document(system)?);
        }
        if let Err(error) = loaded
        {
            if app.alert.is_none()
            {
                app.preference_error(system, 7, error)?;
            }
            else
            {
                system.log("TEXT_EDIT_PREFERENCES_LOAD_FAILED\n")?;
            }
        }
        Ok(app)
    }

    fn modal_menus(system: &System<'a>, enabled: bool) -> Result<(), Error>
    {
        for id in [128, 129, 130, 131]
        {
            system.menus().set_item_enabled(MenuId(id), 0, enabled)?;
        }
        Ok(())
    }

    fn error(&mut self, system: &System<'a>, name: &str, error: Error) -> Result<(), Error>
    {
        self.quitting = false;
        let mut body = Text::<768>::default();
        write!(&mut body, "{name}\r{error}").map_err(|_| Error::NoMemory)?;
        system.log(body.as_str())?;
        self.alert = Some(system.dialogs().alert_owned(mac_toolbox::resources::APPLICATION,
                                                        2001,
                                                        body.as_str())?);
        self.purpose = AlertPurpose::Error;
        Self::modal_menus(system, false)
    }

    fn preference_error(&mut self, system: &System<'a>, index: u32, error: Error)
                        -> Result<(), Error>
    {
        let mut name = [0; 256];
        let name = crate::text(system, index, &mut name)?;
        self.error(system, name, error)
    }

    fn new_document(&mut self, system: &System<'a>) -> Result<usize, Error>
    {
        let slot = self.editors.iter().position(Option::is_none).ok_or(Error::NoMemory)?;
        let number = self.untitled.checked_add(1).ok_or(Error::NoMemory)?;
        let mut name = [0; 256];
        let name = crate::text(system, 0, &mut name)?;
        let mut title = Text::<256>::default();
        write!(title, "{name}").map_err(|_| Error::NoMemory)?;
        if number > 1
        {
            write!(title, " {number}").map_err(|_| Error::NoMemory)?;
        }
        let mut editor = editor::Editor::with_font(self.settings.font());
        editor.new_document(system, title.as_str())?;
        self.editors[slot] = Some(editor);
        self.untitled = number;
        self.update_menus(system, Some(slot))?;
        Ok(slot)
    }

    fn open_file(&mut self, system: &System<'a>, file: processes::DocumentRef)
                 -> Result<usize, Error>
    {
        if let Some(slot) =
            self.editors
                .iter()
                .position(|editor| {
                    editor.as_ref().is_some_and(|editor| editor.document == Some(file))
                })
        {
            system.select_window(self.editors[slot].as_ref().unwrap().window)?;
            return Ok(slot);
        }
        let slot = self.editors.iter().position(Option::is_none).ok_or(Error::NoMemory)?;
        let mut editor = editor::Editor::with_font(self.settings.font());
        editor.open_document(system, file)?;
        self.editors[slot] = Some(editor);
        self.update_menus(system, Some(slot))?;
        Ok(slot)
    }

    fn discard_welcome(&mut self, system: &System<'a>) -> Result<(), Error>
    {
        if let Some(slot) = self.welcome.take()
        {
            if let Some(editor) = &self.editors[slot]
            {
                if !editor.dirty(system)?
                {
                    self.close_slot(system, slot)?;
                }
            }
        }
        Ok(())
    }

    fn receive(&mut self, system: &System<'a>) -> Result<(), Error>
    {
        self.documents_pending = false;
        while let Some(batch) = system.processes().next_document_batch()?
        {
            self.discard_welcome(system)?;
            let mut created = 0u8;
            let mut current = 0;
            let result = (|| {
                for at in 0..batch.len()
                {
                    current = at;
                    let file = batch.at(at)?;
                    let already =
                        self.editors.iter().flatten().any(|editor| editor.document == Some(file));
                    let slot = self.open_file(system, file)?;
                    if !already
                    {
                        created |= 1 << slot;
                    }
                }
                Ok::<_, Error>(())
            })();
            if let Err(error) = result
            {
                for (slot, editor) in self.editors.iter_mut().enumerate()
                {
                    if created & (1 << slot) != 0
                    {
                        *editor = None;
                    }
                }
                let name = batch.info(current).ok();
                self.error(system, name.as_ref().map_or("Text Edit", |entry| entry.name()), error)?;
            }
            batch.finish()?;
            if self.alert.is_some()
            {
                self.documents_pending = true;
                break;
            }
        }
        Ok(())
    }

    fn close_slot(&mut self, system: &System<'a>, slot: usize) -> Result<(), Error>
    {
        self.editors[slot].as_mut().ok_or(Error::NotFound)?.close(system)?;
        self.editors[slot] = None;
        if self.welcome == Some(slot)
        {
            self.welcome = None;
        }
        Ok(())
    }

    fn request_close(&mut self, system: &System<'a>, slot: usize) -> Result<(), Error>
    {
        let editor = self.editors[slot].as_ref().ok_or(Error::NotFound)?;
        if !editor.dirty(system)?
        {
            return self.close_slot(system, slot);
        }
        let mut name = [0; 256];
        let name = editor.name(system, &mut name)?;
        self.alert =
            Some(system.dialogs().alert_owned(mac_toolbox::resources::APPLICATION, 2002, name)?);
        self.purpose = AlertPurpose::Close(slot);
        Self::modal_menus(system, false)
    }

    fn save_slot(&mut self, system: &System<'a>, slot: usize, save_as: bool)
                 -> Result<bool, Error>
    {
        let empty = processes::DocumentRef { volume: VolumeId::NONE,
                                             id:     Cnid::UNSAVED };
        let mut occupied = [empty; MAX_DOCUMENTS];
        let mut count = 0;
        for (at, editor) in self.editors.iter().enumerate()
        {
            if at != slot
               && let Some(file) = editor.as_ref().and_then(|editor| editor.document)
            {
                occupied[count] = file;
                count += 1;
            }
        }
        let saved = self.editors[slot].as_mut()
                                      .ok_or(Error::NotFound)?
                                      .save(system, save_as, self.location, &occupied[..count])?;
        if saved
        {
            let file = self.editors[slot].as_ref()
                                         .and_then(|editor| editor.document)
                                         .ok_or(Error::BadAbi)?;
            let entry = system.files().catalog(file.volume, file.id)?;
            self.location = standard_file::Location { volume:    file.volume,
                                                      directory: entry.parent };
            self.welcome = self.welcome.filter(|old| *old != slot);
        }
        Ok(saved)
    }

    fn continue_quit(&mut self, system: &System<'a>) -> Result<bool, Error>
    {
        while self.quitting && self.alert.is_none()
        {
            let Some(slot) = self.editors.iter().position(Option::is_some)
            else
            {
                return Ok(false);
            };
            self.request_close(system, slot)?;
        }
        Ok(true)
    }

    fn update_menus(&mut self, system: &System<'a>, slot: Option<usize>) -> Result<(), Error>
    {
        let editor = slot.and_then(|slot| self.editors[slot].as_ref());
        self.menu_policy.update(system, editor, self.settings.font())
    }

    fn choose_font(&mut self, system: &System<'a>, slot: Option<usize>, command: u32)
                   -> Result<bool, Error>
    {
        let bytes = command.to_be_bytes();
        if !matches!(bytes[0], b'f' | b's') || !bytes[1..].iter().all(u8::is_ascii_digit)
        {
            return Ok(false);
        }
        let value = i16::from(bytes[1] - b'0') * 100
                    + i16::from(bytes[2] - b'0') * 10
                    + i16::from(bytes[3] - b'0');
        let mut font = slot.and_then(|slot| self.editors[slot].as_ref())
                           .map_or(self.settings.font(), editor::Editor::font);
        if bytes[0] == b'f'
        {
            font.family = *preferences::FAMILIES.get(value as usize).ok_or(Error::BadArgument)?;
        }
        else
        {
            if !preferences::SIZES.contains(&value)
            {
                return Err(Error::BadArgument);
            }
            font.size = value;
        }
        system.quickdraw().fonts().metrics(font)?;
        if let Some(editor) = slot.and_then(|slot| self.editors[slot].as_mut())
        {
            editor.set_font(system, font)?;
        }
        self.settings = preferences::Settings::from_font(font);
        self.update_menus(system, slot)?;
        if let Err(error) = self.settings.save(system)
        {
            self.preference_error(system, 8, error)?;
        }
        Ok(true)
    }

    pub fn event(&mut self, system: &System<'a>, event: Event<'a>, position: PointerPosition)
                 -> Result<bool, Error>
    {
        if matches!(event, Event::DocumentsAvailable { .. })
        {
            self.documents_pending = true;
        }
        if let Some(alert) = &mut self.alert
        {
            if let Some(item) = alert.handle_event(&event)?
            {
                alert.close()?;
                self.alert = None;
                Self::modal_menus(system, true)?;
                if let AlertPurpose::Close(slot) = self.purpose
                {
                    let result = match item
                    {
                        1 => self.save_slot(system, slot, false),
                        3 => Ok(true),
                        _ => Ok(false)
                    };
                    match result
                    {
                        Ok(true) => self.close_slot(system, slot)?,
                        Ok(false) => self.quitting = false,
                        Err(error) =>
                        {
                            self.error(system, "Text Edit: could not save changes", error)?
                        }
                    }
                }
                if !self.continue_quit(system)?
                {
                    return Ok(false);
                }
                if self.alert.is_none() && self.documents_pending
                {
                    self.receive(system)?;
                }
                self.update_front_menus(system)?;
            }
            return Ok(true);
        }
        if self.documents_pending
        {
            self.receive(system)?;
            if self.alert.is_some() || matches!(event, Event::DocumentsAvailable { .. })
            {
                return Ok(true);
            }
        }
        let front =
            event.window().or_else(|| system.window(WindowId(0)).ok().map(|window| window.id));
        let slot =
            self.editors
                .iter()
                .position(|editor| {
                    editor.as_ref().is_some_and(|editor| Some(editor.window) == front)
                });
        let selection = system.menus().selection(&event)?;
        let command = selection.map(|selection| selection.command);
        let update_policy = !matches!(event, Event::Idle { .. } | Event::KeyUp { .. });
        let keyboard = batch::is_keyboard(&event);
        let result = (|| {
            if command == Some(u32::from_be_bytes(*b"quit"))
            {
                self.quitting = true;
                return self.continue_quit(system);
            }
            if command == Some(u32::from_be_bytes(*b"new "))
            {
                self.welcome = None;
                self.new_document(system)?;
                return Ok(true);
            }
            if command == Some(u32::from_be_bytes(*b"open"))
            {
                if let Some(choice) = system.standard_file().standard_get_file(self.location, standard_file::TypeFilter::Types(&[TypeCode::new(*b"TEXT"), TypeCode::new(*b"JSON")]))? {
                    self.discard_welcome(system)?; self.location = choice.location();
                    self.open_file(system, processes::DocumentRef { volume: choice.entry.volume, id: choice.entry.id })?;
                }
                return Ok(true);
            }
            if let Some(command) = command
               && self.choose_font(system, slot, command)?
            {
                return Ok(true);
            }
            if command == Some(u32::from_be_bytes(*b"clos"))
               || matches!(event, Event::CloseRequested { .. })
            {
                if let Some(slot) = slot
                {
                    self.request_close(system, slot)?;
                }
                return Ok(true);
            }
            if let Some(slot) = slot
            {
                if command == Some(u32::from_be_bytes(*b"save"))
                   || command == Some(u32::from_be_bytes(*b"svas"))
                {
                    self.save_slot(system, slot, command == Some(u32::from_be_bytes(*b"svas")))?;
                    self.update_menus(system, Some(slot))?;
                    return Ok(true);
                }
                let editor = self.editors[slot].as_mut().unwrap();
                if let Some(selection) = selection
                {
                    editor.command(system, selection.command, event.input().when)?;
                }
                else if batch::is_keyboard(&event)
                {
                    let changed = batch::keyboard(system, editor, event)?;
                    if changed
                    {
                        self.update_menus(system, Some(slot))?;
                    }
                }
                else
                {
                    editor.event(system, &event, position)?;
                }
                if update_policy && !keyboard
                {
                    self.update_menus(system, Some(slot))?;
                }
            }
            Ok(true)
        })();
        match result
        {
            Ok(running) => Ok(running),
            Err(error) =>
            {
                self.error(system, "Text Edit", error)?;
                Ok(true)
            }
        }
    }

    fn update_front_menus(&mut self, system: &System<'a>) -> Result<(), Error>
    {
        if self.alert.is_some()
        {
            return Ok(());
        }
        let front = system.window(WindowId(0)).ok().map(|window| window.id);
        self.update_menus(system,
                          self.editors.iter().position(|editor| {
                                                 editor.as_ref().is_some_and(|editor| {
                                                                    Some(editor.window) == front
                                                                })
                                             }))
    }
}
