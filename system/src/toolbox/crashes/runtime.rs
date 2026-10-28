//! The report window is serviced by System even while its application has
//! stopped executing. Input outside its content still reaches the desktop.
use alloc::string::String;
use core::fmt;

use mac_abi::{crashes::*, quickdraw::*, *};

use crate::toolbox::{crashes::{RESOURCE_ID, ReportLayout},
                     events::input::Input,
                     quickdraw::{fonts_service, icons_service, service},
                     windows::WindowPart};

const REPORT: Rect = Rect::new(14, 91, 566, 347);
const BUTTONS: [Rect; 6] = [Rect::new(14, 369, 55, 391),
                            Rect::new(61, 369, 102, 391),
                            Rect::new(108, 369, 160, 391),
                            Rect::new(166, 369, 225, 391),
                            Rect::new(302, 369, 421, 391),
                            Rect::new(435, 369, 566, 391)];
const LABELS: [u32; 6] = [3, 4, 5, 6, 2, 1];
#[derive(Clone, Copy, PartialEq, Eq)]
enum Paint
{
    Full,
    Report,
    Warnings
}
const FOOTER: Rect = Rect::new(14, 350, 566, 365);
const WARNINGS: Rect = Rect::new(14, 74, 566, 87);
const MONACO: FontSelection = FontSelection { family: 4,
                                              size:   9,
                                              face:   0 };

fn checked(status: Status) -> Result<(), Status>
{
    if status == OK { Ok(()) } else { Err(status) }
}

// STRING replies already pass the Resource Manager's canonical MacRoman/UTF-8
// boundary. Keep that existing reply capacity on the stack instead of cloning it.
struct Label
{
    bytes:  [u8; 256],
    length: usize
}
impl Label
{
    fn text(&self) -> Result<&str, Status>
    {
        core::str::from_utf8(&self.bytes[..self.length]).map_err(|_| BAD_ARGUMENT)
    }

    fn as_bytes(&self) -> &[u8]
    {
        &self.bytes[..self.length]
    }

    fn len(&self) -> usize
    {
        self.length
    }
}
impl fmt::Display for Label
{
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        output.write_str(self.text().map_err(|_| fmt::Error)?)
    }
}
fn label(index: u32) -> Result<Label, Status>
{
    let mut bytes = [0; 256];
    let mut call = resources::ResourceCall { operation: resources::STRING,
                                             scope: resources::SYSTEM,
                                             key: ResourceKey { kind: *b"STR#",
                                                                id:   RESOURCE_ID },
                                             index,
                                             ..Default::default() };
    crate::kernel::with_resources(|resources| resources.request(&mut call, "", &mut bytes)).ok_or(NOT_SUPPORTED)??;
    core::str::from_utf8(bytes.get(..call.transferred).ok_or(BAD_ARGUMENT)?).map_err(|_| {
                                                                                BAD_ARGUMENT
                                                                            })?;
    Ok(Label { bytes,
               length: call.transferred })
}
fn append(output: &mut String, text: &str) -> Result<(), Status>
{
    output.try_reserve(text.len()).map_err(|_| NO_MEMORY)?;
    output.push_str(text);
    Ok(())
}
fn write_text(output: &mut String, arguments: fmt::Arguments<'_>) -> Result<(), Status>
{
    struct Writer<'a>
    {
        output: &'a mut String,
        error:  Option<Status>
    }
    impl fmt::Write for Writer<'_>
    {
        fn write_str(&mut self, value: &str) -> fmt::Result
        {
            if self.error.is_some()
            {
                return Err(fmt::Error);
            }
            if let Err(error) = append(self.output, value)
            {
                self.error = Some(error);
                return Err(fmt::Error);
            }
            Ok(())
        }
    }
    let mut writer = Writer { output,
                              error: None };
    fmt::write(&mut writer, arguments).map_err(|_| writer.error.unwrap_or(BAD_ARGUMENT))
}
fn formatted(arguments: fmt::Arguments<'_>) -> Result<String, Status>
{
    let mut output = String::new();
    write_text(&mut output, arguments)?;
    Ok(output)
}
/// Exact UTF-8 extent after the existing four-column tab expansion.
fn expanded_line_length(line: &str) -> Result<usize, Status>
{
    let (mut length, mut column) = (0usize, 0usize);
    for character in line.chars()
    {
        let columns = if character == '\t' { 4 - (column % 4) } else { 1 };
        column = column.checked_add(columns).ok_or(BAD_ARGUMENT)?;
        length = length.checked_add(if character == '\t' { columns } else { character.len_utf8() })
                       .ok_or(BAD_ARGUMENT)?;
    }
    Ok(length)
}

fn window_request(id: WindowId) -> WindowRequest
{
    WindowRequest { id:             id.0,
                    bounds:         Rect::ZERO,
                    title:          [0; WINDOW_TITLE_BYTES],
                    title_length:   0,
                    procedure:      1,
                    minimum_width:  580,
                    minimum_height: 404,
                    reserved:       0 }
}

impl super::Desktop
{
    /// Boot preparation uses the renderer's actual managed-button requirement.
    pub(crate) const fn crash_control_count() -> usize
    {
        BUTTONS.len()
    }

    pub fn crash_request(&mut self, call: &mut CrashCall, input: &str, output: &mut [u8])
                         -> Result<(), Status>
    {
        let previous = self.crash.info();
        let initial_layout = if call.operation == BEGIN
        {
            Some(crate::toolbox::crashes::prepared_layout()?)
        }
        else
        {
            None
        };
        let thread = if call.operation == BEGIN || previous.app == 0
        {
            let identity = crate::context::current_thread_context().identity();
            if identity.app.0 == self.owner
            {
                identity.thread.0
            }
            else
            {
                self.processes.get(self.owner)?.info.thread
            }
        }
        else
        {
            previous.thread
        };
        if previous.app == 0 && matches!(call.operation, APPEND | SHOW | SCROLL | DISMISS)
        {
            return Err(BAD_ARGUMENT);
        }
        if call.operation == BEGIN
        {
            // Capacity/reader refusal leaves the previous complete diagnostic
            // and its origin untouched. Only successful BEGIN clears it.
            let old = self.crash.info().window;
            if old.0 != 0 && self.windows.window(old).is_some()
            {
                // Native dispatch must first drain the actual service window
                // under its private Activity. Capture cannot bypass that ACK.
                return Err(SERVICE_BUSY);
            }
            self.crash.begin_protected(self.owner, thread, call.flags)?;
        }
        if call.operation == DISMISS
        {
            if previous.app != 0 && previous.app == crate::context::finder_app().0
            {
                return Err(ACCESS_DENIED);
            }
            let activity = crate::context::recovery_activity().ok_or(ACCESS_DENIED)?;
            // Preflight retention before admitting any destructive close.
            let binding = self.processes.resource_binding(self.owner)?;
            let snapshot = self.crash.snapshot()?;
            if let Err(status) = self.close_crash_report(&activity)
            {
                crate::toolbox::crashes::queue_dismissal(binding,
                                                         activity.clone(),
                                                         snapshot,
                                                         previous.window)?;
                activity.close();
                return Err(status);
            }
            activity.close();
        }
        let changed =
            if call.operation == BEGIN { true } else { self.crash.request(call, input, output)? };
        if let Some(layout) = initial_layout
        {
            // BEGIN just emptied the boot-admitted arrays. Incremental APPEND
            // now builds the final index; SHOW never rescans the full report.
            self.crash.set_columns(layout.columns)?;
            self.crash.set_page_lines(layout.page_lines);
        }
        if call.operation == SHOW
        {
            self.show_crash_report()?;
            self.crash.archive();
        }
        else if call.operation == DISMISS
        {
            self.crash.release_payload();
            serial!("CRASH_REPORT_DISMISSED app={}", self.owner);
        }
        else if changed && self.crash.info().window.0 != 0
        {
            self.draw_crash_report()?;
        }
        call.info = self.crash.info();
        Ok(())
    }

    /// Boot preparation runs after the actual System font manager is ready.
    pub(crate) fn measure_crash_report_layout() -> Result<ReportLayout, Status>
    {
        let fonts = crate::kernel::fonts().ok_or(NOT_SUPPORTED)?;
        let metrics = fonts.metrics(fonts_service::selection(MONACO))?;
        let advance =
            metrics.ascent.saturating_add(metrics.descent).saturating_add(metrics.leading).max(1);
        let columns = ((REPORT.right - REPORT.left - 8) / metrics.max_width.max(1)).max(1) as usize;
        let page_lines = ((REPORT.bottom - REPORT.top - 8) / advance).max(1) as u32;
        Ok(ReportLayout { columns,
                          page_lines })
    }

    fn prepare_crash_layout(&mut self) -> Result<(), Status>
    {
        let layout = crate::toolbox::crashes::prepared_layout()?;
        if self.crash.columns() != layout.columns
        {
            // A fixed report cannot silently repair a foreign layout by doing
            // a full retained-text scan inside the interrupt-masked renderer.
            return Err(BAD_ABI);
        }
        self.crash.set_page_lines(layout.page_lines);
        Ok(())
    }

    pub(crate) fn queue_crash_report(&mut self, flags: u32) -> Result<CrashInfo, Status>
    {
        if self.crash.info().app == 0
        {
            return Err(BAD_ARGUMENT);
        }
        self.prepare_crash_layout()?;
        self.crash.add_flags(flags);
        let binding = self.processes.resource_binding(self.owner)?;
        let snapshot = self.crash.snapshot()?;
        crate::toolbox::crashes::queue_report(binding, snapshot, flags)
    }

    /// Complete normal WDEF disposal before a report recipient is transferred.
    /// A refused close invalidates the ID but retains its original ticket and
    /// Activity in Window Manager; only that manager's ACK clears the wait.
    pub(crate) fn close_crash_report(&mut self, activity: &crate::recovery::Activity)
                                     -> Result<(), Status>
    {
        if activity.owner() != self.owner
        {
            return Err(ACCESS_DENIED);
        }
        self.close_report_window(self.crash.info().window, activity)?;
        self.crash.set_window(WindowId(0));
        activity.set_object(0);
        Ok(())
    }

    fn close_report_window(&mut self, window: WindowId, activity: &crate::recovery::Activity)
                           -> Result<(), Status>
    {
        if window.0 == 0
        {
            return Ok(());
        }
        if self.windows.window(window).is_some()
        {
            let status = self.window(CLOSE_WINDOW, &mut window_request(window));
            if status != OK && self.windows.window(window).is_some()
            {
                // Refusal before invalidation leaves the normal live payload.
                return Err(status);
            }
        }
        if self.windows.recovery_close_pending(window, activity)?
        {
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }

    /// Physical teardown for an abandoned recipient; logical report state is
    /// retained independently and no stopped app context is selected.
    pub(crate) fn cancel_crash_report_for_recovery(&mut self,
                                                   activity: &crate::recovery::Activity,
                                                   window: WindowId)
                                                   -> Result<(), Status>
    {
        activity.run(|| self.close_report_window(window, activity))?
    }

    /// Complete an admitted dismissal without selecting or impersonating a
    /// stopped app. Its actual saved manager may already have been reaped.
    pub(crate) fn finish_crash_dismissal(&mut self,
                                         binding: &crate::toolbox::processes::ResourceBinding,
                                         activity: &crate::recovery::Activity, window: WindowId,
                                         snapshot: &crate::toolbox::crashes::Snapshot)
                                         -> Result<(), Status>
    {
        if binding.ledger().owner() != activity.recipient().owner()
        {
            return Err(ACCESS_DENIED);
        }
        self.close_report_window(window, activity)?;
        let changed = self.finish_saved_crash_dismissal(binding, snapshot)?;
        activity.set_object(0);
        activity.close();
        if changed
        {
            serial!("CRASH_REPORT_DISMISSED app={}", binding.owner());
        }
        Ok(())
    }

    pub(crate) fn resume_crash_report(&mut self) -> Result<WindowId, Status>
    {
        self.show_crash_report()?;
        self.crash.archive();
        Ok(self.crash.info().window)
    }

    pub(crate) fn publish_pending_crash_report(&mut self,
                                               binding:&crate::toolbox::processes::ResourceBinding,
                                               snapshot: crate::toolbox::crashes::Snapshot)
                                               -> Result<WindowId, Status>
    {
        let owner = self.processes.ledger(binding.owner())?;
        if owner.owner() != binding.ledger().owner()
        {
            return Err(ACCESS_DENIED);
        }
        self.enter(binding.owner());
        if self.owner != binding.owner()
        {
            return Err(NOT_FOUND);
        }
        self.crash = crate::toolbox::crashes::Manager::from_snapshot(snapshot);
        self.resume_crash_report()
    }

    fn show_crash_report(&mut self) -> Result<(), Status>
    {
        let activity = crate::context::recovery_activity().ok_or(ACCESS_DENIED)?;
        if activity.owner() != self.owner
        {
            return Err(ACCESS_DENIED);
        }
        if self.dialogs.active()
           && let Err(status) = self.close_dialog()
        {
            // The protocol is already invalidated. Window cleanup retains its
            // own ticket on refusal; a child must not suppress the panic report.
            serial!("CRASH_ALERT_CLEANUP_REFUSED status={} protocol=disposed child=retained",
                    status);
        }
        self.menus.clear();
        self.cursors = Default::default();
        // An application can panic between BeginUpdate and EndUpdate. Its
        // uncommitted pixels must never leak into a later presentation.
        while let Some(index) =
            self.pending_content
                .iter()
                .position(|content| content.id.window().is_ok_and(|id| self.owns(id)))
        {
            let id = self.pending_content.remove(index).id.window()?;
            self.windows.end_update(id);
            self.textedit.begin_cursor_update(id);
        }
        self.prepare_crash_layout()?;
        let old = self.crash.info().window;
        if old.0 != 0 && self.windows.window(old).is_some()
        {
            checked(self.window(SELECT_WINDOW, &mut window_request(old)))?;
            return self.draw_crash_report();
        }
        let mut request = crate::kernel::with_resources(|resources| {
                              let (_, resource) = resources.get(resources::SYSTEM,
                                                                ResourceKey { kind: *b"WIND",
                                                                              id:   RESOURCE_ID })?;
                              let bytes = &resource.data;
                              let word = |at: usize| -> Result<i16, Status> {
                                  Ok(i16::from_be_bytes(bytes.get(at..at + 2)
                                                             .ok_or(BAD_ARGUMENT)?
                                                             .try_into()
                                                             .map_err(|_| BAD_ARGUMENT)?))
                              };
                              let mut request = window_request(WindowId(0));
                              request.bounds = Rect::new(word(2)?, word(0)?, word(6)?, word(4)?);
                              request.procedure = word(8)?;
                              Ok::<_, Status>(request)
                          }).ok_or(NOT_SUPPORTED)??;
        let title = label(0)?;
        request.title[..title.len()].copy_from_slice(title.as_bytes());
        request.title_length = title.len() as u32;
        checked(self.window(NEW_WINDOW, &mut request))?;
        self.crash.set_window(WindowId(request.id));
        activity.set_object(request.id);
        request.bounds = Rect::ZERO;
        request.reserved = 0;
        checked(self.window(SET_GROW_ICON_CLIP, &mut request))?;
        checked(self.window(SELECT_WINDOW, &mut request))?;
        self.draw_crash_report()?;
        let info = self.crash.info();
        serial!("CRASH_REPORT_READY app={} window={} bytes={} lines={} page_lines={} flags={}",
                self.owner, info.window.0, info.report_bytes, info.line_count, info.page_lines,
                info.flags);
        Ok(())
    }

    fn crash_text(&mut self, window: u64, font: FontSelection, x: i16, baseline: i16, text: &str)
                  -> Result<(), Status>
    {
        self.font_draw(&mut FontCall { target:
                                           mac_abi::quickdraw::Target::window(WindowId(window)),
                                       selection: font,
                                       baseline: Point { h: x,
                                                         v: baseline },
                                       ..Default::default() },
                       text,
                       crate::kernel::fonts().ok_or(NOT_SUPPORTED)?)
    }

    fn is_current_finder_report(&self) -> bool
    {
        self.owner == crate::context::finder_app().0 && self.crash.info().app == self.owner
    }

    fn crash_button_enabled(&self, index: usize) -> bool
    {
        let info = self.crash.info();
        match index
        {
            0 | 1 => info.first_line != 0,
            2 | 3 => info.first_line.saturating_add(info.page_lines) < info.line_count,
            4 => info.report_bytes.saturating_add(8) <= mac_abi::scrap::MAX_BYTES,
            5 => !self.crash.restart_pending,
            _ => false
        }
    }

    fn draw_crash_report(&mut self) -> Result<(), Status>
    {
        self.paint_crash(Paint::Full)
    }

    fn draw_crash_scroll(&mut self) -> Result<(), Status>
    {
        self.paint_crash(Paint::Report)
    }

    fn draw_crash_warnings(&mut self) -> Result<(), Status>
    {
        self.paint_crash(Paint::Warnings)
    }

    fn paint_crash(&mut self, mode: Paint) -> Result<(), Status>
    {
        let info = self.crash.info();
        if info.window.0 == 0
        {
            return Err(NOT_FOUND);
        }
        let window = info.window.0;
        let target = mac_abi::quickdraw::Target::window(info.window);
        let mut request = window_request(info.window);
        if mode == Paint::Full
        {
            checked(self.window(BEGIN_UPDATE, &mut request))?;
        }
        else
        {
            let rectangles: &[Rect] =
                if mode == Paint::Report { &[REPORT, FOOTER] } else { &[WARNINGS] };
            let mut bytes = [0; 16];
            for (rect, output) in rectangles.iter().zip(bytes.chunks_exact_mut(8))
            {
                for (word, slot) in
                    [rect.left, rect.top, rect.right, rect.bottom].iter()
                                                                  .zip(output.chunks_exact_mut(2))
                {
                    slot.copy_from_slice(&word.to_le_bytes());
                }
            }
            self.graphics(&mut GraphicsCall { operation: QD_WINDOW_BEGIN_RECTS,
                                              target,
                                              ..Default::default() },
                          &[],
                          &bytes[..rectangles.len() * 8])?;
        }
        let result: Result<(), Status> = (|| {
            self.graphics(&mut GraphicsCall { operation: QD_RESET,
                                              target,
                                              ..Default::default() },
                          &[],
                          &[])?;
            let erases: &[Rect] = match mode
            {
                Paint::Full => &[Rect::new(0, 0, 580, 404)],
                Paint::Report => &[REPORT, FOOTER],
                Paint::Warnings => &[WARNINGS]
            };
            for bounds in erases
            {
                self.graphics(&mut GraphicsCall { operation: QD_SHAPE,
                                                  target,
                                                  bounds: *bounds,
                                                  shape: SHAPE_RECT,
                                                  verb: VERB_ERASE,
                                                  ..Default::default() },
                              &[],
                              &[])?;
            }
            if mode == Paint::Full
            {
                let call = IconCall { target:
                                          mac_abi::quickdraw::Target::window(WindowId(window)),
                                      scope: resources::SYSTEM,
                                      id: RESOURCE_ID,
                                      destination: Rect::new(18, 14, 50, 46),
                                      ..Default::default() };
                let drawing =
                    crate::kernel::with_resources(|resources| {
                        icons_service::Drawing::new(resources, &call, service::Port::default())
                    }).ok_or(NOT_SUPPORTED)??;
                self.icon(&call, drawing)?;
                self.crash_text(window, FontSelection::default(), 64, 29, label(0)?.text()?)?;
                self.crash_text(window,
                                MONACO,
                                64,
                                48,
                                &formatted(format_args!("{} {}   {} {}",
                                                        label(12)?,
                                                        info.app,
                                                        label(13)?,
                                                        info.thread))?)?;
                let description =
                    if self.is_current_finder_report() { label(8)? } else { label(11)? };
                self.crash_text(window, MONACO, 14, 72, description.text()?)?;
            }
            if matches!(mode, Paint::Full | Paint::Warnings)
            {
                let mut warnings = String::new();
                for (enabled, index) in [(info.flags & TRUNCATED != 0, 9),
                                         (info.flags & FORMAT_FAILED != 0, 10),
                                         (self.crash.copy_failed, 17)]
                {
                    if enabled
                    {
                        if !warnings.is_empty()
                        {
                            append(&mut warnings, "   ")?;
                        }
                        append(&mut warnings, label(index)?.text()?)?;
                    }
                }
                if let Some(status) = self.crash.restart_error
                {
                    warnings.clear();
                    append(&mut warnings,
                           label(if status == NO_MEMORY { 19 } else { 18 })?.text()?)?;
                    write_text(&mut warnings, format_args!(" ({status})"))?;
                }
                self.crash_text(window, MONACO, 14, 84, &warnings)?;
            }
            if matches!(mode, Paint::Full | Paint::Report)
            {
                self.graphics(&mut GraphicsCall { operation: QD_SHAPE,
                                              target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                              bounds: REPORT,
                                              shape: SHAPE_RECT,
                                              verb: VERB_FRAME,
                                              ..Default::default() },
                          &[],
                          &[])?;
                self.graphics(&mut GraphicsCall { operation: QD_CLIP_RECT,
                                              target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                              bounds: Rect::new(REPORT.left + 3,
                                                                REPORT.top + 3,
                                                                REPORT.right - 3,
                                                                REPORT.bottom - 3),
                                              ..Default::default() },
                          &[],
                          &[])?;
                let fonts = crate::kernel::fonts().ok_or(NOT_SUPPORTED)?;
                let metrics = fonts.metrics(fonts_service::selection(MONACO))?;
                let advance = metrics.ascent
                                     .saturating_add(metrics.descent)
                                     .saturating_add(metrics.leading)
                                     .max(1);
                let mut length = 0;
                for visible in 0..info.page_lines
                {
                    let Some(line) = self.crash.line((info.first_line + visible) as usize)
                    else
                    {
                        break;
                    };
                    length = length.max(expanded_line_length(line)?);
                }
                let mut text = String::new();
                text.try_reserve_exact(length).map_err(|_| NO_MEMORY)?;
                for visible in 0..info.page_lines
                {
                    let Some(line) = self.crash.line((info.first_line + visible) as usize)
                    else
                    {
                        break;
                    };
                    text.clear();
                    let mut column = 0;
                    for character in line.chars()
                    {
                        if character == '\t'
                        {
                            let spaces = 4 - (column % 4);
                            for _ in 0..spaces
                            {
                                text.push(' ');
                            }
                            column += spaces;
                        }
                        else
                        {
                            text.push(character);
                            column += 1;
                        }
                    }
                    self.crash_text(window,
                                    MONACO,
                                    REPORT.left + 4,
                                    REPORT.top + 4 + metrics.ascent + visible as i16 * advance,
                                    &text)?;
                }
                self.graphics(&mut GraphicsCall { operation: QD_CLIP_RECT,
                                              target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                              bounds: Rect::new(0, 0, 580, 404),
                                              ..Default::default() },
                          &[],
                          &[])?;
                let coverage = formatted(format_args!("{} {}-{} {} {}   {} {}",
                                                      label(14)?,
                                                      if info.line_count == 0
                                                      {
                                                          0
                                                      }
                                                      else
                                                      {
                                                          info.first_line + 1
                                                      },
                                                      info.first_line
                                                          .saturating_add(info.page_lines)
                                                          .min(info.line_count),
                                                      label(15)?,
                                                      info.line_count,
                                                      info.report_bytes,
                                                      label(16)?))?;
                self.crash_text(window, MONACO, 14, 360, &coverage)?;
            }
            if mode == Paint::Full
            {
                self.crash_controls(true)?;
            }
            Ok(())
        })();
        if mode == Paint::Full
        {
            let ended = if result.is_ok()
            {
                checked(self.window(END_UPDATE, &mut request))
            }
            else
            {
                self.cancel_system_window_update(info.window)
            };
            if let (Err(original), Err(cleanup)) = (result, ended)
            {
                serial!("CRASH_PAINT_CANCEL_REFUSED window={} original={} cleanup={}", window,
                        original, cleanup);
            }
            result?;
            ended?;
        }
        else
        {
            let operation =
                if result.is_ok() { QD_WINDOW_COMMIT_MASK } else { QD_WINDOW_CANCEL_MASK };
            let ended = self.graphics(&mut GraphicsCall { operation,
                                                          target,
                                                          ..Default::default() },
                                      &[],
                                      &[]);
            result?;
            ended?;
        }
        if mode == Paint::Report
        {
            self.crash_controls(false)?;
        }
        Ok(())
    }

    fn crash_controls(&mut self, draw: bool) -> Result<(), Status>
    {
        use mac_abi::controls::*;
        let window = self.crash.info().window.0;
        for (index, bounds) in BUTTONS.iter().enumerate()
        {
            let enabled = u32::from(self.crash_button_enabled(index));
            // This chooses presentation only. Retry execution independently
            // validates the retained Work admission and genuine binding.
            let label_index = if index == 5 && self.is_current_finder_report()
            {
                7
            }
            else if index == 5
                      && self.owner == crate::context::finder_app().0
                      && (self.crash.restart_error.is_some() || self.crash.restart_pending)
            {
                21
            }
            else if index == 5 && self.crash.info().app != self.owner
            {
                20
            }
            else
            {
                LABELS[index]
            };
            let id = match self.crash.controls[index]
            {
                Some(id) =>
                {
                    if self.crash.control_enabled[index] != (enabled != 0)
                       || self.crash.control_labels[index] != label_index
                    {
                        let mut info = ControlCall { operation: INFO_OBJECT,
                                                     id,
                                                     ..Default::default() };
                        self.managed_control(&mut info)?;
                        info.standard.enabled = enabled;
                        if self.crash.control_labels[index] != label_index
                        {
                            let title = label(label_index)?;
                            info.standard.title.fill(0);
                            info.standard.title[..title.len()].copy_from_slice(title.as_bytes());
                            info.standard.title_length = title.len() as u32;
                        }
                        info.operation = CONFIGURE_OBJECT;
                        self.managed_control(&mut info)?;
                        self.crash.control_enabled[index] = enabled != 0;
                        self.crash.control_labels[index] = label_index;
                    }
                    id
                }
                None =>
                {
                    let title = label(label_index)?;
                    let mut standard =
                        mac_abi::definitions::ControlDefinitionState::new(*bounds, title.text()?);
                    standard.enabled = enabled;
                    if index == 5
                    {
                        standard.bounds.inset(-4, -4);
                        standard.flags = mac_abi::definitions::DEFAULT_BUTTON;
                    }
                    let mut call = ControlCall { operation: CREATE_STANDARD,
                                                 window,
                                                 standard,
                                                 ..Default::default() };
                    let created = self.managed_control(&mut call);
                    if call.id != 0
                    {
                        self.crash.controls[index] = Some(call.id);
                        self.crash.control_enabled[index] = enabled != 0;
                        self.crash.control_labels[index] = label_index;
                    }
                    created?;
                    call.id
                }
            };
            if draw
            {
                self.managed_control(&mut ControlCall { operation: DRAW_OBJECT,
                                                        id,
                                                        ..Default::default() })?;
            }
        }
        Ok(())
    }

    fn request_finder_restart(&mut self) -> Result<(), Status>
    {
        if !self.is_current_finder_report()
        {
            return Err(ACCESS_DENIED);
        }
        if self.crash.restart_pending
        {
            return Ok(());
        }
        let binding = self.processes.resource_binding(self.owner)?;
        let permit = crate::recovery::permit()?;
        match crate::toolbox::processes::finder::request(binding, permit)
        {
            Ok(()) =>
            {
                self.crash.restart_pending = true;
                if self.crash.restart_error.take().is_some()
                {
                    self.draw_crash_warnings()?;
                }
                self.crash_controls(false)
            }
            Err(status) => self.finder_restart_failed(status)
        }
    }

    fn retry_finder(&mut self) -> Result<(), Status>
    {
        if self.owner != crate::context::finder_app().0 || self.crash.restart_error.is_none()
        {
            return Err(ACCESS_DENIED);
        }
        let binding = self.processes.resource_binding(self.owner)?;
        match crate::toolbox::processes::finder::retry(&binding)
        {
            Ok(()) =>
            {
                self.crash.restart_pending = true;
                self.crash.restart_error = None;
                self.draw_crash_warnings()?;
                self.crash_controls(false)
            }
            Err(status) => self.finder_restart_failed(status)
        }
    }

    /// A committed Finder is already runnable. This refresh can refuse without
    /// rolling back its admission, restarting activation or reopening a report
    /// which the user has already acknowledged and dismissed.
    pub(crate) fn finish_finder_start_ui(&mut self, admission: &super::FinderAdmission)
                                         -> Result<(), Status>
    {
        if self.owner != admission.id() || self.crash.restart_pending
        {
            return Err(ACCESS_DENIED);
        }
        let window = self.crash.info().window;
        if window.0 == 0
        {
            return Ok(());
        }
        if self.windows.window(window).is_none()
        {
            // Dismissal owns its real ACK wait; do not redraw an invalidated ID.
            return Err(SERVICE_BUSY);
        }
        self.draw_crash_warnings()?;
        self.crash_controls(false)
    }

    pub(crate) fn finder_restart_failed(&mut self, status: Status) -> Result<(), Status>
    {
        let changed = self.crash.restart_pending || self.crash.restart_error != Some(status);
        self.crash.restart_pending = false;
        self.crash.restart_error = Some(status);
        if changed
        {
            serial!("FINDER_RESTART_FAILED app={} status={status}; report retained, retry \
                     available",
                    self.owner);
        }
        if self.crash.info().window.0 == 0
        {
            return self.show_crash_report();
        }
        self.draw_crash_warnings()?;
        self.crash_controls(false)
    }

    fn cancel_crash_tracking(&mut self)
    {
        if let Some(index) = self.crash.tracking.take()
           && let Some(id) = self.crash.controls[index]
        {
            let _ = self.managed_control(&mut mac_abi::controls::ControlCall {
                operation: mac_abi::controls::CANCEL_TRACKING, id, ..Default::default() });
        }
    }

    fn crash_button(&mut self, index: usize) -> Result<(), Status>
    {
        if !self.crash_button_enabled(index)
        {
            return Ok(());
        }
        let delta = match index
        {
            0 => i32::MIN,
            1 => -(self.crash.info().page_lines as i32),
            2 => self.crash.info().page_lines as i32,
            3 => i32::MAX,
            4 =>
            {
                let copied = crate::kernel::with_scrap(|scrap| {
                                 scrap.replace_text(self.crash.report())
                             }).map_err(|error| error.status())
                               .and_then(|result| result);
                let failed = copied.is_err();
                if self.crash.copy_failed != failed
                {
                    self.crash.copy_failed = failed;
                    self.draw_crash_warnings()?;
                }
                if let Err(status) = copied
                {
                    serial!("CRASH_REPORT_COPY_FAILED app={} status={}", self.owner, status);
                    return Err(status);
                }
                serial!("CRASH_REPORT_COPIED app={} bytes={}",
                        self.owner,
                        self.crash.info().report_bytes);
                return Ok(());
            }
            5 =>
            {
                if self.is_current_finder_report()
                {
                    return self.request_finder_restart();
                }
                if self.owner == crate::context::finder_app().0
                   && self.crash.restart_error.is_some()
                {
                    return self.retry_finder();
                }
                return self.crash_request(&mut CrashCall { operation: DISMISS,
                                                           ..Default::default() },
                                          "",
                                          &mut []);
            }
            _ => return Err(BAD_ARGUMENT)
        };
        if self.crash.scroll(delta)
        {
            self.draw_crash_scroll()?;
            self.log_crash_scroll();
        }
        Ok(())
    }

    fn log_crash_scroll(&self)
    {
        let info = self.crash.info();
        serial!("CRASH_REPORT_SCROLL app={} first={} lines={} page_lines={}", self.owner,
                info.first_line, info.line_count, info.page_lines);
    }

    fn crash_pointer(&mut self, point: Point, down: bool, when: u32) -> bool
    {
        use mac_abi::controls::*;
        let window = self.crash.info().window;
        let Some(bounds) = self.windows.window(window).map(|window| window.bounds)
        else
        {
            self.cancel_crash_tracking();
            return false;
        };
        let local = Point { h: point.h.saturating_sub(bounds.left),
                            v: point.v.saturating_sub(bounds.top) };
        if let Some(index) = self.crash.tracking
        {
            let Some(id) = self.crash.controls[index]
            else
            {
                self.crash.tracking = None;
                return true;
            };
            let mut call = ControlCall { operation: POLL_TRACKING,
                                         id,
                                         window: window.0,
                                         point: local,
                                         ticks: u64::from(when),
                                         flags: if down { BUTTON_DOWN } else { 0 },
                                         ..Default::default() };
            if self.managed_control(&mut call).is_err()
            {
                self.cancel_crash_tracking();
                return true;
            }
            if call.flags & TRACKING == 0
            {
                self.crash.tracking = None;
            }
            if call.flags & ACTION != 0
            {
                let _ = self.crash_button(index);
            }
            return true;
        }
        if !down
        {
            return false;
        }
        let hit = self.windows.find_window(point);
        if hit.window != Some(window) || hit.part != WindowPart::Content
        {
            return false;
        }
        let _ = self.window(SELECT_WINDOW, &mut window_request(window));
        for index in 0..self.crash.controls.len()
        {
            let Some(id) = self.crash.controls[index]
            else
            {
                continue;
            };
            let mut call = ControlCall { operation: BEGIN_TRACKING,
                                         id,
                                         window: window.0,
                                         point: local,
                                         ticks: u64::from(when),
                                         ..Default::default() };
            if self.managed_control(&mut call).is_err()
            {
                return true;
            }
            if call.flags & TRACKING != 0
            {
                self.crash.tracking = Some(index);
                break;
            }
        }
        true
    }

    pub(super) fn crash_input(&mut self, input: Input, when: u32) -> bool
    {
        let info = self.crash.info();
        if self.owner != self.foreground
           || info.window.0 == 0
           || info.dismissed != 0
           || self.windows.window(info.window).is_none()
           || self.app_menu.is_tracking()
           || self.menus.is_tracking()
           || self.windows.is_tracking()
        {
            self.cancel_crash_tracking();
            return false;
        }
        match input
        {
            Input::Key { mac_code,
                         character,
                         down,
                         modifiers,
                         .. } if self.windows.active_window() == Some(info.window) =>
            {
                if !down
                {
                    return true;
                }
                let delta = match mac_code
                {
                    0x7e => -1,
                    0x7d => 1,
                    0x74 => -(info.page_lines as i32),
                    0x79 => info.page_lines as i32,
                    0x73 => i32::MIN,
                    0x77 => i32::MAX,
                    _ => 0
                };
                if delta != 0 && self.crash.scroll(delta) && self.draw_crash_scroll().is_ok()
                {
                    self.log_crash_scroll();
                }
                if modifiers & CMD_KEY != 0 && character.eq_ignore_ascii_case(&b'c')
                {
                    let _ = self.crash_button(4);
                }
                if character == b'\r' || character == 3
                {
                    let _ = self.crash_button(5);
                }
                if modifiers & CMD_KEY != 0 && character.eq_ignore_ascii_case(&b'w')
                {
                    let _ = self.crash_button(5);
                }
                true
            }
            Input::Button { point,
                            down } => self.crash_pointer(point, down, when),
            Input::Move(point) if self.crash.tracking.is_some() =>
            {
                self.crash_pointer(point, true, when)
            }
            _ => false
        }
    }
}
