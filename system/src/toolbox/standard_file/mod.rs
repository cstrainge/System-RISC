//! Shared Standard File state. All IDs use the resident generation table;
//! native and Pascal callers enter this same model. Rendering stays in System.
#![forbid(unsafe_code)]
pub mod layout;
use alloc::{borrow::ToOwned, string::String, vec::Vec};

use mac_abi::{files::{self, CatalogInfo, FileCall, ROOT_ID},
              standard_file::*,
              *};

use crate::toolbox::{files::Manager as Files,
                     handles::{Kind, Table},
                     textedit::Editor};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action
{
    Cancel,
    Accept,
    Select(usize),
    Open(usize),
    Parent,
    Desktop,
    Volume(i32),
    Previous(i32),
    Page(i32),
    Home,
    End,
    Edit(char),
    Navigate(textedit::CursorMovement, bool),
    SelectName,
    ToggleFocus,
    NewFolder,
    Confirm,
    Dismiss,
    Backspace
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sheet
{
    Main,
    Replace,
    NewFolder,
    Error
}
pub struct Dialog
{
    saved_name:          Option<String>,
    pub owner:           u64,
    pub window:          WindowId,
    pub auxiliary:       WindowId,
    pub auxiliary_sheet: Sheet,
    pub mode:            u32,
    pub location:        (u32, u32),
    pub entries:         Vec<CatalogInfo>,
    pub selected:        Option<usize>,
    pub first:           usize,
    pub prompt:          String,
    pub editor:          Editor,
    pub focus_name:      bool,
    pub state:           u32,
    pub result:          CatalogInfo,
    pub replacing:       bool,
    types:               Vec<[u8; 4]>,
    all:                 bool,
    pub sheet:           Sheet,
    pub error:           Status,
    pub pressed:         Option<usize>,
    pub hot:             bool,
    pub last_click:      Option<(usize, u32)>,
    pub popup:           bool,
    pub ancestors:       Vec<CatalogInfo>,
    pub typeahead:       String,
    pub typed_at:        u32
}
impl Dialog
{
    pub fn page(&self) -> usize
    {
        if self.mode == GET { 8 } else { 6 }
    }

    pub fn name(&self) -> &str
    {
        self.editor.text()
    }

    pub fn active(&self) -> bool
    {
        self.state == PENDING
    }

    pub fn can_accept(&self) -> bool
    {
        self.sheet != Sheet::Main
        || if self.mode == GET
        {
            self.selected.is_some()
        }
        else
        {
            self.selected.is_some_and(|i| self.entries[i].is_directory()) || valid_name(self.name())
        }
    }

    fn reveal(&mut self)
    {
        if let Some(i) = self.selected
        {
            if i < self.first
            {
                self.first = i;
            }
            else if i >= self.first + self.page()
            {
                self.first = i + 1 - self.page();
            }
        }
    }
}
pub struct Manager
{
    slots: Table<Dialog>
}
impl Default for Manager
{
    fn default() -> Self
    {
        Self { slots: Table::new(Kind::StandardFiles) }
    }
}
fn file(files: &mut Files, operation: u32, volume: u32, cnid: u32, index: u32, name: &str)
        -> Result<FileCall, Status>
{
    let mut c = FileCall { operation,
                           volume,
                           cnid,
                           index,
                           ..Default::default() };
    files.request_owned(0, &mut c, name, &mut [])?;
    Ok(c)
}
fn valid_name(name: &str) -> bool
{
    mac_formats::text::encode(name).is_ok_and(|b| {
                                       !b.is_empty()
                                       && b.len() <= 31
                                       && !b.contains(&0)
                                       && !b.contains(&b':')
                                       && b != b"."
                                       && b != b".."
                                   })
}
fn inline(bytes: &[u8], length: u32) -> Result<&str, Status>
{
    core::str::from_utf8(bytes.get(..length as usize).ok_or(BAD_ARGUMENT)?).map_err(|_| {
                                                                               BAD_ARGUMENT
                                                                           })
}
impl Manager
{
    pub fn get(&self, id: u32) -> Result<&Dialog, Status>
    {
        self.slots.get(id).ok_or(NOT_FOUND)
    }

    pub fn get_mut(&mut self, id: u32) -> Result<&mut Dialog, Status>
    {
        self.slots.get_mut(id).ok_or(NOT_FOUND)
    }

    pub fn active(&self, owner: u64) -> Option<u32>
    {
        self.slots.iter().find(|(_, d)| d.owner == owner && d.active()).map(|(id, _)| id)
    }

    pub fn remove_owner(&mut self, owner: u64)
    {
        loop
        {
            let id = self.slots.iter().find(|(_, d)| d.owner == owner).map(|(id, _)| id);
            if let Some(id) = id
            {
                self.slots.remove(id);
            }
            else
            {
                break;
            }
        }
    }

    pub fn request(&mut self, owner: u64, call: &mut StandardFileCall, files: &mut Files)
                   -> Result<(), Status>
    {
        if call.reserved != [0; 2]
        {
            return Err(BAD_ARGUMENT);
        }
        if call.operation == BEGIN
        {
            if self.active(owner).is_some()
            {
                return Err(SERVICE_BUSY);
            }
            if call.mode > PUT || !(-1..=MAX_TYPES as i32).contains(&call.type_count)
            {
                return Err(BAD_ARGUMENT);
            }
            let prompt = inline(&call.prompt, call.prompt_length)?;
            let name = inline(&call.name, call.name_length)?;
            if call.mode == PUT && !name.is_empty() && !valid_name(name)
            {
                return Err(BAD_ARGUMENT);
            }
            self.slots.reserve().map_err(|_| NO_MEMORY)?;
            let mut editor =
                Editor::new(Rect::new(15, 157, 4111, 173), Rect::new(15, 157, 227, 173))?;
            editor.set_text(name)?;
            editor.select(0, name.len() as u32)?;
            editor.activate(true);
            let volume = if call.volume == 0
            {
                file(files, files::VOLUME_INDEX, 0, 0, 0, "")?.volume_info.id
            }
            else
            {
                call.volume
            };
            let directory = if call.directory == 0 { ROOT_ID } else { call.directory };
            let mut types = Vec::new();
            let n = call.type_count.max(0) as usize;
            types.try_reserve_exact(n).map_err(|_| NO_MEMORY)?;
            types.extend_from_slice(&call.types[..n]);
            let mut label = String::new();
            label.try_reserve(prompt.len()).map_err(|_| NO_MEMORY)?;
            label.push_str(prompt);
            let mut d = Dialog { saved_name: None,
                                 owner,
                                 window: WindowId(0),
                                 auxiliary: WindowId(0),
                                 auxiliary_sheet: Sheet::Main,
                                 mode: call.mode,
                                 location: (volume, directory),
                                 entries: Vec::new(),
                                 selected: None,
                                 first: 0,
                                 prompt: label,
                                 editor,
                                 focus_name: call.mode == PUT,
                                 state: PENDING,
                                 result: Default::default(),
                                 replacing: false,
                                 types,
                                 all: call.type_count == -1,
                                 sheet: Sheet::Main,
                                 error: OK,
                                 pressed: None,
                                 hot: false,
                                 last_click: None,
                                 popup: false,
                                 ancestors: Vec::new(),
                                 typeahead: String::new(),
                                 typed_at: 0 };
            Self::load(&mut d, files)?;
            call.handle = self.slots.insert(d).map_err(|_| NO_MEMORY)?;
        }
        else
        {
            let d = self.get(call.handle)?;
            if d.owner != owner
            {
                return Err(ACCESS_DENIED);
            }
            if call.operation == DISPOSE
            {
                self.slots.remove(call.handle);
                return Ok(());
            }
            if call.operation != POLL
            {
                return Err(NOT_SUPPORTED);
            }
        }
        let d = self.get(call.handle)?;
        call.state = d.state;
        call.replacing = u32::from(d.replacing);
        call.entry = d.result;
        Ok(())
    }

    fn load(d: &mut Dialog, files: &mut Files) -> Result<(), Status>
    {
        let mut entries = Vec::new();
        let mut ancestors = Vec::new();
        if d.location.0 == 0
        {
            let n = file(files, files::VOLUME_COUNT, 0, 0, 0, "")?.index;
            for i in 0..n
            {
                let v = file(files, files::VOLUME_INDEX, 0, 0, i, "")?.volume_info;
                let e = file(files, files::CATALOG, v.id, ROOT_ID, 0, "")?.entry;
                entries.try_reserve(1).map_err(|_| NO_MEMORY)?;
                entries.push(e);
            }
        }
        else
        {
            let directory = file(files, files::CATALOG, d.location.0, d.location.1, 0, "")?.entry;
            if !directory.is_directory()
            {
                return Err(BAD_ARGUMENT);
            }
            for i in 0..u32::from(directory.child_count)
            {
                let e =
                    file(files, files::DIRECTORY_ENTRY, d.location.0, d.location.1, i, "")?.entry;
                if d.mode == GET
                   && !d.all
                   && (e.finder_flags() & 0x4000 != 0
                       || !e.is_directory() && !d.types.contains(&e.file_type))
                {
                    continue;
                }
                if entries.len() >= 4096
                {
                    return Err(NO_MEMORY);
                }
                entries.try_reserve(1).map_err(|_| NO_MEMORY)?;
                entries.push(e);
            }
            let mut e = directory;
            loop
            {
                ancestors.try_reserve(1).map_err(|_| NO_MEMORY)?;
                ancestors.push(e);
                if e.id == ROOT_ID
                {
                    break;
                }
                if ancestors.len() >= 128
                {
                    return Err(BAD_ARGUMENT);
                }
                e = file(files, files::CATALOG, d.location.0, e.parent, 0, "")?.entry;
            }
        }
        d.entries = entries;
        d.ancestors = ancestors;
        d.selected = if d.mode == GET && !d.entries.is_empty() { Some(0) } else { None };
        d.first = 0;
        d.last_click = None;
        d.popup = false;
        d.typeahead.clear();
        Ok(())
    }

    pub fn navigate_to(&mut self, id: u32, files: &mut Files, location: (u32, u32))
                       -> Result<(), Status>
    {
        let d = self.get_mut(id)?;
        let old = d.location;
        d.location = location;
        if let Err(e) = Self::load(d, files)
        {
            d.location = old;
            return Err(e);
        }
        Ok(())
    }

    pub fn action(&mut self, id: u32, files: &mut Files, action: Action, when: u32)
                  -> Result<(), Status>
    {
        let d = self.get_mut(id)?;
        if !d.active()
        {
            return Err(NOT_FOUND);
        }
        if action == Action::Cancel
        {
            if d.sheet == Sheet::Main
            {
                d.state = CANCELLED;
            }
            else
            {
                if let Some(name) = d.saved_name.take()
                {
                    d.editor.set_text(&name)?;
                    d.editor.select(0, name.len() as u32)?;
                }
                d.sheet = Sheet::Main;
            }
            return Ok(());
        }
        if action == Action::Dismiss
        {
            d.sheet = Sheet::Main;
            return Ok(());
        }
        if d.sheet == Sheet::Error
        {
            if action == Action::Accept || action == Action::Confirm
            {
                if let Some(name) = d.saved_name.take()
                {
                    d.editor.set_text(&name)?;
                    d.editor.select(0, name.len() as u32)?;
                }
                d.sheet = Sheet::Main;
            }
            return Ok(());
        }
        if d.sheet == Sheet::Replace
        {
            if action == Action::Accept || action == Action::Confirm
            {
                d.replacing = true;
                d.state = ACCEPTED;
            }
            return Ok(());
        }
        if d.sheet == Sheet::NewFolder && matches!(action, Action::Accept | Action::Confirm)
        {
            let name = d.name().to_owned();
            let mut c = FileCall { operation: files::NEW_FOLDER,
                                   volume: d.location.0,
                                   cnid: d.location.1,
                                   position: u64::from(when),
                                   ..Default::default() };
            files.request_owned(d.owner, &mut c, &name, &mut [])?;
            d.sheet = Sheet::Main;
            d.location.1 = c.entry.id;
            if let Some(name) = d.saved_name.take()
            {
                d.editor.set_text(&name)?;
                d.editor.select(0, name.len() as u32)?;
            }
            return Self::load(d, files);
        }
        match action
        {
            Action::Select(i) | Action::Open(i) =>
            {
                if i >= d.entries.len()
                {
                    return Err(BAD_ARGUMENT);
                }
                d.selected = Some(i);
                d.focus_name = false;
                d.reveal();
                if d.mode == PUT && !d.entries[i].is_directory()
                {
                    d.editor.set_text(d.entries[i].name())?;
                    d.editor.select(0, d.editor.text().len() as u32)?;
                }
                if matches!(action, Action::Open(_))
                {
                    return self.action(id, files, Action::Accept, when);
                }
            }
            Action::Accept | Action::Confirm =>
            {
                if !d.can_accept()
                {
                    return Ok(());
                }
                if let Some(e) = d.selected.map(|i| d.entries[i]).filter(CatalogInfo::is_directory)
                {
                    return self.navigate_to(id, files, (e.volume, e.id));
                }
                if d.mode == GET
                {
                    let e = d.entries[d.selected.ok_or(BAD_ARGUMENT)?];
                    // Alias resolution requires the Alias Manager. Never claim
                    // success on the unresolved alias's resource fork.
                    if e.finder_flags() & 0x8000 != 0
                    {
                        return Err(NOT_SUPPORTED);
                    }
                    d.result = e;
                    d.state = ACCEPTED;
                }
                else
                {
                    if d.location.0 == 0 || !valid_name(d.name())
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    if file(files, files::VOLUME_INFO, d.location.0, 0, 0, "")?.volume_info
                                                                               .read_only
                       != 0
                    {
                        return Err(READ_ONLY);
                    }
                    let found = file(files, files::LOOKUP, d.location.0, d.location.1, 0, d.name());
                    match found
                    {
                        Ok(c) =>
                        {
                            if c.entry.is_directory()
                            {
                                let volume = d.location.0;
                                return self.navigate_to(id, files, (volume, c.entry.id));
                            }
                            if c.entry.is_locked()
                            {
                                return Err(ACCESS_DENIED);
                            }
                            d.result = c.entry;
                            d.sheet = Sheet::Replace;
                        }
                        Err(NOT_FOUND) =>
                        {
                            let mut e = CatalogInfo { volume: d.location.0,
                                                      parent: d.location.1,
                                                      ..Default::default() };
                            e.name[..d.name().len()].copy_from_slice(d.name().as_bytes());
                            e.name_length = d.name().len() as u32;
                            d.result = e;
                            d.state = ACCEPTED;
                        }
                        Err(e) => return Err(e)
                    }
                }
            }
            Action::Parent =>
            {
                let l = if d.location.1 == ROOT_ID
                {
                    (0, 0)
                }
                else
                {
                    (d.location.0,
                     file(files, files::CATALOG, d.location.0, d.location.1, 0, "")?.entry.parent)
                };
                return self.navigate_to(id, files, l);
            }
            Action::Desktop => return self.navigate_to(id, files, (0, 0)),
            Action::Volume(delta) =>
            {
                let n = file(files, files::VOLUME_COUNT, 0, 0, 0, "")?.index;
                let mut current = 0;
                for i in 0..n
                {
                    if file(files, files::VOLUME_INDEX, 0, 0, i, "")?.volume_info.id == d.location.0
                    {
                        current = i;
                    }
                }
                let next =
                    (current as i64 + i64::from(delta)).rem_euclid(i64::from(n.max(1))) as u32;
                let v = file(files, files::VOLUME_INDEX, 0, 0, next, "")?.volume_info.id;
                return self.navigate_to(id, files, (v, ROOT_ID));
            }
            Action::Previous(delta) | Action::Page(delta) =>
            {
                if !d.entries.is_empty()
                {
                    let step = if matches!(action, Action::Page(_))
                    {
                        delta * d.page() as i32
                    }
                    else
                    {
                        delta
                    };
                    d.selected =
                        Some((d.selected.unwrap_or(0) as i64 + i64::from(step)).clamp(0,
                                                                                      d.entries
                                                                                       .len()
                                                                                      as i64
                                                                                      - 1)
                             as usize);
                    d.focus_name = false;
                    d.reveal();
                }
            }
            Action::Home =>
            {
                d.selected = (!d.entries.is_empty()).then_some(0);
                d.reveal();
            }
            Action::End =>
            {
                d.selected = d.entries.len().checked_sub(1);
                d.reveal();
            }
            Action::ToggleFocus =>
            {
                if d.mode == PUT
                {
                    d.focus_name = !d.focus_name;
                    d.selected = None;
                }
            }
            Action::SelectName =>
            {
                d.focus_name = true;
                d.selected = None;
                d.editor.select(0, d.name().len() as u32)?;
            }
            Action::NewFolder =>
            {
                if d.mode == PUT && d.location.0 != 0
                {
                    d.sheet = Sheet::NewFolder;
                    d.focus_name = true;
                    d.selected = None;
                    let mut previous = String::new();
                    previous.try_reserve(d.name().len()).map_err(|_| NO_MEMORY)?;
                    previous.push_str(d.name());
                    d.saved_name = Some(previous);
                    d.editor.set_text("")?;
                    d.editor.select(0, d.name().len() as u32)?;
                }
            }
            Action::Edit(c) =>
            {
                if d.focus_name
                {
                    let start = d.editor.info().selection_start as usize;
                    let end = d.editor.info().selection_end as usize;
                    let mut candidate = String::new();
                    candidate.try_reserve(d.name().len() + c.len_utf8()).map_err(|_| NO_MEMORY)?;
                    candidate.push_str(&d.name()[..start]);
                    candidate.push(c);
                    candidate.push_str(&d.name()[end..]);
                    if valid_name(&candidate)
                    {
                        d.editor.key(c)?;
                        d.selected = None;
                    }
                }
                else
                {
                    if when.wrapping_sub(d.typed_at) > 60
                    {
                        d.typeahead.clear();
                    }
                    d.typed_at = when;
                    d.typeahead.try_reserve(c.len_utf8()).map_err(|_| NO_MEMORY)?;
                    d.typeahead.push(c);
                    let prefix = d.typeahead.to_lowercase();
                    if let Some(i) =
                        d.entries.iter().position(|e| e.name().to_lowercase().starts_with(&prefix))
                    {
                        d.selected = Some(i);
                        d.reveal();
                    }
                }
            }
            Action::Backspace =>
            {
                if d.focus_name
                {
                    d.editor.key('\u{8}')?;
                    d.selected = None;
                }
            }
            Action::Navigate(m, extend) =>
            {
                if d.focus_name
                {
                    d.editor.navigate(m, extend)?;
                }
            }
            Action::Cancel | Action::Dismiss =>
            {}
        }
        Ok(())
    }
}
