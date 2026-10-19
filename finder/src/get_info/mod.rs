//! Finder-owned Get Info controllers. System owns filesystem/resource services;
//! each window retains a stable(volume,CNID) identity and private editor state.
mod draw;
pub(crate) mod format;
mod layout;
mod editing;
mod totals;
use core::fmt::Write;

use layout::{FONT, Layout};
use mac_toolbox::{events::{CMD_KEY, CONTROL_KEY, EVERY_EVENT, SHIFT_KEY},
                  textedit::EditorId,
                  *};

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
#[derive(Clone, Copy)]
struct Pressed
{
    check:  Check,
    inside: bool
}
struct Info
{
    window:         u32,
    identity:       Location,
    entry:          files::CatalogInfo,
    layout:         Layout,
    name:           EditorId,
    comments:       EditorId,
    focus:          Focus,
    dirty_name:     bool,
    dirty_comments: bool,
    read_only:      bool,
    active:         bool,
    pressed:        Option<Pressed>,
    status:         Option<Status>,
    metadata_error: Option<Status>,
    changed:        bool,
    allocated:      u64,
    logical:        u64,
    job:            Option<totals::Job>,
    kind:           Text<256>,
    version:        Text<512>,
    description:    Text<256>,
    location:       Text<768>,
    icon:           crate::icon_view::Icon
}
#[derive(Default)]
pub struct Controller
{
    windows:     [Option<Info>; CAPACITY],
    work_cursor: usize,
    changed:     bool
}
fn error(error: Error) -> Status
{
    error.0
}
fn editor(system: &System<'_>, dest: Rect, view: Rect, text: &str) -> Result<EditorId, Status>
{
    let id = system.textedit().create(dest, view).map_err(error)?;
    let mut stage = "font";
    let configured = (|| {
        system.textedit().set_font(id, FONT).map_err(error)?;
        stage = "text";
        system.textedit().set_text(id, text).map_err(error)?;
        Ok(())
    })();
    if let Err(status) = configured
    {
        let mut log = Text::<160>::default();
        let _ = writeln!(&mut log,
                         "FINDER_INFO_EDITOR_FAILED stage={} editor={} top={} status={}",
                         stage, id.0, dest.top, status);
        let _ = system.log(log.as_str());
        let _ = system.textedit().dispose(id);
        return Err(status);
    }
    Ok(id)
}
impl Info
{
    fn create(system: &System<'_>, identity: Location) -> Result<Self, Status>
    {
        let entry = system.files().catalog(identity.volume, identity.folder).map_err(error)?;
        let volume = system.files().volume(identity.volume).map_err(error)?;
        let folder = entry.is_directory();
        // This first native profile uses ordinary file/folder original geometry.
        // APPL metadata is visible, but its memory-requirements extension is deferred.
        let document = !folder && entry.file_type == *b"TEXT";
        let layout = Layout::load(system, folder, document)?;
        let title = draw::title(system, entry.name())?;
        let window = system.new_window(Rect::new(8, 46, 8 + layout.width, 46 + layout.height),
                                       title.as_str(),
                                       (layout.width, layout.height),
                                       4)?;
        let mut name = EditorId(0);
        let mut comments = EditorId(0);
        let mut stage = "close-policy";
        let configured = (|| {
            system.windows().set_close_deferred(WindowId(window), true).map_err(error)?;
            system.windows()
                  .set_grow_icon_clip(WindowId(window), Some(Rect::ZERO))
                  .map_err(error)?;
            stage = "name-editor";
            name = editor(system, layout.name, layout.name, entry.name())?;
            stage = "comments-load";
            let mut text = Text::<768>::default();
            system.files().comment(identity.volume, identity.folder, &mut text).map_err(error)?;
            stage = "comments-editor";
            let comment_bounds = layout::inset(layout.comments, 1);
            comments = editor(system, comment_bounds, comment_bounds, text.as_str())?;
            stage = "allocation";
            let allocation = if folder
            {
                files::AllocationInfo::default()
            }
            else
            {
                system.files().allocation_info(identity.volume, identity.folder).map_err(error)?
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
                                      else if entry.file_type == *b"APPL"
                                      {
                                          17252
                                      }
                                      else
                                      {
                                          12752
                                      },
                                      &mut bytes)
                              .map_err(error)?;
            kind.write_str(label).map_err(|_| NO_MEMORY)?;
            let mut version = Text::default();
            version.write_str(draw::text(system, 10, &mut bytes)?).map_err(|_| NO_MEMORY)?;
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
                            version.write_str(metadata.short()).map_err(|_| NO_MEMORY)?;
                        }
                        else
                        {
                            if !metadata.short().is_empty()
                               && !metadata.long().starts_with(metadata.short())
                            {
                                write!(&mut version, "{}, ", metadata.short()).map_err(|_| {
                                                                                  NO_MEMORY
                                                                              })?;
                            }
                            version.write_str(metadata.long()).map_err(|_| NO_MEMORY)?;
                        }
                    }
                    Err(Error(NOT_FOUND)) =>
                    {}
                    Err(error) => metadata_status = Some(error.0)
                }
                match system.resources().file_version(identity.volume, identity.folder, 2)
                {
                    Ok(metadata) =>
                    {
                        description.write_str(metadata.short()).map_err(|_| NO_MEMORY)?;
                    }
                    Err(Error(NOT_FOUND)) =>
                    {}
                    Err(error) => metadata_status = Some(error.0)
                }
            }
            let icon = crate::browser::entry_icon(system, &volume, &entry);
            let location =
                draw::location(system, identity.volume, entry.parent).unwrap_or_else(|_| {
                                                                         let mut value =
                                                                             Text::default();
                                                                         let _ =
                                                                             value.write_str("…");
                                                                         value
                                                                     });
            let info =
                Self { window,
                       identity,
                       entry,
                       layout,
                       name,
                       comments,
                       focus: Focus::Comments,
                       dirty_name: false,
                       dirty_comments: false,
                       read_only: volume.read_only != 0,
                       active: true,
                       pressed: None,
                       status: None,
                       metadata_error: metadata_status,
                       changed: false,
                       allocated: allocation.data_bytes
                                            .checked_add(allocation.resource_bytes)
                                            .ok_or(BAD_ARGUMENT)?,
                       logical: entry.data_length
                                     .checked_add(entry.resource_length)
                                     .ok_or(BAD_ARGUMENT)?,
                       job: folder.then(|| totals::Job::new(identity.volume, identity.folder)),
                       kind,
                       version,
                       description,
                       location,
                       icon };
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
                                 stage, identity.volume, identity.folder, window, status);
                let _ = system.log(log.as_str());
                if name.0 != 0
                {
                    let _ = system.textedit().dispose(name);
                }
                if comments.0 != 0
                {
                    let _ = system.textedit().dispose(comments);
                }
                let _ = system.close_window(window);
                Err(status)
            }
        }
    }

    fn editable(&self, focus: Focus) -> bool
    {
        !self.read_only
        && !self.entry.is_locked()
        && (focus != Focus::Name || !self.entry.is_name_locked())
    }

    fn current_editor(&self) -> EditorId
    {
        if self.focus == Focus::Name { self.name } else { self.comments }
    }

    fn activate_editor(&self, system: &System<'_>) -> Result<(), Status>
    {
        for (focus, id) in [(Focus::Name, self.name), (Focus::Comments, self.comments)]
        {
            system.textedit()
                  .activate(id, self.active && self.focus == focus && self.editable(focus))
                  .map_err(error)?;
        }
        Ok(())
    }

    fn fail(&mut self, system: &System<'_>, status: Status) -> Result<(), Status>
    {
        self.status = Some(status);
        let mut line = Text::<128>::default();
        let _ = writeln!(&mut line,
                         "FINDER_INFO_ERROR volume={} cnid={} status={}",
                         self.identity.volume, self.identity.folder, status);
        system.log(line.as_str())?;
        self.draw(system)
    }

    fn dispose(&self, system: &System<'_>) -> Result<(), Status>
    {
        let first = system.textedit().dispose(self.name).map_err(error);
        let second = system.textedit().dispose(self.comments).map_err(error);
        first.and(second)
    }
}
impl Controller
{
    pub fn open(&mut self, system: &System<'_>, identity: Location) -> Result<(), Status>
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
        let slot = self.windows.iter().position(Option::is_none).ok_or(NO_MEMORY)?;
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
        self.windows.iter().flatten().any(|info| info.job.as_ref().is_some_and(|job| !job.complete))
    }

    pub fn redraw_all(&self, system: &System<'_>) -> Result<(), Status>
    {
        for info in self.windows.iter().flatten()
        {
            info.draw(system)?;
        }
        Ok(())
    }

    fn close(&mut self, system: &System<'_>, slot: usize) -> Result<(), Status>
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
        let info = self.windows[slot].take().ok_or(NOT_FOUND)?;
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
    fn prune(&mut self, system: &System<'_>) -> Result<(), Status>
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
                Err(NOT_FOUND) =>
                {
                    let info = row.take().ok_or(NOT_FOUND)?;
                    info.dispose(system)?;
                }
                Err(status) => return Err(status)
            }
        }
        Ok(())
    }

    pub fn update_menus(&self, system: &System<'_>) -> Result<(), Status>
    {
        let front = system.window(0).ok().and_then(|window| {
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
        let writable = front.is_some_and(|info| info.editable(info.focus));
        for (item, enabled) in [(3, selected && writable),
                                (4, selected),
                                (5, writable),
                                (6, selected && writable),
                                (7, front.is_some())]
        {
            system.menus().set_item_enabled(1253, item, enabled).map_err(error)?;
        }
        Ok(())
    }

    pub fn event(&mut self, system: &System<'_>, event: Event) -> Result<bool, Status>
    {
        self.prune(system)?;
        if event.kind == EventKind::OsEvent
           && event.message & 0xff00_0000 == windows::WINDOW_CLOSE_REQUEST
        {
            let window = event.message & 0x00ff_ffff;
            if let Some(slot) =
                self.windows
                    .iter()
                    .position(|row| row.as_ref().is_some_and(|info| info.window == window))
            {
                self.close(system, slot)?;
                return Ok(true);
            }
        }
        if event.kind == EventKind::Activate
           && let Some(info) =
               self.windows.iter_mut().flatten().find(|info| info.window == event.message)
        {
            info.active = event.modifiers & ACTIVE_FLAG != 0;
            if !info.active
               && let Err(status) = info.commit(system)
            {
                info.fail(system, status)?;
            }
            info.activate_editor(system)?;
            return Ok(true);
        }
        if event.kind == EventKind::Update
           && let Some(info) =
               self.windows.iter().flatten().find(|info| info.window == event.message)
        {
            info.draw(system)?;
            return Ok(true);
        }
        if matches!(event.kind, EventKind::Null | EventKind::MouseUp)
        {
            for info in self.windows.iter_mut().flatten()
            {
                if info.pressed.is_some()
                {
                    info.track(system, event)?;
                    return Ok(true);
                }
            }
        }
        let front = system.window(0).ok().map(|w| w.id);
        let Some(slot) =
            self.windows
                .iter()
                .position(|row| row.as_ref().is_some_and(|info| Some(info.window) == front))
        else
        {
            if event.kind == EventKind::Null
            {
                self.progress(system)?;
            }
            return Ok(false);
        };
        if event.kind == EventKind::MenuCommand
           && let Some(selection) = system.menus().selection(&event).map_err(error)?
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
        if event.kind == EventKind::MouseDown && event.message == front.unwrap_or(0)
        {
            let info = self.windows[slot].as_mut().ok_or(NOT_FOUND)?;
            info.click(system, event)?;
            return Ok(true);
        }
        if matches!(event.kind, EventKind::KeyDown | EventKind::AutoKey | EventKind::KeyUp)
        {
            let mut batch = [Event::default(); 128];
            batch[0] = event;
            let mut count = 1;
            while count < batch.len()
            {
                match system.events().peek(EVERY_EVENT).map_err(error)?
                {
                    Some(next)
                        if matches!(next.kind,
                                    EventKind::KeyDown | EventKind::AutoKey | EventKind::KeyUp) =>
                    {
                        batch[count] = system.next_event(EVERY_EVENT)?;
                        count += 1;
                    }
                    _ => break
                }
            }
            let info = self.windows[slot].as_mut().ok_or(NOT_FOUND)?;
            for event in batch[..count].iter().copied()
            {
                if event.kind != EventKind::KeyUp
                {
                    info.key(system, event)?;
                }
            }
            if batch[..count].iter().any(|event| event.kind != EventKind::KeyUp)
            {
                info.draw(system)?;
            }
            return Ok(true);
        }
        if event.kind == EventKind::Null
        {
            let info = self.windows[slot].as_mut().ok_or(NOT_FOUND)?;
            if info.active && info.editable(info.focus)
            {
                let editor = info.current_editor();
                let old = system.textedit().info(editor).map_err(error)?;
                let new = system.textedit().idle(editor, event.when).map_err(error)?;
                if old.caret_visible != new.caret_visible
                {
                    info.draw(system)?;
                }
            }
            self.progress(system)?;
        }
        Ok(false)
    }

    fn progress(&mut self, system: &System<'_>) -> Result<(), Status>
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
                                         allocated);
                        system.log(log.as_str())?;
                    }
                }
                Err(status) =>
                {
                    let job = info.job.as_mut().ok_or(NOT_FOUND)?;
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
