//! Resident panic reports. Native capture uses one boot-admitted System buffer;
//! genuine saved managers retain immutable full diagnostic snapshots independently
//! of the visible page and of a dying application account.
#![forbid(unsafe_code)]
use alloc::vec::Vec;
mod storage;
#[cfg(systemrisc)]
mod detail;
#[cfg(systemrisc)]
pub(crate) use detail::FailureDetail;
#[cfg(systemrisc)]
mod pending;
use mac_abi::{crashes::*, geometry::Rect, *};
#[cfg(systemrisc)]
pub(crate) use pending::owner_stopped as pending_owner_stopped;
#[cfg(systemrisc)]
pub(crate) use pending::{cleanup_progress, modal_available, pump_dismissals, pump_reports,
                         take_for_termination};
use storage::Payload;
pub(super) use storage::{PaintError, PaintScratch};

pub const RESOURCE_ID: i16 = -16400;
pub const DEFAULT_COLUMNS: usize = 90;
pub const PAGE_LINES: u32 = 23;

/// Prepared once from the fixed resident report geometry and its System font.
#[cfg(systemrisc)]
#[derive(Clone, Copy)]
pub(crate) struct ReportLayout
{
    pub(crate) columns:    usize,
    pub(crate) page_lines: u32
}
#[cfg(systemrisc)]
struct PreparedReport
{
    layout: ReportLayout,
    icon:   super::quickdraw::icons_service::Drawing
}
#[cfg(systemrisc)]
static PREPARED: spin::Once<PreparedReport> = spin::Once::new();
#[cfg(systemrisc)]
pub(crate) fn prepared_layout() -> Result<ReportLayout, Status>
{
    PREPARED.get().map(|prepared| prepared.layout).ok_or(NOT_SUPPORTED)
}
#[cfg(systemrisc)]
pub(super) fn prepared_icon() -> Result<super::quickdraw::icons_service::Drawing, Status>
{
    // The immutable family and its original protected receipt are reset-pinned.
    // This template's default Port has no owned clip to duplicate; the normal
    // Desktop icon path still captures the actual window Port when recording.
    PREPARED.get().map(|prepared| prepared.icon.clone()).ok_or(NOT_SUPPORTED)
}

#[derive(Clone, Copy)]
enum Capture
{
    None,
    Streaming,
    Submitted,
    Prefix,
    SystemDiagnostic
}

/// Trusted capture identity, retained independently of caller-supplied flags.
#[derive(Clone, Copy)]
enum NativeFailure
{
    AccessFault,
    Exception,
    DefinitionPanic
}

pub struct Manager
{
    capture:                         Capture,
    dismissal:                       Option<storage::Origin>,
    superseded:                      bool,
    payload:                         Payload,
    archive_origin:                  Option<storage::Origin>,
    columns:                         usize,
    column:                          usize,
    previous_cr:                     bool,
    native_fault:                    Option<NativeFailure>,
    info:                            CrashInfo,
    #[cfg(systemrisc)]
    pub(super) controls:             [Option<u64>; 2],
    #[cfg(systemrisc)]
    pub(super) control_enabled:      [bool; 2],
    #[cfg(systemrisc)]
    pub(super) control_labels:       [u32; 2],
    #[cfg(systemrisc)]
    pub(super) tracking:             Option<usize>,
    #[cfg(systemrisc)]
    pub(super) copy_failed:          bool,
    #[cfg(systemrisc)]
    pub(super) restart_pending:      bool,
    #[cfg(systemrisc)]
    pub(super) restart_error:        Option<Status>,
    #[cfg(systemrisc)]
    pub(super) restart_terminal:     bool,
    #[cfg(systemrisc)]
    pub(super) power_failure:        Option<(Status, u32)>,
    #[cfg(systemrisc)]
    pub(super) scrollbox:            Option<u64>,
    #[cfg(systemrisc)]
    pub(super) scroll_tracking:      bool,
    #[cfg(systemrisc)]
    pub(super) restart_detail:       Option<FailureDetail>,
    #[cfg(systemrisc)]
    pub(super) restart_detail_dirty: bool
}

/// Diagnostic data is independent of the dying application's window/control
/// objects. A recovery controller can preserve it before stopping that owner.
#[cfg(systemrisc)]
pub(crate) struct Snapshot
{
    capture:      Capture,
    superseded:   bool,
    payload:      Payload,
    columns:      usize,
    column:       usize,
    previous_cr:  bool,
    native_fault: Option<NativeFailure>,
    info:         CrashInfo
}
#[cfg(systemrisc)]
impl Snapshot
{
    pub(crate) fn retain(&self) -> Result<Self, Status>
    {
        Ok(Self { capture:      self.capture,
                  superseded:   self.superseded,
                  payload:      self.payload.snapshot()?,
                  columns:      self.columns,
                  column:       self.column,
                  previous_cr:  self.previous_cr,
                  native_fault: self.native_fault,
                  info:         self.info })
    }

    pub(crate) fn add_flags(&mut self, flags: u32)
    {
        self.info.flags |= flags;
    }

    pub(crate) fn info(&self) -> CrashInfo
    {
        self.info
    }
}
#[cfg(systemrisc)]
pub(crate) fn queue_report(binding: crate::toolbox::processes::ResourceBinding,
                           snapshot: Snapshot, flags: u32)
                           -> Result<CrashInfo, Status>
{
    pending::queue(binding, snapshot, flags)
}

#[cfg(systemrisc)]
pub(crate) fn queue_dismissal(binding: crate::toolbox::processes::ResourceBinding,
                              activity: crate::recovery::Activity, snapshot: Snapshot,
                              window: WindowId)
                              -> Result<(), Status>
{
    pending::queue_dismissal(binding, activity, snapshot, window)
}

impl Manager
{
    pub(in crate::toolbox) fn title_label(&self) -> u32
    {
        match self.native_fault
        {
            Some(NativeFailure::AccessFault) => 28,
            Some(NativeFailure::Exception) => 30,
            Some(NativeFailure::DefinitionPanic) => 0,
            None if self.info.flags & MAIN_ERROR != 0 => 26,
            None => 0
        }
    }

    pub(in crate::toolbox) fn description_label(&self, finder: bool) -> u32
    {
        if finder
        {
            return 4;
        }
        match self.native_fault
        {
            Some(NativeFailure::AccessFault) => 29,
            Some(NativeFailure::Exception) => 31,
            Some(NativeFailure::DefinitionPanic) => 7,
            None if self.info.flags & MAIN_ERROR != 0 => 27,
            None => 7
        }
    }
}

impl Default for Manager
{
    fn default() -> Self
    {
        Self { capture:                                              Capture::None,
               dismissal:                                            None,
               superseded:                                           false,
               payload:                                              Payload::default(),
               archive_origin:                                       None,
               columns:                                              DEFAULT_COLUMNS,
               column:                                               0,
               previous_cr:                                          false,
               native_fault:                                         None,
               info:
                   CrashInfo { page_lines: PAGE_LINES,
                               ..Default::default() },
               #[cfg(systemrisc)]
               controls:                                             [None; 2],
               #[cfg(systemrisc)]
               control_enabled:                                      [false; 2],
               #[cfg(systemrisc)]
               control_labels:                                       [u32::MAX; 2],
               #[cfg(systemrisc)]
               tracking:                                             None,
               #[cfg(systemrisc)]
               scrollbox:                                            None,
               #[cfg(systemrisc)]
               scroll_tracking:                                      false,
               #[cfg(systemrisc)]
               copy_failed:                                          false,
               #[cfg(systemrisc)]
               restart_pending:                                      false,
               #[cfg(systemrisc)]
               restart_error:                                        None,
               #[cfg(systemrisc)]
               restart_terminal:                                     false,
               #[cfg(systemrisc)]
               power_failure:                                        None,
               #[cfg(systemrisc)]
               restart_detail:                                       None,
               #[cfg(systemrisc)]
               restart_detail_dirty:                                 false }
    }
}
#[cfg(systemrisc)]
pub(crate) fn initialize_protected_report(permit: crate::allocator::recovery::Permit)
                                          -> Result<(), Status>
{
    crate::context::with_recovery_allocation(permit, || {
        crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
            PREPARED.try_call_once(|| {
                        let layout =
                            crate::toolbox::runtime::Toolbox::measure_crash_report_layout()?;
                        let paint_bytes =
                            crate::toolbox::runtime::Toolbox::measure_crash_paint_capacity(layout)?;
                        let call = quickdraw::IconCall { scope: resources::SYSTEM,
                                                         id: RESOURCE_ID,
                                                         destination: Rect::new(18, 14, 50, 46),
                                                         ..Default::default() };
                        let icon = crate::kernel::with_resources(|resources| {
                                       super::quickdraw::icons_service::Drawing::new(
                        resources, &call, super::quickdraw::service::Port::default())
                                   }).ok_or(NOT_SUPPORTED)??;
                        storage::prepare(paint_bytes)?;
                        Ok::<_, Status>(PreparedReport { layout,
                                                         icon })
                    })?;
            Ok(())
        })
    })
}

#[cfg(systemrisc)]
pub(crate) fn protected_stock_available() -> bool
{
    storage::available()
}

impl Manager
{
    /// Preserve the genuine capture/archive identity after its System-owned
    /// presentation has acknowledged dismissal. This retains no protected
    /// snapshot, so the single capture stock can be reused by a later BEGIN.
    #[cfg(systemrisc)]
    pub(crate) fn dismissed_metadata(&self) -> Self
    {
        let mut info = self.info;
        info.window = WindowId(0);
        info.dismissed = 1;
        Self { capture: self.capture,
               dismissal: if self.superseded
               {
                   None
               }
               else
               {
                   self.archive_origin.or(self.payload.origin())
               },
               superseded: self.superseded,
               archive_origin: self.archive_origin.or(self.payload.origin()),
               columns: self.columns,
               column: self.column,
               previous_cr: self.previous_cr,
               native_fault: self.native_fault,
               info,
               ..Self::default() }
    }

    #[cfg(systemrisc)]
    pub(crate) fn capture_attempt(&mut self)
    {
        self.dismissal = None;
        self.superseded = true;
    }

    #[cfg(systemrisc)]
    pub(crate) fn capture_superseded(&self) -> bool
    {
        self.superseded
    }

    #[cfg(systemrisc)]
    pub(crate) fn acknowledged_capture(&self, owner: u64) -> bool
    {
        !self.superseded
        && self.info.dismissed != 0
        && self.info.window.0 == 0
        && self.dismissal.is_some_and(|receipt| receipt.app() == owner)
    }

    #[cfg(systemrisc)]
    pub(crate) fn main_error_capture(&self, owner: u64, thread: u64) -> bool
    {
        !self.superseded
        && matches!(self.capture, Capture::Submitted)
        && self.info.app == owner
        && self.info.thread == thread
        && self.info.flags & MAIN_ERROR != 0
    }

    #[cfg(systemrisc)]
    pub(crate) fn owns_protected_stock(&self) -> bool
    {
        self.payload.protected()
    }

    pub(crate) fn submit(&mut self, flags: u32)
    {
        if matches!(self.capture, Capture::Streaming)
        {
            self.capture = Capture::Submitted;
        }
        self.info.flags |= flags;
    }

    #[cfg(systemrisc)]
    pub(crate) fn snapshot(&self) -> Result<Snapshot, Status>
    {
        let payload = if let Some(origin) = self.archive_origin
        {
            Payload::from_archive(origin).ok_or(NOT_FOUND)?
        }
        else
        {
            self.payload.snapshot()?
        };
        Ok(Snapshot { capture: self.capture,
                      superseded: self.superseded,
                      payload,
                      columns: self.columns,
                      column: self.column,
                      previous_cr: self.previous_cr,
                      native_fault: self.native_fault,
                      info: self.info() })
    }

    #[cfg(systemrisc)]
    pub(crate) fn from_snapshot(snapshot: Snapshot) -> Self
    {
        let mut info = snapshot.info;
        info.window = WindowId(0);
        info.dismissed = 0;
        Self { dismissal: None,
               superseded: snapshot.superseded,
               capture: snapshot.capture,
               payload: snapshot.payload,
               columns: snapshot.columns,
               column: snapshot.column,
               previous_cr: snapshot.previous_cr,
               native_fault: snapshot.native_fault,
               info,
               ..Default::default() }
    }

    #[cfg(systemrisc)]
    pub(crate) fn begin_protected(&mut self, app: u64, thread: u64, flags: u32)
                                  -> Result<(), Status>
    {
        let previous = self.info;
        let previous_archive = self.archive_origin;
        let authorized = previous_archive.or(self.payload.origin());
        self.payload.protected_begin()?;
        // This new origin is published only by accepted BEGIN. A refusal must
        // not turn newly borrowed archive bytes into another manager's report.
        self.info.app = app;
        self.info.thread = thread;
        let result = self.request(&mut CrashCall { operation: BEGIN,
                                                   flags,
                                                   ..Default::default() },
                                  "",
                                  &mut []);
        if let Err(status) = result
        {
            self.info = previous;
            self.archive_origin = previous_archive;
            self.payload.archive();
            if previous_archive.is_some() || self.payload.origin() != authorized
            {
                self.payload = Payload::default();
            }
            return Err(status);
        }
        Ok(())
    }

    #[cfg(systemrisc)]
    pub(crate) fn archive(&self)
    {
        self.payload.archive();
    }

    /// End this saved manager's use after actual UI disposal. The System archive
    /// retains the complete latest bytes, while the one boot buffer can be
    /// claimed for a later report once every genuine snapshot is gone.
    #[cfg(systemrisc)]
    pub(crate) fn release_payload(&mut self)
    {
        if let Some(origin) = self.payload.origin()
        {
            self.archive_origin = Some(origin);
        }
        self.payload.archive();
        self.payload = Payload::default();
    }

    #[cfg(systemrisc)]
    pub(crate) fn finish_dismissal(&mut self, snapshot: &Snapshot) -> Result<bool, Status>
    {
        let expected = snapshot.payload.origin().ok_or(BAD_ABI)?;
        if self.archive_origin.or(self.payload.origin()) != Some(expected)
        {
            return Ok(false);
        }
        let changed = self.info.dismissed == 0;
        self.set_window(WindowId(0));
        self.request(&mut CrashCall { operation: DISMISS,
                                      ..Default::default() },
                     "",
                     &mut [])?;
        self.release_payload();
        Ok(changed)
    }

    #[cfg(systemrisc)]
    pub(crate) fn finder_recovered(&mut self)
    {
        self.restart_error = None;
        self.restart_pending = false;
        self.restart_terminal = false;
        self.power_failure = None;
        self.restart_detail = None;
        self.restart_detail_dirty = true;
    }

    #[cfg(systemrisc)]
    pub(crate) fn add_flags(&mut self, flags: u32)
    {
        self.info.flags |= flags;
    }

    #[cfg(systemrisc)]
    pub(crate) fn seal_failure_capture(&mut self)
                                       -> crate::toolbox::processes::termination::Retention
    {
        use crate::toolbox::processes::termination::Retention;
        let retained = match self.capture
        {
            Capture::None | Capture::Streaming =>
            {
                self.capture = Capture::Prefix;
                self.info.flags |= TRUNCATED;
                Retention::CapturedPrefix
            }
            Capture::Submitted =>
            {
                if self.info.flags & TRUNCATED != 0
                {
                    Retention::CapturedPrefix
                }
                else
                {
                    Retention::CapturedComplete
                }
            }
            Capture::Prefix => Retention::CapturedPrefix,
            Capture::SystemDiagnostic => Retention::SystemDiagnostic
        };
        if self.superseded
        {
            match retained
            {
                Retention::CapturedComplete => Retention::PreviousComplete,
                Retention::CapturedPrefix => Retention::PreviousPrefix,
                Retention::SystemDiagnostic => Retention::PreviousSystemDiagnostic,
                other => other
            }
        }
        else
        {
            retained
        }
    }

    #[cfg(systemrisc)]
    pub(crate) fn seal_system_diagnostic(&mut self)
    {
        self.capture = Capture::SystemDiagnostic;
    }

    /// Preserve trusted fault scalars even if the author filled its diagnostic
    /// allowance first. A bounded tail replaces only the necessary app suffix;
    /// TRUNCATED then explicitly prevents a complete-report claim.
    #[cfg(systemrisc)]
    pub(crate) fn append_native_fault(&mut self, fault: crate::machine::scheduler::FaultSnapshot,
                                      application_label: &str, application_name: &str)
                                      -> Result<(), Status>
    {
        if self.native_fault.is_some()
        {
            return Ok(());
        }
        // One bounded line per captured scalar/register, with ample space for
        // names, 64-bit hexadecimal values and the fixed explanatory header.
        const BYTES: usize = (30 + 12) * 64
                             + 2 * mac_abi::INLINE_STRING_BYTES
                             + core::mem::size_of::<mac_abi::definitions::DefinitionPanic>();
        struct Text
        {
            bytes: [u8; BYTES],
            used:  usize
        }
        impl core::fmt::Write for Text
        {
            fn write_str(&mut self, text: &str) -> core::fmt::Result
            {
                let end = self.used.checked_add(text.len()).ok_or(core::fmt::Error)?;
                self.bytes
                    .get_mut(self.used..end)
                    .ok_or(core::fmt::Error)?
                    .copy_from_slice(text.as_bytes());
                self.used = end;
                Ok(())
            }
        }
        let mut text = Text { bytes: [0; BYTES],
                              used:  0 };
        core::fmt::write(&mut text, format_args!("\n{application_label} {application_name}\n{fault}")).map_err(|_| BAD_ARGUMENT)?;
        let text = core::str::from_utf8(&text.bytes[..text.used]).map_err(|_| BAD_ARGUMENT)?;
        let data = self.payload.data_mut()?;
        let previous = data.report.len();
        let mut keep =
            previous.min(MAX_REPORT_BYTES.checked_sub(text.len()).ok_or(BAD_ARGUMENT)?);
        while !data.report.is_char_boundary(keep)
        {
            keep -= 1;
        }
        let starts = data.starts.partition_point(|offset| *offset as usize <= keep);
        let required = starts.checked_add(text.len()).ok_or(BAD_ARGUMENT)?;
        if data.report.capacity() < keep + text.len() || data.starts.capacity() < required
        {
            return Err(NO_MEMORY);
        }
        // Every fallible admission precedes mutation. The existing protected
        // String and row-directory capacities cover this entire bounded tail.
        data.report.truncate(keep);
        data.starts.truncate(starts);
        self.column = 0;
        self.previous_cr = data.report.ends_with('\r');
        data.report.push_str(text);
        for (offset, character) in text.char_indices()
        {
            layout(&mut data.starts,
                   &mut self.column,
                   &mut self.previous_cr,
                   self.columns,
                   keep + offset,
                   character);
        }
        if keep != previous
        {
            self.info.flags |= TRUNCATED;
        }
        self.native_fault = Some(if fault.is_definition_panic()
                                 {
                                     NativeFailure::DefinitionPanic
                                 }
                                 else if fault.is_access_fault()
                                 {
                                     NativeFailure::AccessFault
                                 }
                                 else
                                 {
                                     NativeFailure::Exception
                                 });
        Ok(())
    }

    /// The report owns both ordinary buttons and its managed row scrollbar.
    /// Its real capture continues outside the window until release/cancel.
    #[cfg(systemrisc)]
    pub(crate) fn tracks_input(&self) -> bool
    {
        self.tracking.is_some() || self.scroll_tracking
    }

    pub fn set_window(&mut self, window: WindowId)
    {
        self.info.window = window;
        #[cfg(systemrisc)]
        {
            self.controls = [None; 2];
            self.control_enabled = [false; 2];
            self.control_labels = [u32::MAX; 2];
            self.tracking = None;
            self.scrollbox = None;
            self.scroll_tracking = false;
        }
    }

    pub fn info(&self) -> CrashInfo
    {
        let archived = self.archive_origin.and_then(storage::archived);
        let (report_bytes, line_count) = if self.archive_origin.is_some()
        {
            archived.as_ref().map_or((0, 0), |view| {
                                 (view.data().report.len(), view.data().starts.len() as u32)
                             })
        }
        else
        {
            (self.payload.data().report.len(), self.payload.data().starts.len() as u32)
        };
        CrashInfo { report_bytes,
                    line_count,
                    ..self.info }
    }

    pub fn report(&self) -> &str
    {
        &self.payload.data().report
    }

    pub fn set_page_lines(&mut self, count: u32)
    {
        self.info.page_lines = count.max(1);
        self.clamp();
    }

    pub fn line(&self, index: usize) -> Option<&str>
    {
        let start = *self.payload.data().starts.get(index)? as usize;
        let end = self.payload.data().starts.get(index + 1).copied().map_or(self.payload
                                                                                .data()
                                                                                .report
                                                                                .len(),
                                                                            |n| n as usize);
        self.payload.data().report.get(start..end).map(|line| line.trim_end_matches(['\r', '\n']))
    }

    #[cfg(systemrisc)]
    pub(crate) fn columns(&self) -> usize
    {
        self.columns
    }

    pub fn set_columns(&mut self, columns: usize) -> Result<(), Status>
    {
        let columns = columns.max(1);
        if self.columns == columns
        {
            return Ok(());
        }
        // One entry per byte is a conservative upper bound even for all-newline
        // input. Reserve before changing any layout state.
        let data = self.payload.data_mut()?;
        data.starts
            .try_reserve(data.report.len().saturating_add(1).saturating_sub(data.starts.len()))
            .map_err(|_| NO_MEMORY)?;
        self.columns = columns;
        data.starts.clear();
        self.column = 0;
        self.previous_cr = false;
        data.starts.push(0);
        for (offset, character) in data.report.char_indices()
        {
            layout(&mut data.starts, &mut self.column, &mut self.previous_cr, columns, offset,
                   character);
        }
        self.clamp();
        Ok(())
    }

    fn clamp(&mut self)
    {
        self.info.first_line =
            self.info.first_line.min(self.info().line_count.saturating_sub(self.info.page_lines));
    }

    pub fn scroll(&mut self, delta: i32) -> bool
    {
        let previous = self.info.first_line;
        self.info.first_line =
            (i64::from(previous) + i64::from(delta)).clamp(0, u32::MAX as i64) as u32;
        self.clamp();
        self.info.first_line != previous
    }

    pub fn request(&mut self, call: &mut CrashCall, input: &str, output: &mut [u8])
                   -> Result<bool, Status>
    {
        let changed = match call.operation
        {
            BEGIN =>
            {
                let app = self.info.app;
                let thread = self.info.thread;
                let data = self.payload.data_mut()?;
                data.starts
                    .try_reserve(usize::from(data.starts.capacity() == 0))
                    .map_err(|_| NO_MEMORY)?;
                data.begin_origin(app, thread);
                data.report.clear();
                data.starts.clear();
                data.starts.push(0);
                let payload = core::mem::take(&mut self.payload);
                *self = Self { capture: Capture::Streaming,
                               payload,
                               info: CrashInfo { app,
                                                 thread,
                                                 flags: call.flags,
                                                 page_lines: PAGE_LINES,
                                                 ..Default::default() },
                               ..Default::default() };
                true
            }
            APPEND =>
            {
                if !matches!(self.capture, Capture::Streaming)
                {
                    return Err(ACCESS_DENIED);
                }
                if input.len() > MAX_APPEND_BYTES
                {
                    return Err(BAD_ARGUMENT);
                }
                if self.payload
                       .data()
                       .report
                       .len()
                       .checked_add(input.len())
                       .is_none_or(|n| n > MAX_REPORT_BYTES)
                {
                    self.info.flags |= TRUNCATED;
                    call.info = self.info();
                    return Err(NO_MEMORY);
                }
                let data = self.payload.data_mut()?;
                let initial = usize::from(data.starts.is_empty());
                if data.report.try_reserve(input.len()).is_err()
                   || data.starts.try_reserve(input.len().saturating_add(initial)).is_err()
                {
                    self.info.flags |= TRUNCATED;
                    call.info = self.info();
                    return Err(NO_MEMORY);
                }
                if data.starts.is_empty()
                {
                    data.starts.push(0);
                }
                let base = data.report.len();
                data.report.push_str(input);
                for (offset, character) in input.char_indices()
                {
                    layout(&mut data.starts,
                           &mut self.column,
                           &mut self.previous_cr,
                           self.columns,
                           base + offset,
                           character);
                }
                self.info.flags |= call.flags;
                !input.is_empty()
            }
            SHOW =>
            {
                self.submit(call.flags);
                true
            }
            INFO => false,
            READ =>
            {
                let archived = self.archive_origin.and_then(storage::archived);
                let data = if self.archive_origin.is_some()
                {
                    archived.as_ref().ok_or(NOT_FOUND)?.data()
                }
                else
                {
                    self.payload.data()
                };
                let bytes = data.report.as_bytes().get(call.offset..).ok_or(BAD_ARGUMENT)?;
                let count = bytes.len().min(output.len());
                output[..count].copy_from_slice(&bytes[..count]);
                call.length = count;
                false
            }
            SCROLL => self.scroll(call.delta),
            DISMISS =>
            {
                if self.info.app != 0 && self.info.app == crate::context::finder_app().0
                {
                    return Err(ACCESS_DENIED);
                }
                let changed = self.info.dismissed == 0;
                self.info.dismissed = 1;
                self.dismissal = if self.superseded
                {
                    None
                }
                else
                {
                    self.archive_origin.or(self.payload.origin())
                };
                changed
            }
            _ => return Err(NOT_SUPPORTED)
        };
        call.info = self.info();
        Ok(changed)
    }
}

fn layout(starts: &mut Vec<u32>, column: &mut usize, previous_cr: &mut bool, columns: usize,
          offset: usize, character: char)
{
    if character == '\n' && *previous_cr
    {
        if let Some(start) = starts.last_mut()
        {
            *start = (offset + 1) as u32;
        }
        *previous_cr = false;
        return;
    }
    *previous_cr = character == '\r';
    if matches!(character, '\r' | '\n')
    {
        starts.push((offset + character.len_utf8()) as u32);
        *column = 0;
    }
    else
    {
        let mut advance = if character == '\t' { 4 - (*column % 4) } else { 1 };
        if *column != 0 && column.saturating_add(advance) > columns
        {
            starts.push(offset as u32);
            *column = 0;
            advance = if character == '\t' { 4 } else { 1 };
        }
        *column += advance;
    }
}

