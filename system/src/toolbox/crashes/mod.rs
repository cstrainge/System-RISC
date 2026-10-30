//! Resident panic reports. Native capture uses one boot-admitted System buffer;
//! genuine saved managers retain immutable full diagnostic snapshots independently
//! of the visible page and of a dying application account.
#![forbid(unsafe_code)]
use alloc::vec::Vec;
mod storage;
#[cfg(target_arch = "riscv64")]
mod pending;
use mac_abi::{crashes::*, *};
#[cfg(target_arch = "riscv64")]
pub(crate) use pending::owner_stopped as pending_owner_stopped;
#[cfg(target_arch = "riscv64")]
pub(crate) use pending::{cleanup_progress, modal_available, pump_dismissals, pump_reports,
                         take_for_termination};
use storage::Payload;

pub const RESOURCE_ID: i16 = -16400;
pub const DEFAULT_COLUMNS: usize = 90;
pub const PAGE_LINES: u32 = 23;

/// Prepared once from the fixed resident report geometry and its System font.
#[cfg(target_arch = "riscv64")]
#[derive(Clone, Copy)]
pub(crate) struct ReportLayout
{
    pub(crate) columns:    usize,
    pub(crate) page_lines: u32
}
#[cfg(target_arch = "riscv64")]
static LAYOUT: spin::Once<ReportLayout> = spin::Once::new();
#[cfg(target_arch = "riscv64")]
pub(crate) fn prepared_layout() -> Result<ReportLayout, Status>
{
    LAYOUT.get().copied().ok_or(NOT_SUPPORTED)
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

pub struct Manager
{
    capture:                    Capture,
    dismissal:                  Option<storage::Origin>,
    superseded:                 bool,
    payload:                    Payload,
    archive_origin:             Option<storage::Origin>,
    columns:                    usize,
    column:                     usize,
    previous_cr:                bool,
    info:                       CrashInfo,
    #[cfg(target_arch = "riscv64")]
    pub(super) controls:        [Option<u64>; 6],
    #[cfg(target_arch = "riscv64")]
    pub(super) control_enabled: [bool; 6],
    #[cfg(target_arch = "riscv64")]
    pub(super) control_labels:  [u32; 6],
    #[cfg(target_arch = "riscv64")]
    pub(super) tracking:        Option<usize>,
    #[cfg(target_arch = "riscv64")]
    pub(super) copy_failed:     bool,
    #[cfg(target_arch = "riscv64")]
    pub(super) restart_pending: bool,
    #[cfg(target_arch = "riscv64")]
    pub(super) restart_error:   Option<Status>
}

/// Diagnostic data is independent of the dying application's window/control
/// objects. A recovery controller can preserve it before stopping that owner.
#[cfg(any(target_arch = "riscv64", test))]
pub(crate) struct Snapshot
{
    capture:     Capture,
    superseded:  bool,
    payload:     Payload,
    columns:     usize,
    column:      usize,
    previous_cr: bool,
    info:        CrashInfo
}
#[cfg(target_arch = "riscv64")]
impl Snapshot
{
    pub(crate) fn retain(&self) -> Result<Self, Status>
    {
        Ok(Self { capture:     self.capture,
                  superseded:  self.superseded,
                  payload:     self.payload.snapshot()?,
                  columns:     self.columns,
                  column:      self.column,
                  previous_cr: self.previous_cr,
                  info:        self.info })
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
#[cfg(target_arch = "riscv64")]
pub(crate) fn queue_report(binding: crate::toolbox::processes::ResourceBinding,
                           snapshot: Snapshot, flags: u32)
                           -> Result<CrashInfo, Status>
{
    pending::queue(binding, snapshot, flags)
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn queue_dismissal(binding: crate::toolbox::processes::ResourceBinding,
                              activity: crate::recovery::Activity, snapshot: Snapshot,
                              window: WindowId)
                              -> Result<(), Status>
{
    pending::queue_dismissal(binding, activity, snapshot, window)
}

impl Default for Manager
{
    fn default() -> Self
    {
        Self { capture:                                         Capture::None,
               dismissal:                                       None,
               superseded:                                      false,
               payload:                                         Payload::default(),
               archive_origin:                                  None,
               columns:                                         DEFAULT_COLUMNS,
               column:                                          0,
               previous_cr:                                     false,
               info:                                            CrashInfo { page_lines:
                                                                                PAGE_LINES,
                                                                            ..Default::default() },
               #[cfg(target_arch = "riscv64")]
               controls:                                        [None; 6],
               #[cfg(target_arch = "riscv64")]
               control_enabled:                                 [false; 6],
               #[cfg(target_arch = "riscv64")]
               control_labels:                                  [u32::MAX; 6],
               #[cfg(target_arch = "riscv64")]
               tracking:                                        None,
               #[cfg(target_arch = "riscv64")]
               copy_failed:                                     false,
               #[cfg(target_arch = "riscv64")]
               restart_pending:                                 false,
               #[cfg(target_arch = "riscv64")]
               restart_error:                                   None }
    }
}
#[cfg(target_arch = "riscv64")]
pub(crate) fn initialize_protected_report(permit: crate::allocator::recovery::Permit)
                                          -> Result<(), Status>
{
    crate::context::with_recovery_allocation(permit, || {
        crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
            let layout = crate::toolbox::runtime::Desktop::measure_crash_report_layout()?;
            storage::prepare()?;
            LAYOUT.call_once(|| layout);
            Ok(())
        })
    })
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn protected_stock_available() -> bool
{
    storage::available()
}

impl Manager
{
    #[cfg(target_arch = "riscv64")]
    pub(crate) fn capture_attempt(&mut self)
    {
        self.dismissal = None;
        self.superseded = true;
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn capture_superseded(&self) -> bool
    {
        self.superseded
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn acknowledged_capture(&self, owner: u64) -> bool
    {
        !self.superseded
        && self.info.dismissed != 0
        && self.info.window.0 == 0
        && self.dismissal.is_some_and(|receipt| receipt.app() == owner)
    }

    #[cfg(target_arch = "riscv64")]
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

    #[cfg(any(target_arch = "riscv64", test))]
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
                      info: self.info() })
    }

    #[cfg(any(target_arch = "riscv64", test))]
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
               info,
               ..Default::default() }
    }

    #[cfg(target_arch = "riscv64")]
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

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn archive(&self)
    {
        self.payload.archive();
    }

    /// End this saved manager's use after actual UI disposal. The System archive
    /// retains the complete latest bytes, while the one boot buffer can be
    /// claimed for a later report once every genuine snapshot is gone.
    #[cfg(target_arch = "riscv64")]
    pub(crate) fn release_payload(&mut self)
    {
        if let Some(origin) = self.payload.origin()
        {
            self.archive_origin = Some(origin);
        }
        self.payload.archive();
        self.payload = Payload::default();
    }

    #[cfg(target_arch = "riscv64")]
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

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn finder_recovered(&mut self)
    {
        self.restart_error = None;
        self.restart_pending = false;
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn add_flags(&mut self, flags: u32)
    {
        self.info.flags |= flags;
    }

    #[cfg(target_arch = "riscv64")]
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

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn seal_system_diagnostic(&mut self)
    {
        self.capture = Capture::SystemDiagnostic;
    }

    pub fn set_window(&mut self, window: WindowId)
    {
        self.info.window = window;
        #[cfg(target_arch = "riscv64")]
        {
            self.controls = [None; 6];
            self.control_enabled = [false; 6];
            self.control_labels = [u32::MAX; 6];
            self.tracking = None;
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

    #[cfg(target_arch = "riscv64")]
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

#[cfg(test)]
mod recovery_tests
{
    use super::*;
    #[test]
    fn recovery_snapshot_preserves_complete_origin_and_layout_without_ui_ids()
    {
        let mut old = Manager::default();
        old.info.app = 401;
        old.info.thread = 991;
        old.request(&mut CrashCall { operation: BEGIN,
                                     ..Default::default() },
                    "",
                    &mut [])
           .unwrap();
        let text = "Complete report café λ\r\nsecond\tline\nthird";
        old.request(&mut CrashCall { operation: APPEND,
                                     ..Default::default() },
                    text,
                    &mut [])
           .unwrap();
        old.set_columns(12).unwrap();
        old.set_page_lines(2);
        old.scroll(i32::MAX);
        old.set_window(WindowId(345));
        let info = old.info();
        let snapshot = old.snapshot().unwrap();
        old.request(&mut CrashCall { operation: BEGIN,
                                     ..Default::default() },
                    "",
                    &mut [])
           .unwrap();
        drop(old);
        let restored = Manager::from_snapshot(snapshot);
        assert_eq!(restored.report(), text);
        assert_eq!(restored.info(),
                   CrashInfo { window: WindowId(0),
                               dismissed: 0,
                               ..info });
        assert_eq!(restored.columns, 12);
        assert_eq!(restored.column, 5);
        assert_eq!(restored.payload.data().starts.len(), info.line_count as usize);
    }
}
