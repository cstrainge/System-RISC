//! Resident, application-local panic reports. Raw UTF-8 is retained independently
//! of the visible page, so scrolling and copying never discard diagnostic text.
#![forbid(unsafe_code)]
use alloc::{string::String, vec::Vec};

use mac_abi::{crashes::*, *};

pub const RESOURCE_ID: i16 = -16400;
pub const DEFAULT_COLUMNS: usize = 90;
pub const PAGE_LINES: u32 = 23;

pub struct Manager
{
    report:                     String,
    starts:                     Vec<u32>,
    columns:                    usize,
    column:                     usize,
    previous_cr:                bool,
    info:                       CrashInfo,
    #[cfg(target_arch = "riscv64")]
    pub(super) controls:        [Option<u64>; 6],
    #[cfg(target_arch = "riscv64")]
    pub(super) control_enabled: [bool; 6],
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
    report:      String,
    starts:      Vec<u32>,
    columns:     usize,
    column:      usize,
    previous_cr: bool,
    info:        CrashInfo
}
impl Default for Manager
{
    fn default() -> Self
    {
        Self { report:                                          String::new(),
               starts:                                          Vec::new(),
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
               tracking:                                        None,
               #[cfg(target_arch = "riscv64")]
               copy_failed:                                     false,
               #[cfg(target_arch = "riscv64")]
               restart_pending:                                 false,
               #[cfg(target_arch = "riscv64")]
               restart_error:                                   None }
    }
}
impl Manager
{
    #[cfg(any(target_arch = "riscv64", test))]
    pub(crate) fn snapshot(&self) -> Result<Snapshot, Status>
    {
        let mut report = String::new();
        report.try_reserve_exact(self.report.len()).map_err(|_| NO_MEMORY)?;
        report.push_str(&self.report);
        let mut starts = Vec::new();
        starts.try_reserve_exact(self.starts.len()).map_err(|_| NO_MEMORY)?;
        starts.extend_from_slice(&self.starts);
        Ok(Snapshot { report,
                      starts,
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
        Self { report: snapshot.report,
               starts: snapshot.starts,
               columns: snapshot.columns,
               column: snapshot.column,
               previous_cr: snapshot.previous_cr,
               info,
               ..Default::default() }
    }

    pub fn set_context(&mut self, app: u64, thread: u64)
    {
        self.info.app = app;
        self.info.thread = thread;
    }

    pub fn set_window(&mut self, window: WindowId)
    {
        self.info.window = window;
        #[cfg(target_arch = "riscv64")]
        {
            self.controls = [None; 6];
            self.control_enabled = [false; 6];
            self.tracking = None;
        }
    }

    pub fn info(&self) -> CrashInfo
    {
        CrashInfo { report_bytes: self.report.len(),
                    line_count: self.starts.len() as u32,
                    ..self.info }
    }

    pub fn report(&self) -> &str
    {
        &self.report
    }

    pub fn set_page_lines(&mut self, count: u32)
    {
        self.info.page_lines = count.max(1);
        self.clamp();
    }

    pub fn line(&self, index: usize) -> Option<&str>
    {
        let start = *self.starts.get(index)? as usize;
        let end = self.starts.get(index + 1).copied().map_or(self.report.len(), |n| n as usize);
        self.report.get(start..end).map(|line| line.trim_end_matches(['\r', '\n']))
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
        self.starts
            .try_reserve(self.report.len().saturating_add(1).saturating_sub(self.starts.len()))
            .map_err(|_| NO_MEMORY)?;
        self.columns = columns;
        self.starts.clear();
        self.column = 0;
        self.previous_cr = false;
        self.starts.push(0);
        for (offset, character) in self.report.char_indices()
        {
            layout(&mut self.starts, &mut self.column, &mut self.previous_cr, columns, offset,
                   character);
        }
        self.clamp();
        Ok(())
    }

    fn clamp(&mut self)
    {
        self.info.first_line =
            self.info
                .first_line
                .min((self.starts.len() as u32).saturating_sub(self.info.page_lines));
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
                let mut starts = Vec::new();
                starts.try_reserve(1).map_err(|_| NO_MEMORY)?;
                starts.push(0);
                *self = Self { starts,
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
                if self.report.len().checked_add(input.len()).is_none_or(|n| n > MAX_REPORT_BYTES)
                {
                    self.info.flags |= TRUNCATED;
                    call.info = self.info();
                    return Err(NO_MEMORY);
                }
                if self.report.try_reserve(input.len()).is_err()
                   || self.starts.try_reserve(input.len().saturating_add(1)).is_err()
                {
                    self.info.flags |= TRUNCATED;
                    call.info = self.info();
                    return Err(NO_MEMORY);
                }
                if self.starts.is_empty()
                {
                    self.starts.push(0);
                }
                let base = self.report.len();
                self.report.push_str(input);
                for (offset, character) in input.char_indices()
                {
                    layout(&mut self.starts,
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
                self.info.flags |= call.flags;
                true
            }
            INFO => false,
            READ =>
            {
                let bytes = self.report.as_bytes().get(call.offset..).ok_or(BAD_ARGUMENT)?;
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
        old.set_context(401, 991);
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
        assert_eq!(restored.starts.len(), info.line_count as usize);
    }
}
