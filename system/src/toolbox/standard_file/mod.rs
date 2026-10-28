//! System-owned Standard File protocol. Catalog work is a bounded owned state
//! machine; no Files guard or caller buffer survives its asynchronous requests.
#![forbid(unsafe_code)]
pub mod layout;
pub(super) mod page;
pub(super) mod view;
#[cfg(target_arch = "riscv64")]
pub(crate) mod templates;
use alloc::vec::Vec;

use mac_abi::{files::{self, CatalogInfo, FileCall, ROOT_ID, VolumeInfo},
              standard_file::*,
              *};
use page::Page;
#[cfg(target_arch = "riscv64")]
pub(crate) use templates::prepare as prepare_templates;
use view::Text;

use crate::toolbox::{handles::{CapacityPolicy, KindCode, KindLease, OwnerRef, Store},
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
    Backspace,
    Scroll(usize)
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sheet
{
    Main,
    Replace,
    NewFolder,
    Error
}
#[derive(Clone, Copy)]
pub(super) enum Stage
{
    DefaultVolume,
    Volume,
    Directory,
    Ancestor(u32),
    Scan,
    Lookup,
    CreateFolder,
    PrepareSave,
    PrepareFolder,
    VolumeCount(i32),
    FindVolume
    {
        count: u32,
        index: u32,
        delta: i32
    },
    SelectVolume(u32),
    DesktopCount,
    DesktopVolume(u32),
    DesktopRoot
    {
        index:  u32,
        volume: u32
    }
}
pub(super) enum Query
{
    File(FileCall, Text),
    Scan(files::FileScanCall),
    PrepareName
    {
        volume: u32,
        name:   Text
    }
}
#[derive(Clone, Copy)]
pub(super) struct FileResult
{
    pub operation:   u32,
    pub index:       u32,
    pub volume_info: VolumeInfo,
    pub entry:       CatalogInfo
}
#[derive(Clone, Copy)]
pub(super) enum Reply
{
    File(FileResult),
    Scan(files::FileScanCall),
    Name(Text)
}
#[derive(Default)]
pub(super) struct Widgets
{
    pub buttons:         [u64; 32],
    pub scrollbox:       u64,
    pub scroll_tracking: bool,
    pub tracking:        Option<usize>
}
pub struct Dialog
{
    saved_name:                   Option<Text>,
    pub owner:                    u64,
    pub window:                   WindowId,
    pub auxiliary:                WindowId,
    pub auxiliary_sheet:          Sheet,
    pub mode:                     u32,
    pub location:                 (u32, u32),
    pub(super) entries:           Page,
    pub(super) icons: Vec<(CatalogInfo, crate::toolbox::quickdraw::icons_service::Drawing)>,
    pub selected:                 Option<usize>,
    pub first:                    usize,
    pub(super) prompt:            Text,
    pub editor:                   Editor,
    pub focus_name:               bool,
    pub(super) name_tracking:     bool,
    pub state:                    u32,
    pub result:                   CatalogInfo,
    pub replacing:                bool,
    types:                        [[u8; 4]; MAX_TYPES],
    type_count:                   usize,
    all:                          bool,
    pub sheet:                    Sheet,
    pub error:                    Status,
    pub last_click:               Option<(usize, u32)>,
    pub popup:                    bool,
    pub ancestors:                Vec<CatalogInfo>,
    typeahead:                    Text,
    pub typed_at:                 u32,
    name_format:                  Option<mac_filesystem::Format>,
    pub(super) volume_info:       VolumeInfo,
    stage:                        Option<Stage>,
    issued:                       bool,
    pub(super) revision:          u64,
    desktop_count:                u32,
    create_when:                  u64,
    pub(super) main_widgets:      Widgets,
    pub(super) auxiliary_widgets: Widgets,
    #[cfg(target_arch = "riscv64")]
    pub(super) subject:           Option<crate::drivers::jobs::subjects::Subject>,
    #[cfg(target_arch = "riscv64")]
    pub(super) recovery_activity: Option<crate::recovery::Activity>
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

    pub fn visible_entries(&self) -> &[CatalogInfo]
    {
        &self.entries.rows[..self.entries.length]
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
            self.selected.and_then(|i| self.entries.get(i)).is_some()
        }
        else
        {
            self.selected.and_then(|i| self.entries.get(i)).is_some_and(CatalogInfo::is_directory)
            || (!self.name().is_empty() && self.stage.is_none_or(|s| matches!(s, Stage::Scan)))
        }
    }

    fn restart(&mut self, location: (u32, u32)) -> Result<(), Status>
    {
        self.revision = self.revision.checked_add(1).ok_or(NO_MEMORY)?;
        self.location = location;
        self.entries.reset();
        self.icons.clear();
        self.ancestors.clear();
        self.selected = None;
        self.first = 0;
        self.last_click = None;
        self.name_tracking = false;
        self.popup = false;
        self.typeahead.clear();
        self.issued = false;
        self.stage = Some(if location.0 == 0 { Stage::DesktopCount } else { Stage::Volume });
        Ok(())
    }

    fn reveal(&mut self) -> Result<(), Status>
    {
        if let Some(index) = self.selected
        {
            let first = if index < self.first
            {
                index
            }
            else if index >= self.first + self.page()
            {
                index + 1 - self.page()
            }
            else
            {
                self.first
            };
            if self.entries.get(index).is_none()
            {
                self.revision = self.revision.checked_add(1).ok_or(NO_MEMORY)?;
                self.issued = false;
                self.first = first;
                self.entries.request(first, false);
                self.stage = Some(Stage::Scan);
            }
        }
        Ok(())
    }

    pub(super) fn fail(&mut self, cause: Status)
    {
        self.issued = false;
        self.stage = None;
        self.error = cause;
        self.sheet = Sheet::Error;
    }

    pub(super) fn wants_icon(&self, call: &files::FileScanCall) -> bool
    {
        if call.step != files::SCAN_ENTRY
        {
            return false;
        }
        let include = self.mode != GET
                      || self.all
                      || (call.entry.finder_flags() & 0x4000 == 0
                          && (call.entry.is_directory()
                              || self.types[..self.type_count].contains(&call.entry.file_type)));
        let prefix =
            self.typeahead
                .text()
                .is_ok_and(|text| text.is_empty() || prefix_equal(call.entry.name(), text));
        self.entries.wants_entry(self.page(), include, prefix)
    }

    pub(super) fn work_pending(&self) -> bool
    {
        self.active() && (self.stage.is_some() || self.issued || self.main_widgets.scroll_tracking)
    }

    pub(super) fn query(&mut self) -> Result<Option<(u64, Query)>, Status>
    {
        if self.issued || !self.active()
        {
            return Ok(None);
        }
        let Some(stage) = self.stage
        else
        {
            return Ok(None);
        };
        if self.sheet != Sheet::Main && !matches!(stage, Stage::PrepareFolder | Stage::CreateFolder)
        {
            return Ok(None);
        }
        let mut call = FileCall { volume: self.location.0,
                                  cnid: self.location.1,
                                  ..Default::default() };
        let mut text = Text::default();
        match stage
        {
            Stage::DefaultVolume =>
            {
                call.operation = files::VOLUME_INDEX;
                call.index = 0;
            }
            Stage::Volume => call.operation = files::VOLUME_INFO,
            Stage::Directory => call.operation = files::CATALOG,
            Stage::Ancestor(id) =>
            {
                call.operation = files::CATALOG;
                call.cnid = id;
            }
            Stage::Scan =>
            {
                if !self.entries.scanning
                {
                    self.stage = None;
                    return Ok(None);
                }
                self.issued = true;
                return Ok(Some((self.revision,
                                Query::Scan(self.entries
                                                .query(self.location.0, self.location.1)))));
            }
            Stage::Lookup =>
            {
                call.operation = files::LOOKUP;
                text = Text::new(self.name())?;
            }
            Stage::CreateFolder =>
            {
                call.operation = files::NEW_FOLDER;
                call.position = self.create_when;
                text = Text::new(self.name())?;
            }
            Stage::PrepareSave | Stage::PrepareFolder =>
            {
                self.issued = true;
                return Ok(Some((self.revision,
                                Query::PrepareName { volume: self.location.0,
                                                     name:   Text::new(self.name())? })));
            }
            Stage::VolumeCount(_) | Stage::DesktopCount => call.operation = files::VOLUME_COUNT,
            Stage::FindVolume { index,
                                .. }
            | Stage::SelectVolume(index)
            | Stage::DesktopVolume(index) =>
            {
                call.operation = files::VOLUME_INDEX;
                call.index = index;
            }
            Stage::DesktopRoot { volume,
                                 .. } =>
            {
                call.operation = files::CATALOG;
                call.volume = volume;
                call.cnid = ROOT_ID;
            }
        }
        self.issued = true;
        Ok(Some((self.revision, Query::File(call, text))))
    }

    fn prepare_name(&mut self, folder: bool) -> Result<(), Status>
    {
        self.revision = self.revision.checked_add(1).ok_or(NO_MEMORY)?;
        self.issued = false;
        if self.volume_info.read_only != 0
        {
            return Err(READ_ONLY);
        }
        if let Some(format) = self.name_format
        {
            let (bytes,_)=format.prepare_name(self.name()).map_err(|e|match e{
                mac_filesystem::Error::BudgetExceeded(_)|mac_filesystem::Error::AllocationFailed(_)=>NO_MEMORY,
                _=>BAD_ARGUMENT
            })?;
            let name = core::str::from_utf8(&bytes).map_err(|_| BAD_ABI)?;
            self.editor.set_text(name)?;
            self.stage = Some(if folder { Stage::CreateFolder } else { Stage::Lookup });
        }
        else
        {
            self.stage = Some(if folder { Stage::PrepareFolder } else { Stage::PrepareSave });
        }
        Ok(())
    }

    pub(super) fn receive(&mut self, revision: u64, reply: Result<Reply, Status>)
                          -> Result<bool, Status>
    {
        if revision != self.revision || !self.active()
        {
            return Ok(false);
        }
        self.issued = false;
        let Some(stage) = self.stage
        else
        {
            return Err(BAD_ABI);
        };
        let reply = match reply
        {
            Err(SCAN_STALE) if matches!(stage, Stage::Scan) =>
            {
                self.entries.reset();
                self.selected = None;
                self.first = 0;
                return Ok(true);
            }
            Err(NOT_FOUND) if matches!(stage, Stage::Lookup) =>
            {
                let name = self.name().as_bytes();
                let mut entry = CatalogInfo { volume: self.location.0,
                                              parent: self.location.1,
                                              ..Default::default() };
                let dest = entry.name.get_mut(..name.len()).ok_or(BAD_ARGUMENT)?;
                dest.copy_from_slice(name);
                entry.name_length = name.len() as u32;
                self.result = entry;
                self.state = ACCEPTED;
                self.stage = None;
                return Ok(true);
            }
            Err(cause) =>
            {
                self.stage = None;
                return Err(cause);
            }
            Ok(reply) => reply
        };
        match (stage, reply)
        {
            (Stage::DefaultVolume, Reply::File(call)) =>
            {
                self.location.0 = call.volume_info.id;
                self.stage = Some(Stage::Volume);
            }
            (Stage::Volume, Reply::File(call)) =>
            {
                self.volume_info = call.volume_info;
                self.name_format = match call.volume_info.filesystem
                {
                    files::FILESYSTEM_HFS => Some(mac_filesystem::Format::Hfs),
                    files::FILESYSTEM_HFS_B => Some(mac_filesystem::Format::HfsB),
                    _ => None
                };
                self.stage = Some(Stage::Directory);
            }
            (Stage::Directory, Reply::File(call)) =>
            {
                if !call.entry.is_directory()
                {
                    return Err(BAD_ARGUMENT);
                }
                self.ancestors.clear();
                self.ancestors.push(call.entry);
                self.stage = Some(if call.entry.id == ROOT_ID
                                  {
                                      Stage::Scan
                                  }
                                  else
                                  {
                                      Stage::Ancestor(call.entry.parent)
                                  });
            }
            (Stage::Ancestor(_), Reply::File(call)) =>
            {
                if !call.entry.is_directory()
                   || self.ancestors.iter().any(|e| e.id == call.entry.id)
                {
                    return Err(CORRUPT_DATA);
                }
                self.ancestors.push(call.entry);
                self.stage = Some(if call.entry.id == ROOT_ID
                                     || self.ancestors.len() == page::ANCESTORS
                                  {
                                      Stage::Scan
                                  }
                                  else
                                  {
                                      Stage::Ancestor(call.entry.parent)
                                  });
            }
            (Stage::Scan, Reply::Scan(call)) =>
            {
                let include = self.mode != GET
                              || self.all
                              || (call.entry.finder_flags() & 0x4000 == 0
                                  && (call.entry.is_directory()
                                      || self.types[..self.type_count].contains(&call.entry
                                                                                     .file_type)));
                let prefix = self.typeahead.text()?;
                let prefix_match = prefix.is_empty() || prefix_equal(call.entry.name(), prefix);
                let changed = self.entries.accept(call, self.page(), include, prefix_match)?;
                if let Some(selected) = self.entries.selected.take()
                {
                    self.selected = Some(selected);
                }
                if self.selected.is_none() && self.mode == GET && self.entries.length != 0
                {
                    self.selected = Some(self.entries.first);
                }
                self.first = self.entries.first;
                if !self.entries.scanning
                {
                    self.stage = None;
                }
                return Ok(changed && self.entries.ready(self.page()));
            }
            (Stage::Lookup, Reply::File(call)) =>
            {
                if call.entry.is_directory()
                {
                    return self.restart((call.entry.volume, call.entry.id)).map(|()| true);
                }
                if call.entry.is_locked()
                {
                    return Err(ACCESS_DENIED);
                }
                self.result = call.entry;
                self.sheet = Sheet::Replace;
                self.stage = None;
            }
            (Stage::PrepareSave, Reply::Name(name)) | (Stage::PrepareFolder, Reply::Name(name)) =>
            {
                self.editor.set_text(name.text()?)?;
                self.stage = Some(if matches!(stage, Stage::PrepareFolder)
                                  {
                                      Stage::CreateFolder
                                  }
                                  else
                                  {
                                      Stage::Lookup
                                  });
            }
            (Stage::CreateFolder, Reply::File(call)) =>
            {
                if let Some(name) = self.saved_name.take()
                {
                    self.editor.set_text(name.text()?)?;
                    self.editor.select(0, self.name().len() as u32)?;
                }
                self.sheet = Sheet::Main;
                return self.restart((call.entry.volume, call.entry.id)).map(|()| true);
            }
            (Stage::VolumeCount(delta), Reply::File(call)) =>
            {
                if call.index == 0
                {
                    return Err(NOT_FOUND);
                }
                self.stage = Some(Stage::FindVolume { count: call.index,
                                                      index: 0,
                                                      delta });
            }
            (Stage::FindVolume { count,
                                 index,
                                 delta },
             Reply::File(call)) =>
            {
                self.stage = Some(if call.volume_info.id == self.location.0
                                  {
                                      Stage::SelectVolume((i64::from(index)+i64::from(delta)).rem_euclid(i64::from(count))as u32)
                                  }
                                  else if index + 1 < count
                                  {
                                      Stage::FindVolume { count,
                                                          index: index + 1,
                                                          delta }
                                  }
                                  else
                                  {
                                      Stage::SelectVolume(0)
                                  });
            }
            (Stage::SelectVolume(_), Reply::File(call)) =>
            {
                return self.restart((call.volume_info.id, ROOT_ID)).map(|()| true);
            }
            (Stage::DesktopCount, Reply::File(call)) =>
            {
                self.desktop_count = call.index;
                self.entries.desktop_total(call.index as usize);
                self.stage = if call.index == 0 { None } else { Some(Stage::DesktopVolume(0)) };
            }
            (Stage::DesktopVolume(index), Reply::File(call)) =>
            {
                self.stage = Some(Stage::DesktopRoot { index,
                                                       volume: call.volume_info.id });
            }
            (Stage::DesktopRoot { index,
                                  .. },
             Reply::File(call)) =>
            {
                let local = (index as usize).checked_sub(self.first);
                if let Some(local) = local.filter(|&i| i < self.page())
                {
                    self.entries.rows[local] = call.entry;
                    self.entries.length = local + 1;
                }
                let next = index + 1;
                self.stage = if next < self.desktop_count
                                && next < (self.first + self.page()) as u32
                {
                    Some(Stage::DesktopVolume(next))
                }
                else
                {
                    None
                };
                if self.mode == GET && self.selected.is_none() && self.entries.length != 0
                {
                    self.selected = Some(self.first);
                }
            }
            _ => return Err(BAD_ABI)
        }
        Ok(true)
    }
}
fn prefix_equal(name: &str, prefix: &str) -> bool
{
    let mut name = name.chars().flat_map(char::to_lowercase);
    prefix.chars().flat_map(char::to_lowercase).all(|character| name.next() == Some(character))
}
pub struct Manager
{
    slots: Store<Dialog>,
    publication: Option<mac_object_table::store::PublicationReservation<crate::toolbox::handles::InvocationGate>>
}
pub(crate) fn specification(state: bool) -> mac_object_table::registry::Specification<'static>
{
    mac_object_table::registry::Specification { kind:     KindCode(if state
                                                                   {
                                                                       *b"SFST"
                                                                   }
                                                                   else
                                                                   {
                                                                       *b"SFDL"
                                                                   }),
                                                label:    if state
                                                {
                                                    "Standard File Package state"
                                                }
                                                else
                                                {
                                                    "Standard File dialogs"
                                                },
                                                provider: "System Standard File Package",
                                                capacity: if state
                                                {
                                                    CapacityPolicy::HardBound { slots: 1, reason: "One Standard File backing state per domain" }
                                                }
                                                else
                                                {
                                                    CapacityPolicy::GrowableUntilMemory
                                                },
                                                source:
                                                    mac_object_table::registry::Source::Resident }
}
pub(crate) trait FileDialogHost
{
    fn file_dialogs(&mut self) -> &mut Manager;
}
pub(crate) struct ReleaseDialogs;
pub(crate) struct ReleaseStorage;
impl<C: FileDialogHost> mac_object_table::registry::Release<C> for ReleaseDialogs
{
    fn release(&self, context: &mut C, ticket: mac_object_table::domain::ReleasedResource)
               -> Result<(), mac_object_table::domain::CompletionError>
    {
        context.file_dialogs().slots.release(ticket).map(drop)
    }
}
impl<C: FileDialogHost> mac_object_table::registry::Release<C> for ReleaseStorage
{
    fn release(&self, context: &mut C, ticket: mac_object_table::domain::ReleasedResource)
               -> Result<(), mac_object_table::domain::CompletionError>
    {
        context.file_dialogs().slots.release_storage(ticket)
    }
}
impl Manager
{
    pub(crate) fn new((objects, storage): (KindLease, KindLease)) -> Result<Self, Status>
    {
        Ok(Self { slots:       Store::owned(objects, storage).map_err(|_| BAD_ARGUMENT)?,
                  publication: None })
    }
}

fn pool_status(error: mac_object_table::domain::PoolError) -> Status
{
    crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error))
}
fn inline(bytes: &[u8], length: u32) -> Result<&str, Status>
{
    core::str::from_utf8(bytes.get(..length as usize).ok_or(BAD_ARGUMENT)?).map_err(|_| {
                                                                               BAD_ARGUMENT
                                                                           })
}
impl Manager
{
    pub(crate) fn prepare_recovery(&mut self, storage: &OwnerRef) -> Result<(), Status>
    {
        if storage.owner().kind() != mac_object_table::domain::DomainKind::System
        {
            return Err(ACCESS_DENIED);
        }
        if self.publication.is_none()
        {
            self.publication =
                Some(self.slots.reserve_publication(storage).map_err(pool_status)?);
        }
        Ok(())
    }

    /// `recipient` is the native caller/service context, never a request field.
    /// Storage belongs to System; logical ownership remains the authenticated
    /// process for whom the one modal service was admitted.
    pub fn get(&self, recipient: u64, id: u64) -> Result<&Dialog, Status>
    {
        let dialog = self.slots.get(id).ok_or(NOT_FOUND)?;
        if dialog.owner != recipient
        {
            return Err(ACCESS_DENIED);
        }
        Ok(dialog)
    }

    pub fn get_mut(&mut self, recipient: u64, id: u64) -> Result<&mut Dialog, Status>
    {
        self.get(recipient, id)?;
        self.slots.get_mut(id).ok_or(NOT_FOUND)
    }

    /// Resident recovery uses the authenticated recipient capability to find
    /// even a completed protocol whose app stopped before disposing it.
    pub(crate) fn for_recipient(&self, recipient: &OwnerRef) -> Option<u64>
    {
        self.slots.iter().find_map(|(id, dialog)| {
                             (dialog.owner == recipient.owner().incarnation()).then_some(id)
                         })
    }

    pub fn active(&self, recipient: u64) -> Option<u64>
    {
        self.slots
            .iter()
            .find_map(|(id, dialog)| (dialog.owner == recipient && dialog.active()).then_some(id))
    }

    pub(crate) fn take(
        &mut self, owner: u64, id: u64)
        -> Result<mac_object_table::store::Removed<Dialog, crate::toolbox::handles::InvocationGate>,
                  Status>
    {
        self.get(owner, id)?;
        self.slots.remove(id).ok_or(NOT_FOUND)
    }

    pub(crate) fn dispose(&mut self, owner: u64, id: u64) -> Result<(), Status>
    {
        self.take(owner, id).map(drop)
    }

    pub(crate) fn request(&mut self, recipient: &OwnerRef, storage: &OwnerRef,
                          call: &mut StandardFileCall)
                          -> Result<(), Status>
    {
        let owner = recipient.owner().incarnation();
        if storage.owner().kind() != mac_object_table::domain::DomainKind::System
        {
            return Err(ACCESS_DENIED);
        }
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
            let prompt = Text::new(inline(&call.prompt, call.prompt_length)?)?;
            let name = inline(&call.name, call.name_length)?;
            let mut editor =
                Editor::new(Rect::new(15, 157, 4111, 173), Rect::new(15, 157, 227, 173))?;
            editor.set_text(name)?;
            editor.select(0, name.len() as u32)?;
            editor.activate(true);
            let mut types = [[0; 4]; MAX_TYPES];
            let type_count = call.type_count.max(0) as usize;
            types[..type_count].copy_from_slice(&call.types[..type_count]);
            let mut icons = Vec::new();
            icons.try_reserve_exact(page::ROWS).map_err(|_| NO_MEMORY)?;
            let mut ancestors = Vec::new();
            ancestors.try_reserve_exact(page::ANCESTORS).map_err(|_| NO_MEMORY)?;
            let mut entries = Page::default();
            entries.reset();
            let dialog =
                Dialog { saved_name: None,
                         owner,
                         window: WindowId(0),
                         auxiliary: WindowId(0),
                         auxiliary_sheet: Sheet::Main,
                         mode: call.mode,
                         location: (call.volume,
                                    if call.directory == 0 { ROOT_ID } else { call.directory }),
                         entries,
                         icons,
                         selected: None,
                         first: 0,
                         prompt,
                         editor,
                         focus_name: call.mode == PUT,
                         name_tracking: false,
                         state: PENDING,
                         result: CatalogInfo::default(),
                         replacing: false,
                         types,
                         type_count,
                         all: call.type_count == -1,
                         sheet: Sheet::Main,
                         error: OK,
                         last_click: None,
                         popup: false,
                         ancestors,
                         typeahead: Text::default(),
                         typed_at: 0,
                         name_format: None,
                         volume_info: VolumeInfo::default(),
                         stage: Some(if call.volume == 0
                                     {
                                         Stage::DefaultVolume
                                     }
                                     else
                                     {
                                         Stage::Volume
                                     }),
                         issued: false,
                         revision: 1,
                         desktop_count: 0,
                         create_when: 0,
                         main_widgets: Widgets::default(),
                         auxiliary_widgets: Widgets::default(),
                         #[cfg(target_arch = "riscv64")]
                         subject: None,
                         #[cfg(target_arch = "riscv64")]
                         recovery_activity: crate::context::recovery_activity() };
            let reservation = self.publication.as_mut().ok_or(NOT_SUPPORTED)?;
            let publication: Result<u64, Status> = {
                #[cfg(target_arch = "riscv64")]
                {
                    let activity = crate::context::recovery_activity().ok_or(ACCESS_DENIED)?;
                    if activity.recipient().owner() != recipient.owner()
                       || activity.storage_owner().owner() != storage.owner()
                       || !recipient.accepting()
                       || !crate::context::io_subject().is_some_and(|subject| {
                               subject.scope().same_scope(activity.root_subject().scope())
                           })
                    {
                        return Err(ACCESS_DENIED);
                    }
                    let subject = activity.root_subject();
                    self.slots.insert_reserved_in_scope_with_acknowledgement(
                        reservation, subject.scope(), Some(activity.acknowledgement_pin()),
                        |_| dialog).map(|publication| publication.id()).map_err(|error| {
                            match error
                            {
                                mac_object_table::store::PublicationError::Busy => SERVICE_BUSY,
                                mac_object_table::store::PublicationError::Pool(error) =>
                                    pool_status(error)
                            }
                        })
                }
                #[cfg(not(target_arch = "riscv64"))]
                {
                    let _ = (reservation, dialog);
                    Err(NOT_SUPPORTED)
                }
            };
            call.handle = publication?;
        }
        else
        {
            self.get(owner, call.handle)?;
            if call.operation == DISPOSE
            {
                return self.dispose(owner, call.handle);
            }
            if call.operation != POLL
            {
                return Err(NOT_SUPPORTED);
            }
        }
        let dialog = self.get(owner, call.handle)?;
        call.state = dialog.state;
        call.replacing = u32::from(dialog.replacing);
        call.entry = dialog.result;
        Ok(())
    }

    pub fn navigate_to(&mut self, owner: u64, id: u64, location: (u32, u32)) -> Result<(), Status>
    {
        self.get_mut(owner, id)?.restart(location)
    }

    pub fn action(&mut self, owner: u64, id: u64, action: Action, when: u32) -> Result<(), Status>
    {
        let d = self.get_mut(owner, id)?;
        if !d.active()
        {
            return Err(NOT_FOUND);
        }
        match action
        {
            Action::Cancel =>
            {
                if d.sheet == Sheet::Main
                {
                    d.state = CANCELLED;
                    d.revision = d.revision.checked_add(1).ok_or(NO_MEMORY)?;
                }
                else
                {
                    if let Some(name) = d.saved_name.take()
                    {
                        d.editor.set_text(name.text()?)?;
                        d.editor.select(0, d.name().len() as u32)?;
                    }
                    d.sheet = Sheet::Main;
                    d.restart(d.location)?;
                }
                return Ok(());
            }
            Action::Dismiss =>
            {
                d.sheet = Sheet::Main;
                return Ok(());
            }
            _ => ()
        }
        if d.sheet == Sheet::Error
        {
            if matches!(action, Action::Accept | Action::Confirm)
            {
                if let Some(name) = d.saved_name.take()
                {
                    d.editor.set_text(name.text()?)?;
                    d.editor.select(0, d.name().len() as u32)?;
                }
                d.sheet = Sheet::Main;
                d.restart(d.location)?;
            }
            return Ok(());
        }
        if d.sheet == Sheet::Replace
        {
            if matches!(action, Action::Accept | Action::Confirm)
            {
                d.replacing = true;
                d.state = ACCEPTED;
                d.stage = None;
            }
            return Ok(());
        }
        if d.sheet == Sheet::NewFolder && matches!(action, Action::Accept | Action::Confirm)
        {
            d.create_when = u64::from(when);
            return d.prepare_name(true);
        }
        match action
        {
            Action::Select(index) | Action::Open(index) =>
            {
                let entry = *d.entries.get(index).ok_or(SERVICE_BUSY)?;
                if d.selected == Some(index) && !matches!(action, Action::Open(_))
                {
                    return Ok(());
                }
                d.selected = Some(index);
                d.focus_name = false;
                d.reveal()?;
                if d.mode == PUT && !entry.is_directory()
                {
                    d.editor.set_text(entry.name())?;
                    d.editor.select(0, d.name().len() as u32)?;
                }
                if matches!(action, Action::Open(_))
                {
                    return self.action(owner, id, Action::Accept, when);
                }
            }
            Action::Accept | Action::Confirm =>
            {
                if !d.can_accept()
                {
                    return Ok(());
                }
                let entry = d.selected.and_then(|i| d.entries.get(i)).copied();
                if let Some(entry) = entry.filter(CatalogInfo::is_directory)
                {
                    return d.restart((entry.volume, entry.id));
                }
                if d.mode == GET
                {
                    let entry = entry.ok_or(SERVICE_BUSY)?;
                    if entry.finder_flags() & 0x8000 != 0
                    {
                        return Err(NOT_SUPPORTED);
                    }
                    d.result = entry;
                    d.state = ACCEPTED;
                    d.stage = None;
                }
                else
                {
                    if d.location.0 == 0
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    d.prepare_name(false)?;
                }
            }
            Action::Parent =>
            {
                let entry = d.ancestors.first().copied().ok_or(SERVICE_BUSY)?;
                return d.restart(if entry.id == ROOT_ID
                                 {
                                     (0, 0)
                                 }
                                 else
                                 {
                                     (entry.volume, entry.parent)
                                 });
            }
            Action::Desktop => return d.restart((0, 0)),
            Action::Volume(delta) =>
            {
                d.stage = Some(Stage::VolumeCount(delta));
                d.issued = false;
                d.revision = d.revision.checked_add(1).ok_or(NO_MEMORY)?;
            }
            Action::Previous(delta) | Action::Page(delta) =>
            {
                let total = d.entries.count();
                if total == 0
                {
                    return Ok(());
                }
                let step = i64::from(delta)
                           * if matches!(action, Action::Page(_)) { d.page() as i64 } else { 1 };
                d.selected = Some((d.selected.unwrap_or(0) as i64 + step).clamp(0, total as i64 - 1)
                                  as usize);
                d.focus_name = false;
                d.reveal()?;
            }
            Action::Home =>
            {
                d.selected = (d.entries.count() != 0).then_some(0);
                d.reveal()?;
            }
            Action::End =>
            {
                d.selected = d.entries.count().checked_sub(1);
                d.reveal()?;
            }
            Action::Scroll(first) =>
            {
                let first = first.min(d.entries.count().saturating_sub(d.page()));
                if first != d.first
                {
                    d.revision = d.revision.checked_add(1).ok_or(NO_MEMORY)?;
                    d.issued = false;
                    d.first = first;
                    d.entries.request(first, false);
                    d.stage = Some(if d.location.0 == 0
                                   {
                                       Stage::DesktopVolume(first as u32)
                                   }
                                   else
                                   {
                                       Stage::Scan
                                   });
                }
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
                    if d.volume_info.read_only != 0
                    {
                        return Err(READ_ONLY);
                    }
                    d.saved_name = Some(Text::new(d.name())?);
                    d.sheet = Sheet::NewFolder;
                    d.focus_name = true;
                    d.selected = None;
                    d.editor.set_text("")?;
                    d.editor.select(0, 0)?;
                }
            }
            Action::Edit(character) =>
            {
                if d.focus_name
                {
                    let info = d.editor.info();
                    let start = info.selection_start as usize;
                    let end = info.selection_end as usize;
                    let mut candidate = Text::new(&d.name()[..start])?;
                    candidate.push(character.encode_utf8(&mut [0; 4]))?;
                    candidate.push(&d.name()[end..])?;
                    if !candidate.text()?.contains('\0')
                    {
                        d.editor.key(character)?;
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
                    d.typeahead.push(character.encode_utf8(&mut [0; 4]))?;
                    d.revision = d.revision.checked_add(1).ok_or(NO_MEMORY)?;
                    d.issued = false;
                    d.entries.selected = d.selected;
                    d.entries.request(0, true);
                    d.stage = Some(Stage::Scan);
                    d.selected = None;
                    d.first = 0;
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
            Action::Navigate(movement, extend) =>
            {
                if d.focus_name
                {
                    d.editor.navigate(movement, extend)?;
                }
            }
            Action::Cancel | Action::Dismiss => ()
        }
        Ok(())
    }
}
