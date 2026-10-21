//! Finder-owned Get Info controllers. System owns filesystem/resource services;
//! each window retains a stable(volume,CNID) identity and private editor state.
mod draw;
pub(crate) mod format;
mod layout;
mod editing;
mod totals;
mod summary;
use core::fmt::Write;

use layout::{FONT, Layout};
use mac_toolbox::{textedit::EditorId, *};

use crate::browser::{Location, Text};
const CAPACITY: usize = 8;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus
{
    Name,
    Comments
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Check
{
    Locked,
    Stationery
}
struct Info<'a>
{
    window:         WindowId,
    identity:       Location,
    entry:          files::CatalogInfo,
    volume_info:    Option<files::VolumeInfo>,
    layout:         Layout,
    name:           textedit::Editor<'a>,
    comments:       textedit::Editor<'a>,
    focus:          Focus,
    dirty_name:     bool,
    dirty_comments: bool,
    read_only:      bool,
    active:         bool,
    controls:       [Option<controls::ControlId>; 2],
    status:         Option<Error>,
    metadata_error: Option<Error>,
    changed:        bool,
    allocated:      Option<u64>,
    logical:        u64,
    job:            Option<totals::Job>,
    kind:           Text<256>,
    version:        Text<512>,
    description:    Text<256>,
    location:       Text<768>,
    icon:           crate::icon_view::Icon
}
#[derive(Default)]
pub struct Controller<'a>
{
    windows:     [Option<Info<'a>>; CAPACITY],
    work_cursor: usize,
    changed:     bool,
    summary:     Option<summary::Summary<'a>>
}
fn editor<'a>(system: &System<'a>, dest: Rect, view: Rect, text: &str)
              -> Result<textedit::Editor<'a>, Error>
{
    let editor = system.textedit().create_owned(dest, view)?;
    editor.set_font(FONT)?;
    editor.set_text(text)?;
    Ok(editor)
}
impl<'a> Info<'a>
{
    fn create(system: &System<'a>, identity: Location) -> Result<Self, Error>
    {
        let mut entry = system.files().catalog(identity.volume, identity.folder)?;
        let volume = system.files().volume(identity.volume)?;
        let folder = entry.is_directory();
        let is_volume = entry.id == files::ROOT_ID;
        if is_volume
        {
            entry.created = volume.created;
            entry.modified = volume.modified;
        }
        // This first native profile uses ordinary file/folder original geometry.
        // APPL metadata is visible, but its memory-requirements extension is deferred.
        let document = !folder && entry.file_type == TypeCode::new(*b"TEXT");
        let layout = Layout::load(system, folder, document, is_volume)?;
        let title = draw::title(system, entry.name())?;
        let window = system.new_window(Rect::new(8, 46, 8 + layout.width, 46 + layout.height),
                                       title.as_str(),
                                       (layout.width, layout.height),
                                       4)?;
        let mut name = None;
        let mut comments = None;
        let mut stage = "close-policy";
        let configured: ToolboxResult<_> = (|| {
            system.windows().set_close_deferred(window, true)?;
            system.windows().set_grow_icon_clip(window, Some(Rect::ZERO))?;
            stage = "name-editor";
            name = Some(editor(system, layout.name, layout.name, entry.name())?);
            stage = "comments-load";
            let mut text = Text::<768>::default();
            system.files().comment(identity.volume, identity.folder, &mut text)?;
            stage = "comments-editor";
            let comment_bounds = layout::inset(layout.comments, 1);
            comments = Some(editor(system, comment_bounds, comment_bounds, text.as_str())?);
            stage = "allocation";
            let allocation = if folder
            {
                files::AllocationInfo::default()
            }
            else
            {
                system.files().allocation_info(identity.volume, identity.folder)?
            };
            stage = "resource-labels";
            let mut kind = Text::default();
            let mut bytes = [0; 512];
            let label = system.resources()
                              .application()
                              .string(if folder
                                      {
                                          11252
                                      }
                                      else if entry.file_type == TypeCode::new(*b"APPL")
                                      {
                                          17252
                                      }
                                      else
                                      {
                                          12752
                                      },
                                      &mut bytes)?;
            kind.write_str(label).map_err(|_| Error::NoMemory)?;
            if is_volume
            {
                kind = draw::filesystem(system, &volume)?;
            }
            let mut version = Text::default();
            version.write_str(draw::text(system, 10, &mut bytes)?).map_err(|_| Error::NoMemory)?;
            stage = "metadata";
            let mut description = Text::default();
            let mut metadata_status = None;
            if !folder
            {
                match system.resources().file_version(identity.volume, identity.folder, 1)
                {
                    Ok(metadata) =>
                    {
                        version = Text::default();
                        if metadata.long().is_empty()
                        {
                            version.write_str(metadata.short()).map_err(|_| Error::NoMemory)?;
                        }
                        else
                        {
                            if !metadata.short().is_empty()
                               && !metadata.long().starts_with(metadata.short())
                            {
                                write!(&mut version, "{}, ", metadata.short()).map_err(|_| {
                                                                                  Error::NoMemory
                                                                              })?;
                            }
                            version.write_str(metadata.long()).map_err(|_| Error::NoMemory)?;
                        }
                    }
                    Err(Error::NotFound) =>
                    {}
                    Err(error) => metadata_status = Some(error)
                }
                match system.resources().file_version(identity.volume, identity.folder, 2)
                {
                    Ok(metadata) =>
                    {
                        description.write_str(metadata.short()).map_err(|_| Error::NoMemory)?;
                    }
                    Err(Error::NotFound) =>
                    {}
                    Err(error) => metadata_status = Some(error)
                }
            }
            let icon = crate::browser::entry_icon(system, &volume, &entry);
            let location = (if is_volume
                           {
                               draw::volume_location(system, &volume)
                           }
                           else
                           {
                               draw::location(system, identity.volume, entry.parent)
                           }).unwrap_or_else(|_| {
                                 let mut value = Text::default();
                                 let _ = value.write_str("…");
                                 value
                             });
            let mut info = Self { window,
                                  identity,
                                  entry,
                                  volume_info: is_volume.then_some(volume),
                                  layout,
                                  name: name.take().ok_or(Error::BadArgument)?,
                                  comments: comments.take().ok_or(Error::BadArgument)?,
                                  focus: Focus::Comments,
                                  dirty_name: false,
                                  dirty_comments: false,
                                  read_only: volume.read_only,
                                  active: true,
                                  controls: [None; 2],
                                  status: None,
                                  metadata_error: metadata_status,
                                  changed: false,
                                  allocated: allocation.total_bytes()?,
                                  logical: entry.data_length
                                                .checked_add(entry.resource_length)
                                                .ok_or(Error::BadArgument)?,
                                  job: (folder && !is_volume).then(|| {
                                                                 totals::Job::new(identity.volume,
                                                                                  identity.folder)
                                                             }),
                                  kind,
                                  version,
                                  description,
                                  location,
                                  icon };
            for (index, check) in [Check::Locked, Check::Stationery].into_iter().enumerate()
            {
                let state = info.control(system, check, false)?;
                if !state.bounds.is_empty()
                {
                    info.controls[index] =
                        Some(system.controls()
                                   .create_standard(window,
                                                    controls::StandardKind::WindowFontCheckbox,
                                                    state)?
                                   .into_id());
                }
            }
            stage = "activate";
            info.activate_editor(system)?;
            stage = "draw";
            info.draw(system)?;
            Ok(info)
        })();
        match configured
        {
            Ok(info) => Ok(info),
            Err(status) =>
            {
                let mut log = Text::<192>::default();
                let _ = writeln!(&mut log,
                                 "FINDER_INFO_CREATE_FAILED stage={} volume={} cnid={} window={} \
                                  status={}",
                                 stage,
                                 identity.volume,
                                 identity.folder,
                                 window,
                                 status.status());
                let _ = system.log(log.as_str());
                let _ = system.close_window(window);
                Err(status)
            }
        }
    }

    fn editable(&self, focus: Focus) -> bool
    {
        self.volume_info.is_none()
        && !self.read_only
        && !self.entry.is_locked()
        && (focus != Focus::Name || !self.entry.is_name_locked())
    }

    fn current_editor(&self) -> EditorId
    {
        if self.focus == Focus::Name { self.name.id() } else { self.comments.id() }
    }

    fn activate_editor(&self, system: &System<'_>) -> Result<(), Error>
    {
        for (focus, id) in [(Focus::Name, self.name.id()), (Focus::Comments, self.comments.id())]
        {
            system.textedit().set_cursor_enabled(id, self.editable(focus))?;
            system.textedit()
                  .activate(id, self.active && self.focus == focus && self.editable(focus))?;
        }
        Ok(())
    }

    fn fail(&mut self, system: &System<'_>, status: Error) -> Result<(), Error>
    {
        self.status = Some(status);
        let mut line = Text::<128>::default();
        let _ = writeln!(&mut line,
                         "FINDER_INFO_ERROR volume={} cnid={} status={}",
                         self.identity.volume,
                         self.identity.folder,
                         status.status());
        system.log(line.as_str())?;
        self.draw(system)
    }

    fn dispose(&mut self, _system: &System<'_>) -> Result<(), Error>
    {
        let first = self.name.dispose();
        let second = self.comments.dispose();
        first.and(second)
    }
}
impl<'a> Controller<'a>
{
    pub fn open_selection(&mut self, system: &System<'a>,
                          identities: [Option<Location>; crate::selection::CAPACITY])
                          -> Result<(), Error>
    {
        let mut selected = identities.iter().copied().flatten();
        let Some(first) = selected.next()
        else
        {
            return Ok(());
        };
        if selected.next().is_none()
        {
            return self.open(system, first);
        }
        let summary = summary::Summary::new(system, identities)?;
        self.summary = Some(summary);
        Ok(())
    }

    pub fn open(&mut self, system: &System<'a>, identity: Location) -> Result<(), Error>
    {
        if let Some(info) = self.windows.iter_mut().flatten().find(|info| info.identity == identity)
        {
            system.select_window(info.window)?;
            let bounds = system.window(info.window)?.bounds;
            let mut log = Text::<256>::default();
            let _ = writeln!(&mut log,
                             "FINDER_INFO_REUSED volume={} cnid={} window={} bounds={},{},{},{}",
                             identity.volume,
                             identity.folder,
                             info.window,
                             bounds.left,
                             bounds.top,
                             bounds.right,
                             bounds.bottom);
            system.log(log.as_str())?;
            return info.draw(system);
        }
        let slot = self.windows.iter().position(Option::is_none).ok_or(Error::NoMemory)?;
        let info = Info::create(system, identity)?;
        let mut log = Text::<256>::default();
        let _ = writeln!(&mut log,
                         "FINDER_INFO_OPEN volume={} cnid={} window={} folder={} name={}",
                         identity.volume,
                         identity.folder,
                         info.window,
                         u8::from(info.entry.is_directory()),
                         info.entry.name());
        self.windows[slot] = Some(info);
        system.log(log.as_str())?;
        Ok(())
    }

    pub fn take_changed(&mut self) -> bool
    {
        let mut changed = core::mem::take(&mut self.changed);
        for info in self.windows.iter_mut().flatten()
        {
            changed |= info.changed;
            info.changed = false;
        }
        changed
    }

    pub fn pending(&self) -> bool
    {
        self.summary.as_ref().is_some_and(|summary| summary.pending())
        || self.windows
               .iter()
               .flatten()
               .any(|info| info.job.as_ref().is_some_and(|job| !job.complete))
    }

    fn close(&mut self, system: &System<'_>, slot: usize) -> Result<(), Error>
    {
        let Some(info) = self.windows[slot].as_mut()
        else
        {
            return Ok(());
        };
        if let Err(status) = info.commit(system)
        {
            return info.fail(system, status);
        }
        system.close_window(info.window)?;
        self.changed |= info.changed;
        let mut info = self.windows[slot].take().ok_or(Error::NotFound)?;
        let cleanup = info.dispose(system);
        let mut log = Text::<128>::default();
        let _ = writeln!(&mut log,
                         "FINDER_INFO_CLOSED volume={} cnid={} window={}",
                         info.identity.volume, info.identity.folder, info.window);
        system.log(log.as_str())?;
        cleanup
    }

    /// Prune unexpected external disposal without leaking resident TextEdits.
    /// Opt-in close requests normally ensure dirty state commits before disposal.
    fn prune(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        for row in &mut self.windows
        {
            let Some(info) = row.as_ref()
            else
            {
                continue;
            };
            match system.window(info.window)
            {
                Ok(_) =>
                {}
                Err(Error::NotFound) =>
                {
                    let mut info = row.take().ok_or(Error::NotFound)?;
                    info.dispose(system)?;
                }
                Err(status) => return Err(status)
            }
        }
        Ok(())
    }

    pub fn update_menus(&self, system: &System<'_>) -> Result<(), Error>
    {
        if self.summary
               .as_ref()
               .is_some_and(|summary| {
                   system.window(WindowId(0)).is_ok_and(|window| window.id == summary.id())
               })
        {
            for item in 3..=7
            {
                system.menus().set_item_enabled(MenuId(1253), item, false)?;
            }
            return Ok(());
        }
        if self.windows.iter().all(Option::is_none)
        {
            return Ok(());
        }
        let front = system.window(WindowId(0)).ok().and_then(|window| {
                                                       self.windows
                                                           .iter()
                                                           .flatten()
                                                           .find(|info| info.window == window.id)
                                                   });
        let selected = front.is_some_and(|info| {
                                system.textedit()
                                      .info(info.current_editor())
                                      .is_ok_and(|e| e.selection_start != e.selection_end)
                            });
        if front.is_none()
        {
            return Ok(());
        }
        let writable = front.is_some_and(|info| info.editable(info.focus));
        for (item, enabled) in [(3, selected && writable),
                                (4, selected),
                                (5, writable),
                                (6, selected && writable),
                                (7, front.is_some())]
        {
            system.menus().set_item_enabled(MenuId(1253), item, enabled)?;
        }
        Ok(())
    }

    pub fn event(&mut self, system: &System<'_>, event: Event) -> Result<bool, Error>
    {
        if let Some(summary) = self.summary.as_mut()
        {
            let id = summary.id();
            if matches!(event,Event::Other{kind:events::classic::EventKind::OsEvent,message,..} if message & 0xff00_0000==0x0200_0000 && message & 0x00ff_ffff==id.0)
            {
                self.summary = None;
                return Ok(true);
            }
            if matches!(event, Event::Idle { .. })
            {
                summary.step(system)?;
            }
            if event.window() == Some(id)
            {
                if matches!(event, Event::Update { .. })
                {
                    summary.draw(system, false)?;
                }
                return Ok(true);
            }
            if matches!(event, Event::MenuCommand { .. } | Event::KeyDown { .. })
               && system.window(WindowId(0)).is_ok_and(|window| window.id == id)
            {
                if system.menus()
                         .selection(&event)?
                         .is_some_and(|selection| selection.command == crate::menus::CLOSE)
                {
                    self.summary = None;
                }
                return Ok(true);
            }
        }
        if self.windows.iter().all(Option::is_none)
        {
            return Ok(false);
        }
        self.prune(system)?;
        if let Event::CloseRequested { window,
                                       .. } = event
           && let Some(slot) =
               self.windows
                   .iter()
                   .position(|row| row.as_ref().is_some_and(|info| info.window == window))
        {
            self.close(system, slot)?;
            return Ok(true);
        }
        if matches!(event, Event::Activate { .. })
           && let Some(info) =
               self.windows
                   .iter_mut()
                   .flatten()
                   .find(|info| info.window == event.window().unwrap_or_default())
        {
            info.active = event.input().modifiers.contains(mac_toolbox::events::Modifiers::ACTIVE);
            if !info.active
               && let Err(status) = info.commit(system)
            {
                info.fail(system, status)?;
            }
            info.activate_editor(system)?;
            return Ok(true);
        }
        if matches!(event, Event::Update { .. })
           && let Some(info) =
               self.windows
                   .iter()
                   .flatten()
                   .find(|info| info.window == event.window().unwrap_or_default())
        {
            info.draw(system)?;
            return Ok(true);
        }
        let front = system.window(WindowId(0)).ok().map(|w| w.id);
        let Some(slot) =
            self.windows
                .iter()
                .position(|row| row.as_ref().is_some_and(|info| Some(info.window) == front))
        else
        {
            if matches!(event, Event::Idle { .. })
            {
                self.progress(system)?;
            }
            return Ok(false);
        };
        if matches!(event, Event::MenuCommand { .. })
           && let Some(selection) = system.menus().selection(&event)?
        {
            if selection.command == crate::menus::CLOSE
            {
                self.close(system, slot)?;
                return Ok(true);
            }
            if let Some(info) = self.windows[slot].as_mut()
               && info.edit_command(system, selection.command)?
            {
                info.draw(system)?;
                return Ok(true);
            }
        }
        if matches!(event, Event::MouseDown { .. })
           && event.window().unwrap_or_default() == front.unwrap_or_default()
        {
            let info = self.windows[slot].as_mut().ok_or(Error::NotFound)?;
            info.click(system, event)?;
            return Ok(true);
        }
        if matches!(event, Event::KeyDown { .. } | Event::KeyUp { .. })
        {
            let mut batch = [Event::default(); 128];
            batch[0] = event;
            let mut count = 1;
            while count < batch.len()
            {
                match system.events().peek(mac_toolbox::events::EventMask::ALL)?
                {
                    Some(Event::KeyDown { .. } | Event::KeyUp { .. }) =>
                    {
                        batch[count] = system.next_event(mac_toolbox::events::EventMask::ALL)?;
                        count += 1;
                    }
                    _ => break
                }
            }
            let info = self.windows[slot].as_mut().ok_or(Error::NotFound)?;
            for event in batch[..count].iter().copied()
            {
                if !matches!(event, Event::KeyUp { .. })
                {
                    info.key(system, event)?;
                }
            }
            if batch[..count].iter().any(|event| !matches!(event, Event::KeyUp { .. }))
            {
                info.draw(system)?;
            }
            return Ok(true);
        }
        if matches!(event, Event::Idle { .. })
        {
            let info = self.windows[slot].as_mut().ok_or(Error::NotFound)?;
            if info.active && info.editable(info.focus)
            {
                let editor = info.current_editor();
                let old = system.textedit().info(editor)?;
                let new = system.textedit().idle(editor, event.input().when)?;
                if old.caret_visible != new.caret_visible
                {
                    info.draw(system)?;
                }
            }
            self.progress(system)?;
        }
        Ok(false)
    }

    fn progress(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        for offset in 0..CAPACITY
        {
            let slot = (self.work_cursor + offset) % CAPACITY;
            let Some(info) = self.windows[slot].as_mut()
            else
            {
                continue;
            };
            let Some(job) = info.job.as_mut()
            else
            {
                continue;
            };
            if job.complete
            {
                continue;
            }
            let result = job.step(system);
            let completed = job.complete;
            let (items, logical, allocated) = (job.items, job.logical, job.allocated);
            self.work_cursor = (slot + 1) % CAPACITY;
            match result
            {
                Ok(()) =>
                {
                    if completed
                    {
                        info.logical = logical;
                        info.allocated = allocated;
                        info.draw(system)?;
                        let mut log = Text::<160>::default();
                        let _ = writeln!(&mut log,
                                         "FINDER_INFO_TOTALS volume={} cnid={} items={} \
                                          logical={} allocated={}",
                                         info.identity.volume,
                                         info.identity.folder,
                                         items,
                                         logical,
                                         allocated.unwrap_or(u64::MAX));
                        system.log(log.as_str())?;
                    }
                }
                Err(status) =>
                {
                    let job = info.job.as_mut().ok_or(Error::NotFound)?;
                    job.complete = true;
                    job.error = Some(status);
                    info.fail(system, status)?;
                }
            }
            break;
        }
        Ok(())
    }
}
