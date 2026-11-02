//! System-owned Standard File protocol. Catalog work is a bounded owned state
//! machine; no Files guard or caller buffer survives its asynchronous requests.
//!
//! The virtual Desktop presents mounted-volume roots followed by the Files
//! service's combined desktop iterator. Every row retains its real volume/CNID.
//! Unselected Save and New Folder retain the current volume's real Desktop
//! Folder, defaulting to startup only without a current volume. Selecting an
//! existing file retains its real volume/parent even while editing its name.
//! Desktop Folders are created only for writes; read-only failures are errors.
#![forbid(unsafe_code)]
pub mod layout;
pub(super) mod page;
pub(super) mod view;
#[cfg(target_arch = "riscv64")]
pub(crate) mod templates;
#[cfg(target_arch = "riscv64")]
mod preparation;
use alloc::vec::Vec;

use mac_abi::{files::{self, CatalogInfo, FileCall, ROOT_ID, VolumeInfo},
              geometry::Rect,
              standard_file::*,
              *};
use page::Page;
#[cfg(target_arch = "riscv64")]
pub(crate) use preparation::{BeginError, PreparedDialog};
#[cfg(target_arch = "riscv64")]
pub(crate) use templates::prepare as prepare_templates;
#[cfg(target_arch = "riscv64")]
pub(crate) use templates::{Vectors, return_vectors};
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
    Toolbox,
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
    NavigationDesktop,
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
    },
    DesktopScan,
    DesktopDestination,
    SaveDesktopVolume
    {
        folder:    bool,
        volume:    u32,
        directory: u32
    },
    SaveDesktopFolder(bool),
    SaveVolume
    {
        volume:    u32,
        directory: u32
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
    pub(super) error_context:     Text,
    pub last_click:               Option<(usize, u32)>,
    pub popup:                    bool,
    pub ancestors:                Vec<CatalogInfo>,
    typeahead:                    Text,
    pub typed_at:                 u32,
    name_format:                  Option<mac_filesystem::Format>,
    pub(super) volume_info:       VolumeInfo,
    stage:                        Option<Stage>,
    desktop_resume:               Option<Stage>,
    issued:                       bool,
    pub(super) revision:          u64,
    desktop_count:                u32,
    pub(super) desktop_volume:    u32,
    save_destination:             Option<(u32, u32)>,
    desktop_folder:               Option<u32>,
    desktop_cursor:               files::DesktopScanCursor,
    create_when:                  u64,
    pub(super) main_widgets:      Widgets,
    pub(super) auxiliary_widgets: Widgets,
    #[cfg(target_arch = "riscv64")]
    pub(super) subject:           Option<crate::drivers::jobs::subjects::Subject>,
    #[cfg(target_arch = "riscv64")]
    pub(super) recovery_activity: Option<crate::recovery::Activity>,
    #[cfg(target_arch = "riscv64")]
    vector_lease:                 Option<crate::recovery::StandardFileVectorsLease>
}
#[cfg(target_arch = "riscv64")]
impl Drop for Dialog
{
    fn drop(&mut self)
    {
        if let Some(lease) = self.vector_lease.take()
        {
            // Move only backing into the original Session. Clearing artwork
            // waits for its real quiescent return outside Desktop and gates.
            lease.park(Vectors { icons:     core::mem::take(&mut self.icons),
                                 ancestors: core::mem::take(&mut self.ancestors) });
        }
    }
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
            || (!self.name().is_empty()
                && self.stage.is_none_or(|s| {
                                 matches!(s,
                                          Stage::Scan
                                          | Stage::DesktopCount
                                          | Stage::DesktopVolume(_)
                                          | Stage::DesktopRoot { .. }
                                          | Stage::DesktopScan
                                          | Stage::DesktopDestination)
                             }))
        }
    }

    fn restart(&mut self, location: (u32, u32)) -> Result<(), Status>
    {
        self.revision = self.revision.checked_add(1).ok_or(NO_MEMORY)?;
        if location.0 == 0 && self.location.0 != 0
        {
            self.desktop_volume = self.location.0;
        }
        let desktop_info = self.volume_info;
        self.location = location;
        self.name_format = None;
        self.volume_info = if location.0 == 0
                              && (desktop_info.id == self.desktop_volume
                                  || self.desktop_volume == 0 && desktop_info.startup != 0)
        {
            desktop_info
        }
        else
        {
            VolumeInfo::default()
        };
        self.save_destination = None;
        self.desktop_resume = None;
        self.desktop_folder = None;
        self.desktop_cursor = files::DesktopScanCursor::default();
        self.entries.reset();
        self.icons.clear();
        self.ancestors.clear();
        self.selected = None;
        self.first = 0;
        self.last_click = None;
        self.name_tracking = false;
        self.popup = false;
        self.typeahead.clear();
        self.error_context.clear();
        self.issued = false;
        if location.0 == 0
        {
            self.start_scan();
        }
        else
        {
            self.stage = Some(Stage::Volume);
        }
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
                self.start_scan();
            }
        }
        Ok(())
    }

    fn start_scan(&mut self)
    {
        self.desktop_cursor = files::DesktopScanCursor::default();
        self.desktop_resume = None;
        self.stage = Some(if self.location.0 == 0
                          {
                              if self.mode == PUT
                                 && self.desktop_volume != 0
                                 && self.volume_info.id != self.desktop_volume
                              {
                                  self.desktop_resume = Some(Stage::DesktopCount);
                                  Stage::DesktopDestination
                              }
                              else
                              {
                                  Stage::DesktopCount
                              }
                          }
                          else
                          {
                              Stage::Scan
                          });
    }

    fn include(&self, entry: &CatalogInfo) -> bool
    {
        entry.id == ROOT_ID && entry.is_directory()
        || self.mode != GET
        || self.all
        || (entry.finder_flags() & 0x4000 == 0
            && (entry.is_directory() || self.types[..self.type_count].contains(&entry.file_type)))
    }

    fn retain_save_destination(&mut self) -> Result<(), Status>
    {
        let Some(entry) =
            self.selected
                .and_then(|index| self.entries.get(index))
                .copied()
                .filter(|entry| self.mode == PUT && self.location.0 == 0 && !entry.is_directory())
        else
        {
            return Ok(());
        };
        self.save_destination = Some((entry.volume, entry.parent));
        if self.desktop_volume != entry.volume
        {
            self.revision = self.revision.checked_add(1).ok_or(NO_MEMORY)?;
            self.issued = false;
            if !matches!(self.stage, Some(Stage::DesktopDestination))
            {
                self.desktop_resume = self.stage;
            }
            self.desktop_volume = entry.volume;
            self.volume_info = VolumeInfo::default();
            self.name_format = None;
            self.stage = Some(Stage::DesktopDestination);
        }
        Ok(())
    }

    fn accept_scan(&mut self, call: files::FileScanCall) -> Result<bool, Status>
    {
        let include = self.include(&call.entry);
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
        else if self.location.0 == 0 && self.entries.rescan
        {
            self.start_scan();
        }
        self.retain_save_destination()?;
        Ok(changed && self.entries.ready(self.page()))
    }

    pub(super) fn fail(&mut self, cause: Status)
    {
        if let Some(revision) = self.revision.checked_add(1)
        {
            self.revision = revision;
        }
        else
        {
            self.state = CANCELLED;
        }
        self.issued = false;
        self.stage = None;
        self.desktop_resume = None;
        self.error = cause;
        self.sheet = Sheet::Error;
    }

    pub(super) fn wants_icon(&self, call: &files::FileScanCall) -> bool
    {
        if call.step != files::SCAN_ENTRY
        {
            return false;
        }
        let include = self.include(&call.entry);
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
        if self.sheet != Sheet::Main
           && !(self.sheet == Sheet::NewFolder
                && matches!(stage,
                            Stage::PrepareFolder
                            | Stage::CreateFolder
                            | Stage::SaveDesktopVolume { folder: true,
                                                         .. }
                            | Stage::SaveDesktopFolder(true)))
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
            Stage::DesktopDestination =>
            {
                call.operation = files::VOLUME_INFO;
                call.volume = self.desktop_volume;
                call.cnid = 0;
            }
            Stage::Directory => call.operation = files::CATALOG,
            Stage::Ancestor(id) =>
            {
                call.operation = files::CATALOG;
                call.cnid = id;
            }
            Stage::NavigationDesktop =>
            {
                call.operation = files::FIND_FOLDER;
                call.cnid = 0;
                call.file_type = *b"desk";
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
            Stage::DesktopScan =>
            {
                if !self.entries.scanning
                {
                    self.stage = None;
                    return Ok(None);
                }
                self.error_context = Text::new("Desktop")?;
                self.issued = true;
                return Ok(Some((self.revision,
                                Query::Scan(files::FileScanCall { operation:
                                                                      files::SCAN_DESKTOP,
                                                                  desktop:
                                                                      self.desktop_cursor,
                                                                  ..Default::default() }))));
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
            Stage::SaveDesktopVolume { volume,
                                       .. } =>
            {
                call.operation = files::VOLUME_INFO;
                call.volume = volume;
            }
            Stage::SaveDesktopFolder(_) =>
            {
                call.operation = files::FIND_FOLDER;
                call.volume = self.desktop_volume;
                call.cnid = 0;
                call.file_type = *b"desk";
                call.index = 1;
                call.position = self.create_when;
            }
            Stage::SaveVolume { volume,
                                .. } =>
            {
                call.operation = files::VOLUME_INFO;
                call.volume = volume;
            }
        }
        self.issued = true;
        Ok(Some((self.revision, Query::File(call, text))))
    }

    fn prepare_name(&mut self, folder: bool) -> Result<(), Status>
    {
        self.revision = self.revision.checked_add(1).ok_or(NO_MEMORY)?;
        self.issued = false;
        self.desktop_resume = None;
        if self.location.0 == 0
        {
            self.error_context = if self.desktop_volume == 0
            {
                Text::new("Desktop: startup disk")?
            }
            else
            {
                let mut context = Text::new("Desktop: ")?;
                context.push_bounded(self.volume_info.name());
                context
            };
            self.stage = Some(Stage::SaveDesktopFolder(folder));
            return Ok(());
        }
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

    fn set_volume(&mut self, volume: VolumeInfo)
    {
        self.volume_info = volume;
        self.name_format = match volume.filesystem
        {
            files::FILESYSTEM_HFS => Some(mac_filesystem::Format::Hfs),
            files::FILESYSTEM_HFS_B => Some(mac_filesystem::Format::HfsB),
            _ => None
        };
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
            Err(SCAN_STALE) if matches!(stage, Stage::Scan | Stage::DesktopScan) =>
            {
                self.entries.reset();
                self.selected = None;
                self.first = 0;
                self.start_scan();
                return Ok(true);
            }
            Err(NOT_FOUND) if matches!(stage, Stage::NavigationDesktop) =>
            {
                self.desktop_folder = None;
                self.stage = Some(Stage::Directory);
                return Ok(false);
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
                self.set_volume(call.volume_info);
                self.stage = Some(Stage::NavigationDesktop);
            }
            (Stage::DesktopDestination, Reply::File(call)) =>
            {
                if call.volume_info.id != self.desktop_volume
                {
                    return Err(CORRUPT_DATA);
                }
                self.set_volume(call.volume_info);
                self.stage = self.desktop_resume.take();
            }
            (Stage::NavigationDesktop, Reply::File(call)) =>
            {
                if !call.entry.is_directory() || call.entry.volume != self.location.0
                {
                    return Err(CORRUPT_DATA);
                }
                self.desktop_folder = Some(call.entry.id);
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
                                     || Some(call.entry.id) == self.desktop_folder
                                     || Some(call.entry.parent) == self.desktop_folder
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
                                     || Some(call.entry.id) == self.desktop_folder
                                     || Some(call.entry.parent) == self.desktop_folder
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
                return self.accept_scan(call);
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
                if call.index == 0
                {
                    return self.accept_scan(files::FileScanCall { step: files::SCAN_DONE,
                                                                  ..Default::default() });
                }
                self.stage = Some(Stage::DesktopVolume(0));
            }
            (Stage::DesktopVolume(index), Reply::File(call)) =>
            {
                if self.desktop_volume == 0 && call.volume_info.startup != 0
                {
                    self.set_volume(call.volume_info);
                }
                self.error_context = Text::new("Desktop: ")?;
                self.error_context.push_bounded(call.volume_info.name());
                self.stage = Some(Stage::DesktopRoot { index,
                                                       volume: call.volume_info.id });
            }
            (Stage::DesktopRoot { index,
                                  volume },
             Reply::File(call)) =>
            {
                if call.entry.volume != volume
                   || call.entry.id != ROOT_ID
                   || !call.entry.is_directory()
                {
                    return Err(CORRUPT_DATA);
                }
                let changed = self.accept_scan(files::FileScanCall { step: files::SCAN_ENTRY,
                                                                     entry: call.entry,
                                                                     ..Default::default() })?;
                if self.entries.scanning && !matches!(self.stage, Some(Stage::DesktopCount))
                {
                    self.stage = Some(if index + 1 < self.desktop_count
                                      {
                                          Stage::DesktopVolume(index + 1)
                                      }
                                      else
                                      {
                                          Stage::DesktopScan
                                      });
                }
                return Ok(changed);
            }
            (Stage::DesktopScan, Reply::Scan(mut call)) =>
            {
                self.desktop_cursor = call.desktop;
                // Volume icons precede the shared desktop file projection.
                // Catalog identity remains the service's real volume/CNID.
                call.cursor = files::DirectoryScanCursor::default();
                return self.accept_scan(call);
            }
            (Stage::SaveDesktopVolume { folder,
                                        volume,
                                        directory },
             Reply::File(call)) =>
            {
                self.error_context = Text::new("Desktop: ")?;
                self.error_context.push_bounded(call.volume_info.name());
                if call.volume_info.id != volume
                {
                    return Err(CORRUPT_DATA);
                }
                if call.volume_info.read_only != 0
                {
                    return Err(READ_ONLY);
                }
                self.restart((volume, directory))?;
                self.set_volume(call.volume_info);
                self.desktop_folder = Some(directory);
                self.error_context = Text::new("Desktop: ")?;
                self.error_context.push_bounded(call.volume_info.name());
                self.prepare_name(folder)?;
            }
            (Stage::SaveDesktopFolder(folder), Reply::File(call)) =>
            {
                if !call.entry.is_directory() || call.entry.volume == 0
                {
                    return Err(CORRUPT_DATA);
                }
                self.stage = Some(Stage::SaveDesktopVolume { folder,
                                                             volume: call.entry.volume,
                                                             directory: call.entry.id });
            }
            (Stage::SaveVolume { volume,
                                 directory },
             Reply::File(call)) =>
            {
                if call.volume_info.id != volume
                {
                    return Err(CORRUPT_DATA);
                }
                self.restart((volume, directory))?;
                self.set_volume(call.volume_info);
                self.error_context = Text::new("Desktop: ")?;
                self.error_context.push_bounded(call.volume_info.name());
                self.prepare_name(false)?;
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
        // BEGIN has one detached, authenticated publication path. Never rebuild
        // its payload while a manager/Desktop guard is held.
        if call.operation == BEGIN
        {
            return Err(NOT_SUPPORTED);
        }
        self.get(owner, call.handle)?;
        if call.operation == DISPOSE
        {
            return self.dispose(owner, call.handle);
        }
        if call.operation != POLL
        {
            return Err(NOT_SUPPORTED);
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
                    d.retain_save_destination()?;
                }
                else if d.mode == PUT
                {
                    d.save_destination = None;
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
                    d.create_when = u64::from(when);
                    if let Some((volume, directory)) =
                        (d.location.0 == 0).then(|| {
                                               entry.map(|entry| (entry.volume, entry.parent))
                                                    .or(d.save_destination)
                                           })
                                           .flatten()
                    {
                        d.revision = d.revision.checked_add(1).ok_or(NO_MEMORY)?;
                        d.issued = false;
                        d.desktop_resume = None;
                        d.stage = Some(Stage::SaveVolume { volume,
                                                           directory });
                        return Ok(());
                    }
                    d.prepare_name(false)?;
                }
            }
            Action::Parent =>
            {
                if d.location.0 == 0
                {
                    return Ok(());
                }
                let entry = d.ancestors.first().copied().ok_or(SERVICE_BUSY)?;
                return d.restart(if entry.id == ROOT_ID
                                    || Some(entry.id) == d.desktop_folder
                                    || Some(entry.parent) == d.desktop_folder
                                 {
                                     (0, 0)
                                 }
                                 else
                                 {
                                     (entry.volume, entry.parent)
                                 });
            }
            Action::Toolbox => return d.restart((0, 0)),
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
                d.save_destination = None;
                d.focus_name = false;
                d.reveal()?;
            }
            Action::Home =>
            {
                d.selected = (d.entries.count() != 0).then_some(0);
                d.save_destination = None;
                d.reveal()?;
            }
            Action::End =>
            {
                d.selected = d.entries.count().checked_sub(1);
                d.save_destination = None;
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
                    d.start_scan();
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
                if d.mode == PUT
                {
                    if d.location.0 != 0 && d.volume_info.read_only != 0
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
                    d.start_scan();
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
        d.retain_save_destination()?;
        Ok(())
    }
}
