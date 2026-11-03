//! The recovery report uses the same managed row viewport and loaded scrollbar
//! as other System UI. The immutable report remains the source of exposed rows.
use mac_abi::scrollbox as sb;

use super::*;
impl crate::toolbox::runtime::Toolbox
{
    fn crash_scroll_spec(&self) -> Result<sb::Spec, Status>
    {
        let fonts = crate::kernel::fonts().ok_or(NOT_SUPPORTED)?;
        let metrics = fonts.metrics(fonts_service::selection(MONACO))?;
        Ok(sb::Spec { viewport: Rect::new(REPORT.left + 4,
                                          REPORT.top + 4,
                                          REPORT.right - 20,
                                          REPORT.bottom - 4),
                      scrollbar: Rect::new(REPORT.right - 17,
                                           REPORT.top + 1,
                                           REPORT.right - 1,
                                           REPORT.bottom - 1),
                      font: MONACO,
                      padding: Point { h: 0,
                                       v: metrics.ascent },
                      row_height: metrics.ascent
                                         .saturating_add(metrics.descent)
                                         .saturating_add(metrics.leading)
                                         .max(1),
                      count: self.crash.info().line_count,
                      ..Default::default() })
    }

    pub(crate) fn crash_scroll_raster_budget() -> Result<usize, Status>
    {
        let width = usize::try_from(REPORT.right - REPORT.left - 24).map_err(|_| BAD_ABI)?;
        let height = usize::try_from(REPORT.bottom - REPORT.top - 8).map_err(|_| BAD_ABI)?;
        width.checked_mul(height).and_then(|n| n.checked_mul(8)).ok_or(BAD_ABI)
    }

    fn fill_crash_rows(&mut self, view: &sb::Call) -> Result<(), Status>
    {
        for row in view.first..view.end
        {
            let mut call = sb::Call { id: view.id,
                                      operation: sb::SET_ROW,
                                      row,
                                      ..Default::default() };
            let line = self.crash.line(row as usize).ok_or(BAD_ABI)?;
            let mut length = 0usize;
            let mut column = 0usize;
            for ch in line.chars()
            {
                let mut utf8 = [0; 4];
                let text =
                    if ch == '\t' { &"    "[..4 - column % 4] } else { ch.encode_utf8(&mut utf8) };
                let end = length.checked_add(text.len()).ok_or(BAD_ARGUMENT)?;
                call.text
                    .get_mut(length..end)
                    .ok_or(BAD_ARGUMENT)?
                    .copy_from_slice(text.as_bytes());
                length = end;
                column += if ch == '\t' { text.len() } else { 1 };
            }
            call.text_length = length as u32;
            self.scroll_box(&mut call)?;
        }
        Ok(())
    }

    pub(super) fn crash_scrollbox(&mut self) -> Result<(), Status>
    {
        let spec = self.crash_scroll_spec()?;
        let info = self.crash.info();
        let mut call = sb::Call { window: info.window.0,
                                  spec,
                                  ..Default::default() };
        if let Some(id) = self.crash.scrollbox
        {
            call.id = id;
            call.operation = sb::INFO;
            self.scroll_box(&mut call)?;
            if call.spec != spec
            {
                call.operation = sb::CONFIGURE;
                call.spec = spec;
                self.scroll_box(&mut call)?;
            }
        }
        else
        {
            call.operation = sb::CREATE;
            let result = self.scroll_box(&mut call);
            if call.id != 0
            {
                self.crash.scrollbox = Some(call.id);
            }
            result?;
        }
        call.operation = sb::SCROLL_TO;
        call.top = info.first_line;
        self.scroll_box(&mut call)?;
        self.fill_crash_rows(&call)?;
        call.operation = sb::DRAW;
        self.scroll_box(&mut call)?;
        Ok(())
    }

    pub(super) fn cancel_crash_scrollbox(&mut self)
    {
        if self.crash.scroll_tracking
        {
            self.crash.scroll_tracking = false;
            if let Some(id) = self.crash.scrollbox
            {
                let _ = self.scroll_box(&mut sb::Call { id,
                                                        operation: sb::TRACK_CANCEL,
                                                        ..Default::default() });
            }
        }
    }

    pub(super) fn crash_scroll_pointer(&mut self, point: Point, down: bool, when: u32) -> bool
    {
        if self.crash.scrollbox.is_none()
        {
            return false;
        }
        if !self.crash.scroll_tracking
           && (!down || !REPORT.contains(point) || self.crash.tracking.is_some())
        {
            return false;
        }
        self.step_crash_scroll(point, down, when);
        true
    }

    /// Poll only a genuinely captured control, using current device state and
    /// the existing UI clock. This never manufactures an application Input.
    pub(in crate::toolbox::runtime) fn poll_crash_scroll(&mut self, when: u32) -> bool
    {
        if !self.crash.scroll_tracking
        {
            return false;
        }
        if self.owner != self.foreground
           || self.windows.active_window() != Some(self.crash.info().window)
        {
            self.cancel_crash_tracking();
            return false;
        }
        let Some(bounds) =
            self.windows.window(self.crash.info().window).map(|window| window.bounds)
        else
        {
            self.cancel_crash_tracking();
            return false;
        };
        // Device state is in screen coordinates, exactly as in crash_pointer.
        // The retained scrollbar and its capture anchor are window-local.
        let point = Point { h: self.events.mouse.h.saturating_sub(bounds.left),
                            v: self.events.mouse.v.saturating_sub(bounds.top) };
        self.step_crash_scroll(point, self.events.modifiers & mac_abi::events::BTN_STATE == 0, when)
    }

    /// True only after an actual changed CDEF/viewport drawing is committed.
    fn step_crash_scroll(&mut self, point: Point, down: bool, when: u32) -> bool
    {
        let Some(id) = self.crash.scrollbox
        else
        {
            return false;
        };
        let mut call = sb::Call { id,
                                  point,
                                  ticks: u64::from(when),
                                  operation: if self.crash.scroll_tracking
                                  {
                                      sb::TRACK_POLL
                                  }
                                  else
                                  {
                                      sb::TRACK_BEGIN
                                  },
                                  flags: if down { sb::BUTTON_DOWN } else { 0 },
                                  ..Default::default() };
        match self.scroll_box(&mut call)
        {
            Ok(_) =>
            {
                self.crash.scroll_tracking = call.flags & sb::TRACKING != 0;
                let moved = call.top != self.crash.info().first_line;
                if moved
                {
                    let delta = i64::from(call.top) - i64::from(self.crash.info().first_line);
                    self.crash.scroll(delta.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32);
                }
                // Press/release and leaving/reentering a scrollbar part change
                // its CDEF highlight even when the viewport stays on one row.
                if moved || call.flags & sb::CHANGED != 0
                {
                    if let Err(status) =
                        self.fill_crash_rows(&call).and_then(|_| self.draw_crash_scroll())
                    {
                        serial!("CRASH_SCROLL_REFUSED status={status}; report retained");
                        self.cancel_crash_tracking();
                        return false;
                    }
                    else if moved
                    {
                        self.log_crash_scroll();
                    }
                    return true;
                }
            }
            Err(status) =>
            {
                serial!("CRASH_SCROLL_REFUSED status={status}; report retained");
                self.cancel_crash_tracking();
            }
        }
        false
    }
}
