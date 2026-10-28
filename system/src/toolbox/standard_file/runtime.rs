//! Standard File Package UI. System owns managed widgets and asynchronous I/O.
//! File/provider work is advanced only after dropping the Desktop guard.
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{controls, quickdraw::*, scrollbox, standard_file::*, *};
use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use crate::toolbox::{events::input::Input,
                     files::{ResidentReply, ResidentRequest},
                     handles::InvocationGate,
                     standard_file::{self, Action, Query, Reply, Sheet, Widgets, layout::Layout,
                                     view::Text}};

fn checked(status: Status) -> Result<(), Status>
{
    if status == OK { Ok(()) } else { Err(status) }
}
fn layout(mode: u32, sheet: Sheet) -> Result<&'static Layout, Status>
{
    standard_file::templates::layout(mode, sheet)
}
fn label(index: u32) -> Result<&'static str, Status>
{
    standard_file::templates::label(index)
}
fn request(id: WindowId) -> WindowRequest
{
    WindowRequest { id:             id.0,
                    bounds:         Rect::ZERO,
                    title:          [0; WINDOW_TITLE_BYTES],
                    title_length:   0,
                    procedure:      1,
                    minimum_width:  1,
                    minimum_height: 1,
                    reserved:       0 }
}

struct Work
{
    handle:        u64,
    owner:         u64,
    revision:      u64,
    request:       Option<ResidentRequest>,
    completed:     Option<Result<Reply, Status>>,
    icon_prepared: bool,
    abandoned:     bool,
    refusal:       Option<Status>,
    icon:          Option<(files::CatalogInfo, crate::toolbox::quickdraw::icons_service::Drawing)>,
    activity:      Option<crate::recovery::Activity>
}
static CURRENT: Spinlock<Option<(u64, u64)>> = Spinlock::new(None);
static IO: Spinlock<Option<Work>> = Spinlock::new(None);
static PUMPING: AtomicBool = AtomicBool::new(false);
static WORK_READY: AtomicBool = AtomicBool::new(false);
struct Pumping;
impl Drop for Pumping
{
    fn drop(&mut self)
    {
        PUMPING.store(false, Ordering::Release);
    }
}

struct InitializingDialog<'a>
{
    desktop:   &'a mut super::Desktop,
    handle:    u64,
    committed: bool
}
impl Drop for InitializingDialog<'_>
{
    fn drop(&mut self)
    {
        if !self.committed
        {
            if let Err(status) = self.desktop.dispose_file_dialog(self.handle)
            {
                serial!("STANDARD_FILE_ROLLBACK_RETAINED app={} handle={} status={status}",
                        self.desktop.owner, self.handle);
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Damage
{
    rects:  [Rect; 12],
    length: usize,
    full:   bool
}
impl Damage
{
    fn full() -> Self
    {
        Self { rects:  [Rect::ZERO; 12],
               length: 0,
               full:   true }
    }

    fn new() -> Self
    {
        Self { rects:  [Rect::ZERO; 12],
               length: 0,
               full:   false }
    }

    fn add(&mut self, rect: Rect)
    {
        if rect.is_empty() || self.full
        {
            return;
        }
        if self.rects[..self.length].contains(&rect)
        {
            return;
        }
        if self.length == self.rects.len()
        {
            self.full = true;
            return;
        }
        self.rects[self.length] = rect;
        self.length += 1;
    }

    fn touches(&self, rect: Rect) -> bool
    {
        self.full
        || self.rects[..self.length].iter().any(|r| {
                                               r.left < rect.right
                                               && r.right > rect.left
                                               && r.top < rect.bottom
                                               && r.bottom > rect.top
                                           })
    }
}
fn row_bounds(list: Rect, first: usize, index: usize) -> Option<Rect>
{
    let row = index.checked_sub(first)?;
    let top = i32::from(list.top) + 1 + row as i32 * 16;
    if top >= i32::from(list.bottom)
    {
        return None;
    }
    Some(Rect::new(list.left + 1,
                   top as i16,
                   list.right - 16,
                   (top + 16).min(i32::from(list.bottom) - 1) as i16))
}

impl super::Desktop
{
    pub(crate) fn prepare_standard_file_storage(&mut self,
                                                storage: &crate::toolbox::handles::OwnerRef)
                                                -> Result<(), Status>
    {
        self.standard_file.prepare_recovery(storage)?;
        self.scroll_boxes.prepare_recovery(storage, standard_file::templates::raster_budget()?)
    }

    pub fn standard_file_request(&mut self, call: &mut StandardFileCall) -> Result<(), Status>
    {
        if call.reserved != [0; 2]
        {
            return Err(BAD_ARGUMENT);
        }
        if call.operation == DISPOSE
        {
            return self.dispose_file_dialog(call.handle);
        }
        let ledger = self.processes.ledger(self.owner)?;
        let subject = if call.operation == BEGIN
        {
            Some(crate::recovery::standard_file_subject(self.owner)?)
        }
        else
        {
            None
        };
        self.standard_file
            .request(&ledger,
                     &crate::context::recovery_activity().ok_or(WRONG_THREAD)?.storage_owner(),
                     call)?;
        if call.operation == BEGIN
        {
            self.standard_file.get_mut(self.owner, call.handle)?.subject = subject;
            let mut guard = InitializingDialog { desktop:   self,
                                                 handle:    call.handle,
                                                 committed: false };
            guard.desktop.initialize_file_dialog(call)?;
            InvocationGate::run(|| *CURRENT.lock() = Some((guard.desktop.owner, call.handle)));
            WORK_READY.store(true, Ordering::Release);
            guard.committed = true;
        }
        Ok(())
    }

    fn dispose_file_dialog(&mut self, handle: u64) -> Result<(), Status>
    {
        let recipient = self.processes.ledger(self.owner)?;
        self.dispose_file_dialog_for(&recipient, handle)
    }

    /// Cancel the actual System-owned protocol for a stopped recipient without
    /// installing that process as Desktop.owner or restoring its saved focus.
    pub(crate) fn cancel_standard_file_for_recovery(&mut self,
                                                    recipient: &crate::toolbox::handles::OwnerRef)
                                                    -> Result<(), Status>
    {
        let activity = crate::context::recovery_activity().ok_or(ACCESS_DENIED)?;
        let current =
            crate::recovery::active(recipient.owner().incarnation(),
                                    crate::recovery::Kind::StandardFile).ok_or(ACCESS_DENIED)?;
        if activity.recipient().owner() != recipient.owner()
           || !activity.same_session(&current)
           || activity.storage_owner().owner().kind()
              != mac_object_table::domain::DomainKind::System
        {
            return Err(ACCESS_DENIED);
        }
        let Some(handle) = self.standard_file.for_recipient(recipient)
        else
        {
            return Ok(());
        };
        self.dispose_file_dialog_for(recipient, handle)
    }

    fn dispose_file_dialog_for(&mut self, recipient: &crate::toolbox::handles::OwnerRef,
                               handle: u64)
                               -> Result<(), Status>
    {
        let owner = recipient.owner().incarnation();
        let dialog = self.standard_file.take(owner, handle)?;
        // Validation has succeeded and this exact protocol is invalidated.
        // Malformed/foreign Dispose requests above must leave it untouched.
        if let Some(activity) = crate::context::recovery_activity()
        {
            activity.close();
        }
        InvocationGate::run(|| {
            let mut current = CURRENT.lock();
            if *current == Some((owner, handle))
            {
                *current = None;
            }
        });
        let pending = InvocationGate::run(|| {
            let mut work = IO.lock();
            if work.as_ref().is_some_and(|work| work.owner == owner && work.handle == handle)
            {
                work.take()
            }
            else
            {
                None
            }
        });
        // Request::drop abandons and queues the original continuation without
        // a foreign callback or allocation. That node keeps Activity until ACK.
        drop(pending);
        let mut failure = None;
        for window in [dialog.auxiliary, dialog.window]
        {
            if self.windows.window(window).is_some()
            {
                if let Err(status) = checked(self.window(CLOSE_WINDOW, &mut request(window)))
                {
                    failure.get_or_insert(status);
                }
            }
        }
        // Any asynchronous receipt is still retained by Work/Request and its
        // cancellation node; it also pins the modal recovery Activity.
        drop(dialog);
        failure.map_or(Ok(()), Err)
    }

    fn initialize_file_dialog(&mut self, call: &StandardFileCall) -> Result<(), Status>
    {
        let template = layout(call.mode, Sheet::Main)?;
        let top = if call.mode == GET { 84 } else { 80 };
        let width = template.bounds.right - template.bounds.left;
        let height = template.bounds.bottom - template.bounds.top;
        let left = (crate::display::WIDTH as i16 - width) / 2;
        let mut window = request(WindowId(0));
        window.bounds = Rect::new(left, top, left + width, top + height);
        window.procedure = template.procedure;
        checked(self.window(NEW_WINDOW, &mut window))?;
        let dialog = self.standard_file.get_mut(self.owner, call.handle)?;
        dialog.window = WindowId(window.id);
        let fonts = crate::kernel::fonts().ok_or(NOT_SUPPORTED)?;
        dialog.editor.set_font(FontSelection::default(), fonts)?;
        let mut characters = Text::default();
        for byte in 32..=255
        {
            characters.push(mac_formats::text::character(byte).encode_utf8(&mut [0; 4]))?;
        }
        dialog.editor.prepare_font_input(characters.text()?, fonts)?;
        checked(self.window(SET_GROW_ICON_CLIP, &mut request(WindowId(window.id))))?;
        checked(self.window(SELECT_WINDOW, &mut window))?;
        self.sf_create_widgets(call.handle, Sheet::Main)?;
        self.standard_file_draw(call.handle, Damage::full())?;
        serial!("STANDARD_FILE_BEGIN app={} handle={} mode={} window={}", self.owner, call.handle,
                call.mode, window.id);
        Ok(())
    }

    fn sf_create_widgets(&mut self, id: u64, sheet: Sheet) -> Result<(), Status>
    {
        let dialog = self.standard_file.get(self.owner, id)?;
        let mode = dialog.mode;
        let template = layout(mode, sheet)?;
        let window = if sheet == Sheet::Main { dialog.window } else { dialog.auxiliary };
        let mut widgets = Widgets::default();
        for (index, item) in template.items.iter().enumerate()
        {
            if item.kind & 0x7f != 4 && !(sheet == Sheet::Main && mode == PUT && index == 11)
            {
                continue;
            }
            let text =
                if sheet == Sheet::Main && index == 11 { label(1)? } else { item.label.as_str() };
            let mut standard = definitions::ControlDefinitionState::new(item.bounds, text);
            if index == 0
            {
                standard.bounds.inset(-4, -4);
                standard.flags = definitions::DEFAULT_BUTTON;
            }
            standard.enabled = u32::from(!(sheet == Sheet::Main && index == 4));
            let mut call = controls::ControlCall { operation: controls::CREATE_STANDARD,
                                                   window: window.0,
                                                   standard,
                                                   ..Default::default() };
            self.managed_control(&mut call)?;
            widgets.buttons[index] = call.id;
        }
        if sheet == Sheet::Main
        {
            let list = template.item(7);
            let mut call = scrollbox::Call { operation: scrollbox::CREATE,
                                             window: window.0,
                                             spec: scrollbox::Spec { viewport:
                                                                         Rect::new(list.left + 1,
                                                                                   list.top + 1,
                                                                                   list.right - 16,
                                                                                   list.bottom - 1),
                                                                     scrollbar:
                                                                         Rect::new(list.right - 16,
                                                                                   list.top,
                                                                                   list.right,
                                                                                   list.bottom),
                                                                     padding: Point { h: 21,
                                                                                      v: 13 },
                                                                     row_height: 16,
                                                                     ..Default::default() },
                                             ..Default::default() };
            self.scroll_box(&mut call)?;
            widgets.scrollbox = call.id;
            self.standard_file.get_mut(self.owner, id)?.main_widgets = widgets;
        }
        else
        {
            self.standard_file.get_mut(self.owner, id)?.auxiliary_widgets = widgets;
        }
        Ok(())
    }

    fn sf_shape(&mut self, window: u64, bounds: Rect, verb: u32) -> Result<(), Status>
    {
        self.graphics(&mut GraphicsCall { operation: QD_SHAPE,
                                          target: Target::window(WindowId(window)),
                                          bounds,
                                          shape: SHAPE_RECT,
                                          verb,
                                          ..Default::default() },
                      &[],
                      &[])
    }

    fn sf_text(&mut self, window: u64, x: i16, y: i16, text: &str) -> Result<(), Status>
    {
        self.font_draw(&mut FontCall { target: Target::window(WindowId(window)),
                                       selection: FontSelection::default(),
                                       baseline: Point { h: x,
                                                         v: y },
                                       ..Default::default() },
                       text,
                       crate::kernel::fonts().ok_or(NOT_SUPPORTED)?)
    }

    fn sf_button(&mut self, id: u64, index: usize, enabled: bool) -> Result<(), Status>
    {
        let dialog = self.standard_file.get(self.owner, id)?;
        let widgets = if dialog.sheet == Sheet::Main
        {
            &dialog.main_widgets
        }
        else
        {
            &dialog.auxiliary_widgets
        };
        let control = *widgets.buttons.get(index).ok_or(BAD_ARGUMENT)?;
        if control == 0
        {
            return Ok(());
        }
        let mut call = controls::ControlCall { operation: controls::INFO_OBJECT,
                                               id: control,
                                               ..Default::default() };
        self.managed_control(&mut call)?;
        if call.standard.enabled != u32::from(enabled)
        {
            call.operation = controls::CONFIGURE_OBJECT;
            call.standard.enabled = u32::from(enabled);
            self.managed_control(&mut call)?;
        }
        call.operation = controls::DRAW_OBJECT;
        self.managed_control(&mut call)
    }

    fn sf_wrapped_text(&mut self, window: u64, bounds: Rect, text: &str) -> Result<(), Status>
    {
        let fonts = crate::kernel::fonts().ok_or(NOT_SUPPORTED)?;
        let font = crate::toolbox::quickdraw::fonts_service::selection(FontSelection::default());
        let metrics = fonts.metrics(font)?;
        let mut baseline = bounds.top + metrics.ascent;
        let advance = (metrics.ascent + metrics.descent + metrics.leading).max(1);
        crate::toolbox::dialogs::wrapped_lines(text,
                                               bounds.right - bounds.left,
                                               |s| fonts.text_width(font, s),
                                               |line, _| {
                                                   if baseline <= bounds.bottom
                                                   {
                                                       self.sf_text(window, bounds.left,
                                                                    baseline, line)?;
                                                   }
                                                   baseline = baseline.saturating_add(advance);
                                                   Ok(())
                                               })
    }

    fn sf_sync_sheet(&mut self, id: u64) -> Result<bool, Status>
    {
        let dialog = self.standard_file.get(self.owner, id)?;
        let sheet = dialog.sheet;
        let mode = dialog.mode;
        let previous = dialog.auxiliary_sheet;
        let old = dialog.auxiliary;
        let main = dialog.window;
        if sheet == previous
        {
            return Ok(false);
        }
        if self.windows.window(old).is_some()
        {
            let closed = checked(self.window(CLOSE_WINDOW, &mut request(old)));
            let dialog = self.standard_file.get_mut(self.owner, id)?;
            dialog.auxiliary = WindowId(0);
            dialog.auxiliary_widgets = Widgets::default();
            closed?;
        }
        let mut auxiliary = WindowId(0);
        if sheet != Sheet::Main
        {
            let template = layout(mode, sheet)?;
            let width = template.bounds.right - template.bounds.left;
            let height = template.bounds.bottom - template.bounds.top;
            let mut window = request(WindowId(0));
            let left = (crate::display::WIDTH as i16 - width) / 2;
            window.bounds = Rect::new(left, 100, left + width, 100 + height);
            window.procedure = template.procedure;
            checked(self.window(NEW_WINDOW, &mut window))?;
            auxiliary = WindowId(window.id);
            // Publish the owned child to the dialog before any later fallible
            // setup, so cancellation retains its genuine cleanup path.
            self.standard_file.get_mut(self.owner, id)?.auxiliary = auxiliary;
            checked(self.window(SET_GROW_ICON_CLIP, &mut request(auxiliary)))?;
            checked(self.window(SELECT_WINDOW, &mut window))?;
            let dialog = self.standard_file.get_mut(self.owner, id)?;
            if sheet == Sheet::NewFolder
            {
                let text = template.items.get(2).ok_or(BAD_ARGUMENT)?.label.as_str();
                dialog.editor.set_text(text)?;
                dialog.editor.select(0, text.len() as u32)?;
            }
            self.sf_create_widgets(id, sheet)?;
        }
        else
        {
            checked(self.window(SELECT_WINDOW, &mut request(main)))?;
        }
        let dialog = self.standard_file.get_mut(self.owner, id)?;
        dialog.auxiliary = auxiliary;
        dialog.auxiliary_sheet = sheet;
        let view = if sheet == Sheet::NewFolder
        {
            layout(mode, sheet)?.item(3)
        }
        else
        {
            Rect::new(15, 157, 227, 173)
        };
        if dialog.editor.info().view != view
        {
            dialog.editor.set_rects(Rect::new(view.left,
                                               view.top,
                                               view.left + 4096,
                                               view.bottom),
                                     view)?;
        }
        Ok(true)
    }

    fn standard_file_draw(&mut self, id: u64, damage: Damage) -> Result<(), Status>
    {
        if !damage.full && damage.length == 0
        {
            return Ok(());
        }
        let dialog = self.standard_file.get(self.owner, id)?;
        let sheet = dialog.sheet;
        let window = if sheet == Sheet::Main { dialog.window } else { dialog.auxiliary };
        let mode = dialog.mode;
        let template = layout(mode, sheet)?;
        let first = dialog.first;
        let selected = dialog.selected;
        let mut rows = [files::CatalogInfo::default(); standard_file::page::ROWS];
        let count = dialog.entries.length;
        rows[..count].copy_from_slice(&dialog.entries.rows[..count]);
        let total = dialog.entries.count();
        let prompt =
            if dialog.prompt.text()?.is_empty() { Text::new(label(2)?)? } else { dialog.prompt };
        let name = Text::new(dialog.name())?;
        let title = Text::new(dialog.ancestors.first().map_or(label(0)?, |entry| entry.name()))?;
        let volume =
            Text::new(if dialog.location.0 == 0 { "" } else { dialog.volume_info.name() })?;
        let error = dialog.error;
        let location = dialog.location;
        let focus = dialog.focus_name;
        let editor = dialog.editor.info();
        let caret = dialog.editor.caret_rect();
        let mut selection = [Rect::ZERO; 2];
        let mut selection_count = 0;
        dialog.editor.selection_rects(|rect| {
                         if selection_count < selection.len()
                         {
                             selection[selection_count] = rect;
                             selection_count += 1;
                         }
                     });
        let popup = dialog.popup;
        let mut ancestors = [Text::default(); standard_file::page::ANCESTORS];
        let ancestor_count = dialog.ancestors.len();
        for (index, entry) in dialog.ancestors.iter().enumerate()
        {
            ancestors[index] = Text::new(entry.name())?;
        }
        let can_accept = dialog.can_accept();
        let scrollbox = dialog.main_widgets.scrollbox;
        let mut icons = core::array::from_fn::<_, { standard_file::page::ROWS }, _>(|_| None);
        for (index, entry) in rows[..count].iter().enumerate()
        {
            icons[index] = dialog.icons
                                 .iter()
                                 .find(|(key, _)| key == entry)
                                 .map(|(_, drawing)| drawing.clone());
        }
        if damage.full
        {
            checked(self.window(BEGIN_UPDATE, &mut request(window)))?;
        }
        else
        {
            let mut bytes = [0u8; 96];
            for (index, rect) in damage.rects[..damage.length].iter().enumerate()
            {
                for (field, value) in
                    [rect.left, rect.top, rect.right, rect.bottom].into_iter().enumerate()
                {
                    bytes[index*8+field*2..index*8+field*2+2].copy_from_slice(&value.to_le_bytes());
                }
            }
            self.graphics(&mut GraphicsCall { operation: QD_WINDOW_BEGIN_RECTS,
                                              target: Target::window(window),
                                              ..Default::default() },
                          &[],
                          &bytes[..damage.length * 8])?;
        }
        let result = (|| {
            self.graphics(&mut GraphicsCall { operation: QD_RESET,
                                              target: Target::window(window),
                                              ..Default::default() },
                          &[],
                          &[])?;
            let bounds = Rect::new(0,
                                   0,
                                   template.bounds.right - template.bounds.left,
                                   template.bounds.bottom - template.bounds.top);
            self.sf_shape(window.0, bounds, VERB_ERASE)?;
            if sheet != Sheet::Main
            {
                let mut message = Text::default();
                match sheet
                {
                    Sheet::Replace =>
                    {
                        let source = &template.items.get(2).ok_or(BAD_ARGUMENT)?.label;
                        let mut parts = source.split("^0");
                        if let Some(part) = parts.next()
                        {
                            message.push(part)?;
                        }
                        for part in parts
                        {
                            message.push(name.text()?)?;
                            message.push(part)?;
                        }
                    }
                    Sheet::NewFolder =>
                    {
                        message = Text::new(&template.items.get(3).ok_or(BAD_ARGUMENT)?.label)?
                    }
                    Sheet::Error =>
                    {
                        message = Text::new(label(match error
                                                  {
                                                      BAD_ARGUMENT => 3,
                                                      NOT_FOUND => 4,
                                                      NOT_SUPPORTED => 5,
                                                      NO_MEMORY => 6,
                                                      SERVICE_BUSY => 7,
                                                      IO_ERROR => 8,
                                                      READ_ONLY => 9,
                                                      ALREADY_EXISTS => 10,
                                                      ACCESS_DENIED => 11,
                                                      DISK_FULL => 12,
                                                      ROLLBACK_FAILED => 13,
                                                      _ => 14
                                                  })?)?
                    }
                    Sheet::Main => return Err(BAD_ABI)
                }
                self.sf_wrapped_text(window.0,
                                     template.item(if sheet == Sheet::NewFolder
                                                   {
                                                       4
                                                   }
                                                   else if sheet == Sheet::Replace
                                                   {
                                                       3
                                                   }
                                                   else
                                                   {
                                                       2
                                                   }),
                                     message.text()?)?;
                if sheet == Sheet::NewFolder
                {
                    let edit = template.item(3);
                    self.sf_shape(window.0,
                                  Rect::new(edit.left - 3,
                                            edit.top - 3,
                                            edit.right + 3,
                                            edit.bottom + 3),
                                  VERB_FRAME)?;
                    self.sf_text(window.0,
                                 editor.dest.left,
                                 editor.dest.top + editor.ascent,
                                 name.text()?)?;
                    if focus
                    {
                        for rect in selection[..selection_count].iter().copied()
                        {
                            self.sf_shape(window.0, rect, VERB_INVERT)?;
                        }
                        self.sf_shape(window.0, caret, VERB_INVERT)?;
                    }
                }
                for (index, item) in template.items.iter().enumerate()
                {
                    if item.kind & 0x7f == 4 && damage.touches(item.bounds)
                    {
                        self.sf_button(id, index, true)?;
                    }
                }
                return Ok(());
            }
            for (index, item) in template.items.iter().enumerate()
            {
                if item.kind & 0x7f == 4 && damage.touches(item.bounds)
                {
                    self.sf_button(id, index, index != 4 && (index != 0 || can_accept))?;
                }
            }
            let pop = template.item(8);
            if damage.touches(pop)
            {
                self.sf_shape(window.0, pop, VERB_FRAME)?;
                self.sf_text(window.0, pop.left + 8, pop.top + 14, title.text()?)?;
                let points = [Point { h: pop.right - 14,
                                      v: pop.top + 7 },
                              Point { h: pop.right - 6,
                                      v: pop.top + 7 },
                              Point { h: pop.right - 10,
                                      v: pop.top + 11 }];
                self.graphics(&mut GraphicsCall { operation: QD_SHAPE,
                                                  target: Target::window(window),
                                                  shape: SHAPE_POLYGON,
                                                  verb: VERB_PAINT,
                                                  points: points.as_ptr(),
                                                  point_count: points.len(),
                                                  ..Default::default() },
                              &points,
                              &[])?;
            }
            if damage.touches(Rect::new(238, 0, 344, 30))
            {
                self.sf_text(window.0, 239, 21, volume.text()?)?;
            }
            let list = template.item(7);
            if damage.touches(list)
            {
                self.sf_shape(window.0, list, VERB_FRAME)?;
                let mut info = scrollbox::Call { operation: scrollbox::INFO,
                                                 id: scrollbox,
                                                 ..Default::default() };
                self.scroll_box(&mut info)?;
                if info.spec.count != total as u32
                {
                    info.operation = scrollbox::CONFIGURE;
                    info.spec.count = total as u32;
                    self.scroll_box(&mut info)?;
                }
                if info.top != first as u32
                {
                    info.operation = scrollbox::SCROLL_TO;
                    info.top = first as u32;
                    self.scroll_box(&mut info)?;
                }
                let fonts = crate::kernel::fonts().ok_or(NOT_SUPPORTED)?;
                let colors = info.spec;
                for (index, entry) in rows[..count].iter().enumerate()
                {
                    let global = first + index;
                    let Some(bounds) = row_bounds(list, first, global)
                    else
                    {
                        continue;
                    };
                    if !damage.touches(bounds)
                    {
                        continue;
                    }
                    let selected = selected == Some(global);
                    let icon = match icons[index].clone()
                    {
                        Some(icon) => icon,
                        None => standard_file::templates::fallback(entry)?
                    };
                    self.scroll_boxes.set_row_with(scrollbox, self.owner, global as u32, |canvas| {
                        if selected
                        {
                            canvas.paint_rect(Rect::new(0,0,list.right-list.left-17,16), colors.selected_background);
                        }
                        let mut icon=icon;
                        icon.destination=Rect::new(1,0,17,16);
                        icon.port=Default::default();
                        icon.paint(canvas)?;
                        fonts.draw(crate::toolbox::quickdraw::fonts_service::selection(FontSelection::default()), canvas,
                                   Point{h:20,v:13},entry.name(),
                                   if selected {colors.selected_foreground} else {colors.foreground})?;
                        Ok(())
                    })?;
                }
                // Blank loading tiles let the standard scrollbar track at
                // input speed while the checked scan supplies a requested page.
                let mut missing = scrollbox::Call { operation: scrollbox::INFO,
                                                    id: scrollbox,
                                                    ..Default::default() };
                self.scroll_box(&mut missing)?;
                for row in missing.first..missing.end
                {
                    self.scroll_boxes.set_row_with(scrollbox, self.owner, row, |_| Ok(()))?;
                }
                let mut draw = scrollbox::Call { operation: scrollbox::DRAW,
                                                 id: scrollbox,
                                                 ..Default::default() };
                self.scroll_box(&mut draw)?;
            }
            if damage.touches(template.item(9))
            {
                self.sf_shape(window.0, template.item(9), VERB_PAINT)?;
            }
            if mode == PUT
            {
                if damage.touches(template.item(12))
                {
                    self.sf_button(id, 11, location.0 != 0)?;
                }
                let edit = template.item(10);
                if damage.touches(Rect::new(edit.left - 3,
                                            edit.top - 3,
                                            edit.right + 3,
                                            edit.bottom + 3))
                {
                    self.sf_text(window.0,
                                 template.item(11).left,
                                 template.item(11).top + 13,
                                 prompt.text()?)?;
                    self.sf_shape(window.0,
                                  Rect::new(edit.left - 3,
                                            edit.top - 3,
                                            edit.right + 3,
                                            edit.bottom + 3),
                                  VERB_FRAME)?;
                    self.graphics(&mut GraphicsCall { operation: QD_CLIP_RECT,
                                                      target: Target::window(window),
                                                      bounds: edit,
                                                      ..Default::default() },
                                  &[],
                                  &[])?;
                    self.sf_text(window.0,
                                 editor.dest.left,
                                 editor.dest.top + editor.ascent,
                                 name.text()?)?;
                    if focus
                    {
                        for rect in selection[..selection_count].iter().copied()
                        {
                            self.sf_shape(window.0, rect, VERB_INVERT)?;
                        }
                        self.sf_shape(window.0, caret, VERB_INVERT)?;
                    }
                    self.graphics(&mut GraphicsCall { operation: QD_CLIP_RECT,
                                                      target: Target::window(window),
                                                      bounds,
                                                      ..Default::default() },
                                  &[],
                                  &[])?;
                }
            }
            if popup
            {
                let height = (ancestor_count + 1) as i16 * 18;
                let area = Rect::new(pop.left, pop.top, pop.right, pop.top + height);
                if damage.touches(area)
                {
                    self.sf_shape(window.0, area, VERB_ERASE)?;
                    self.sf_shape(window.0, area, VERB_FRAME)?;
                    for (index, title) in ancestors[..ancestor_count].iter().enumerate()
                    {
                        self.sf_text(window.0,
                                     pop.left + 8,
                                     pop.top + 14 + index as i16 * 18,
                                     title.text()?)?;
                    }
                    self.sf_text(window.0,
                                 pop.left + 8,
                                 pop.top + 14 + ancestor_count as i16 * 18,
                                 label(0)?)?;
                }
            }
            Ok(())
        })();
        let finish = if result.is_ok()
        {
            checked(self.window(END_UPDATE, &mut request(window)))
        }
        else
        {
            self.cancel_system_window_update(window)
        };
        if let (Err(original), Err(cleanup)) = (result, finish)
        {
            serial!("STANDARD_FILE_PAINT_CANCEL_REFUSED window={} original={original} \
                     cleanup={cleanup}",
                    window.0);
        }
        result?;
        finish
    }

    fn sf_action(&mut self, id: u64, action: Action, when: u32)
    {
        let before = match self.standard_file.get(self.owner, id)
        {
            Ok(d) =>
            {
                (d.selected,
                 d.first,
                 d.sheet,
                 d.revision,
                 Text::new(d.name()),
                 d.editor.info(),
                 d.can_accept())
            }
            Err(_) => return
        };
        let when = if matches!(action, Action::Accept | Action::Confirm)
        {
            crate::machine::clock::now().ok().and_then(|t| t.mac_seconds()).unwrap_or(0)
        }
        else
        {
            when
        };
        if let Err(cause) = self.standard_file.action(self.owner, id, action, when)
        {
            if let Ok(dialog) = self.standard_file.get_mut(self.owner, id)
            {
                dialog.error = cause;
                dialog.sheet = Sheet::Error;
            }
        }
        if self.standard_file.get(self.owner, id).is_ok_and(|dialog| dialog.work_pending())
        {
            WORK_READY.store(true, Ordering::Release);
        }
        let sheet_changed = match self.sf_sync_sheet(id)
        {
            Ok(changed) => changed,
            Err(status) =>
            {
                serial!("STANDARD_FILE_FAILED app={} status={status}", self.owner);
                return;
            }
        };
        let Ok(dialog) = self.standard_file.get(self.owner, id)
        else
        {
            return;
        };
        if dialog.state != PENDING
        {
            serial!("STANDARD_FILE_DONE app={} state={} volume={} parent={} cnid={} replacing={} \
                     name={}",
                    dialog.owner,
                    dialog.state,
                    dialog.result.volume,
                    dialog.result.parent,
                    dialog.result.id,
                    u32::from(dialog.replacing),
                    dialog.result.name());
        }
        let template = match layout(dialog.mode, dialog.sheet)
        {
            Ok(l) => l,
            Err(_) => return
        };
        let mut damage = Damage::new();
        if sheet_changed || before.2 != dialog.sheet
        {
            damage = Damage::full();
        }
        else
        {
            if before.3 != dialog.revision
            {
                damage.add(template.item(7));
            }
            if before.6 != dialog.can_accept()
            {
                let mut bounds = template.item(1);
                bounds.inset(-4, -4);
                damage.add(bounds);
            }
            if before.1 != dialog.first
            {
                damage.add(template.item(7));
            }
            else if before.0 != dialog.selected
            {
                for index in [before.0, dialog.selected].into_iter().flatten()
                {
                    if let Some(rect) = row_bounds(template.item(7), dialog.first, index)
                    {
                        damage.add(rect);
                    }
                }
            }
            let changed_name = before.4
                                     .as_ref()
                                     .ok()
                                     .and_then(|text| text.text().ok())
                                     .is_none_or(|text| text != dialog.name());
            if changed_name
               || before.5 != dialog.editor.info()
               || matches!(action, Action::ToggleFocus | Action::SelectName)
            {
                let mut edit = template.item(if dialog.sheet == Sheet::NewFolder { 3 } else { 10 });
                edit.inset(-3, -3);
                damage.add(edit);
                damage.add(template.item(1));
            }
        }
        if let Err(status) = self.standard_file_draw(id, damage)
        {
            serial!("STANDARD_FILE_FAILED app={} status={status}", self.owner);
        }
    }

    fn sf_widgets(&self, id: u64) -> Result<&Widgets, Status>
    {
        let d = self.standard_file.get(self.owner, id)?;
        Ok(if d.sheet == Sheet::Main { &d.main_widgets } else { &d.auxiliary_widgets })
    }

    fn sf_widgets_mut(&mut self, id: u64) -> Result<&mut Widgets, Status>
    {
        let d = self.standard_file.get_mut(self.owner, id)?;
        Ok(if d.sheet == Sheet::Main { &mut d.main_widgets } else { &mut d.auxiliary_widgets })
    }

    fn sf_track_button(&mut self, id: u64, point: Point, down: bool, begin: bool, when: u32)
                       -> Result<bool, Status>
    {
        let tracking = self.sf_widgets(id)?.tracking;
        if tracking.is_none() && !begin
        {
            return Ok(false);
        }
        let buttons = self.sf_widgets(id)?.buttons;
        for (index, control) in buttons.into_iter().enumerate().filter(|(_, id)| *id != 0)
        {
            if tracking.is_some_and(|active| active != index)
            {
                continue;
            }
            if tracking.is_none() && !down
            {
                continue;
            }
            let mut call =
                controls::ControlCall { operation: if tracking.is_some()
                                        {
                                            controls::POLL_TRACKING
                                        }
                                        else
                                        {
                                            controls::BEGIN_TRACKING
                                        },
                                        id: control,
                                        point,
                                        ticks: u64::from(when),
                                        flags: if down { controls::BUTTON_DOWN } else { 0 },
                                        ..Default::default() };
            self.managed_control(&mut call)?;
            if call.flags & controls::HANDLED != 0
            {
                self.sf_widgets_mut(id)?.tracking =
                    if call.flags & controls::TRACKING != 0 { Some(index) } else { None };
                if call.flags & controls::ACTION != 0
                {
                    let action = match index
                    {
                        0 => Action::Accept,
                        1 => Action::Cancel,
                        5 => Action::Desktop,
                        11 => Action::NewFolder,
                        _ => return Ok(true)
                    };
                    self.sf_action(id, action, when);
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn sf_track_scroll(&mut self, id: u64, point: Point, down: bool, begin: bool, when: u32)
                       -> Result<bool, Status>
    {
        let dialog = self.standard_file.get(self.owner, id)?;
        if dialog.sheet != Sheet::Main
        {
            return Ok(false);
        }
        if !begin && !dialog.main_widgets.scroll_tracking
        {
            return Ok(false);
        }
        let control = dialog.main_widgets.scrollbox;
        if control == 0
        {
            return Ok(false);
        }
        let mut call = scrollbox::Call { operation: if begin
                                         {
                                             scrollbox::TRACK_BEGIN
                                         }
                                         else
                                         {
                                             scrollbox::TRACK_POLL
                                         },
                                         id: control,
                                         point,
                                         ticks: u64::from(when),
                                         flags: if down { scrollbox::BUTTON_DOWN } else { 0 },
                                         ..Default::default() };
        self.scroll_box(&mut call)?;
        self.standard_file.get_mut(self.owner, id)?.main_widgets.scroll_tracking =
            call.flags & scrollbox::TRACKING != 0;
        if call.flags & scrollbox::TRACKING != 0
        {
            WORK_READY.store(true, Ordering::Release);
        }
        let handled = call.flags & scrollbox::TRACKING != 0 || call.part != 0;
        if call.flags & scrollbox::CHANGED != 0
        {
            self.sf_action(id, Action::Scroll(call.top as usize), when);
        }
        if call.flags & scrollbox::CHANGED != 0 || begin && handled
        {
            let template = layout(self.standard_file.get(self.owner, id)?.mode, Sheet::Main)?;
            let list = template.item(7);
            let mut damage = Damage::new();
            damage.add(Rect::new(list.right - 16, list.top, list.right, list.bottom));
            self.standard_file_draw(id, damage)?;
        }
        Ok(handled)
    }

    pub(super) fn standard_file_input(&mut self, input: Input, when: u32) -> bool
    {
        if self.owner != self.foreground
        {
            return false;
        }
        let Some(id) = self.standard_file.active(self.owner)
        else
        {
            return false;
        };
        let Ok(dialog) = self.standard_file.get(self.owner, id)
        else
        {
            return true;
        };
        let sheet = dialog.sheet;
        let mode = dialog.mode;
        let focus = dialog.focus_name;
        let window = if sheet == Sheet::Main { dialog.window } else { dialog.auxiliary };
        let Ok(template) = layout(mode, sheet)
        else
        {
            return true;
        };
        match input
        {
            Input::Key { mac_code,
                         character,
                         down,
                         modifiers,
                         .. } =>
            {
                if !down
                {
                    return true;
                }
                let cmd = modifiers & CMD_KEY != 0;
                let shift = modifiers & SHIFT_KEY != 0;
                use textedit::CursorMovement as M;
                let action = match mac_code
                {
                    0x35 => Some(Action::Cancel),
                    0x24 | 0x4c => Some(Action::Accept),
                    0x30 => Some(Action::ToggleFocus),
                    0x33 => Some(Action::Backspace),
                    0x7e if cmd => Some(Action::Parent),
                    0x7d if cmd => Some(Action::Accept),
                    0x7b if cmd => Some(Action::Volume(-1)),
                    0x7c if cmd => Some(Action::Volume(1)),
                    0x7e =>
                    {
                        Some(if focus
                             {
                                 Action::Navigate(M::DocumentStart, shift)
                             }
                             else
                             {
                                 Action::Previous(-1)
                             })
                    }
                    0x7d =>
                    {
                        Some(if focus
                             {
                                 Action::Navigate(M::DocumentEnd, shift)
                             }
                             else
                             {
                                 Action::Previous(1)
                             })
                    }
                    0x7b => Some(Action::Navigate(M::Left, shift)),
                    0x7c => Some(Action::Navigate(M::Right, shift)),
                    0x73 =>
                    {
                        Some(if focus
                             {
                                 Action::Navigate(M::LineStart, shift)
                             }
                             else
                             {
                                 Action::Home
                             })
                    }
                    0x77 =>
                    {
                        Some(if focus { Action::Navigate(M::LineEnd, shift) } else { Action::End })
                    }
                    0x74 => Some(Action::Page(-1)),
                    0x79 => Some(Action::Page(1)),
                    _ if cmd && character == b'.' => Some(Action::Cancel),
                    _ if cmd && character.eq_ignore_ascii_case(&b'a') => Some(Action::SelectName),
                    _ if !cmd && character >= 32 && character != 127 =>
                    {
                        Some(Action::Edit(mac_formats::text::character(character)))
                    }
                    _ => None
                };
                if let Some(action) = action
                {
                    self.sf_action(id, action, when);
                }
            }
            Input::Button { point,
                            down } =>
            {
                let Some(window) = self.windows.window(window)
                else
                {
                    return true;
                };
                let point = Point { h: point.h - window.bounds.left,
                                    v: point.v - window.bounds.top };
                if let Ok(dialog) = self.standard_file.get_mut(self.owner, id)
                {
                    dialog.name_tracking = false;
                }
                match self.sf_track_button(id, point, down, down, when)
                {
                    Ok(true) => return true,
                    Err(error) =>
                    {
                        serial!("STANDARD_FILE_TRACK_FAILED app={} status={error}", self.owner);
                        return true;
                    }
                    _ => ()
                }
                match self.sf_track_scroll(id, point, down, down, when)
                {
                    Ok(true) => return true,
                    Err(error) =>
                    {
                        serial!("STANDARD_FILE_SCROLL_FAILED app={} status={error}", self.owner);
                        return true;
                    }
                    _ => ()
                }
                if !down
                {
                    return true;
                }
                if sheet == Sheet::NewFolder && template.item(3).contains(point)
                {
                    if let Ok(dialog) = self.standard_file.get_mut(self.owner, id)
                    {
                        dialog.name_tracking = true;
                        let _ = dialog.editor.click(point, self.events.modifiers & SHIFT_KEY != 0);
                    }
                    let mut damage = Damage::new();
                    damage.add(template.item(3));
                    let _ = self.standard_file_draw(id, damage);
                    return true;
                }
                if sheet == Sheet::Main
                {
                    let Ok(dialog) = self.standard_file.get(self.owner, id)
                    else
                    {
                        return true;
                    };
                    if dialog.popup
                    {
                        let row = ((point.v - template.item(8).top) / 18).max(0) as usize;
                        let destination = dialog.ancestors
                                                .get(row)
                                                .map(|entry| (entry.volume, entry.id))
                                                .unwrap_or((0, 0));
                        WORK_READY.store(true, Ordering::Release);
                        if let Err(error) =
                            self.standard_file.navigate_to(self.owner, id, destination)
                        {
                            serial!("STANDARD_FILE_NAVIGATION_FAILED status={error}");
                        }
                        let _ = self.standard_file_draw(id, Damage::full());
                        return true;
                    }
                    if template.item(8).contains(point)
                    {
                        if let Ok(dialog) = self.standard_file.get_mut(self.owner, id)
                        {
                            dialog.popup = true;
                        }
                        let mut damage = Damage::new();
                        let pop = template.item(8);
                        damage.add(Rect::new(pop.left, pop.top, pop.right, pop.top + 12 * 18));
                        let _ = self.standard_file_draw(id, damage);
                        return true;
                    }
                    if template.item(7).contains(point)
                    {
                        if !dialog.entries.ready(dialog.page())
                        {
                            return true;
                        }
                        let control = dialog.main_widgets.scrollbox;
                        let mut call = scrollbox::Call { operation: scrollbox::ROW_AT,
                                                         id: control,
                                                         point,
                                                         ..Default::default() };
                        if self.scroll_box(&mut call).is_ok() && call.row != u32::MAX
                        {
                            let index = call.row as usize;
                            let double =
                                self.standard_file
                                    .get(self.owner, id)
                                    .ok()
                                    .and_then(|d| d.last_click)
                                    .is_some_and(|(i, t)| i == index && when.wrapping_sub(t) <= 30);
                            self.sf_action(id,
                                           if double
                                           {
                                               Action::Open(index)
                                           }
                                           else
                                           {
                                               Action::Select(index)
                                           },
                                           when);
                            if let Ok(dialog) = self.standard_file.get_mut(self.owner, id)
                            {
                                dialog.last_click = if double { None } else { Some((index, when)) };
                            }
                        }
                        return true;
                    }
                    if mode == PUT && template.item(10).contains(point)
                    {
                        if let Ok(dialog) = self.standard_file.get_mut(self.owner, id)
                        {
                            dialog.focus_name = true;
                            dialog.name_tracking = true;
                            dialog.selected = None;
                            let _ =
                                dialog.editor.click(point, self.events.modifiers & SHIFT_KEY != 0);
                        }
                        let mut damage = Damage::new();
                        damage.add(template.item(10));
                        let _ = self.standard_file_draw(id, damage);
                        return true;
                    }
                    if template.item(4).contains(point)
                    {
                        self.sf_action(id, Action::Parent, when);
                    }
                }
            }
            Input::Move(point) =>
            {
                let Some(window) = self.windows.window(window)
                else
                {
                    return true;
                };
                let point = Point { h: point.h - window.bounds.left,
                                    v: point.v - window.bounds.top };
                let down = self.events.modifiers & crate::toolbox::events::BTN_STATE == 0;
                if let Ok(dialog) = self.standard_file.get_mut(self.owner, id)
                {
                    if dialog.name_tracking
                    {
                        if !down
                        {
                            dialog.name_tracking = false;
                            return true;
                        }
                        let before = dialog.editor.info();
                        let changed = dialog.editor
                                            .click(point, true)
                                            .map(|()| before != dialog.editor.info());
                        if changed == Ok(true)
                        {
                            let mut damage = Damage::new();
                            damage.add(template.item(if sheet == Sheet::NewFolder
                                                     {
                                                         3
                                                     }
                                                     else
                                                     {
                                                         10
                                                     }));
                            let _ = self.standard_file_draw(id, damage);
                        }
                        return true;
                    }
                }
                if let Err(error) = self.sf_track_button(id, point, down, false, when)
                {
                    serial!("STANDARD_FILE_TRACK_FAILED status={error}");
                }
                if let Err(error) = self.sf_track_scroll(id, point, down, false, when)
                {
                    serial!("STANDARD_FILE_SCROLL_FAILED status={error}");
                }
            }
        }
        true
    }

    /// Called from guard-free kernel maintenance after physical input. One real
    /// provider transition or one bounded catalog step is advanced per sample.
    pub(crate) fn pump_standard_file()
    {
        if !WORK_READY.load(Ordering::Acquire) || PUMPING.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let _pumping = Pumping;
        WORK_READY.store(false, Ordering::Release);
        let current = InvocationGate::run(|| *CURRENT.lock());
        let mut work = InvocationGate::run(|| IO.lock().take());
        if let Some(active) = work.as_mut()
        {
            let live =
                if current == Some((active.owner, active.handle))
                {
                    crate::kernel::with_desktop(|desktop| {
                        desktop.standard_file
                               .get(active.owner, active.handle)
                               .is_ok_and(|d| d.active() && d.revision == active.revision)
                    })
                }
                else
                {
                    Ok(false)
                };
            match live
            {
                Ok(true) => (),
                Ok(false) =>
                {
                    if !active.abandoned
                    {
                        active.abandoned = true;
                        if let Some(request) = active.request.as_mut()
                        {
                            request.cancel();
                        }
                    }
                }
                Err(_) =>
                {
                    InvocationGate::run(|| *IO.lock() = work);
                    WORK_READY.store(true, Ordering::Release);
                    return;
                }
            }
        }
        if work.is_none()
        {
            let Some((owner, handle)) = current
            else
            {
                return;
            };
            let taken = crate::kernel::with_desktop(|desktop| {
                let previous = desktop.owner;
                desktop.enter(owner);
                let result = (|| {
                    let tracking = desktop.standard_file
                                          .get(owner, handle)
                                          .ok()
                                          .and_then(|dialog| {
                                              dialog.main_widgets.scroll_tracking.then(|| {
                                                                                     (dialog.window,
                                                                         dialog.recovery_activity
                                                                               .clone())
                                                                                 })
                                          });
                    if let Some((window, activity)) = tracking
                    {
                        let mouse = desktop.events.mouse;
                        let down =
                            desktop.events.modifiers & crate::toolbox::events::BTN_STATE == 0;
                        if let Some(window) = desktop.windows.window(window)
                        {
                            let point = Point { h: mouse.h - window.bounds.left,
                                                v: mouse.v - window.bounds.top };
                            let mut poll = || {
                                desktop.sf_track_scroll(handle,
                                                        point,
                                                        down,
                                                        false,
                                                        crate::platform::ticks() as u32)
                            };
                            let result = if let Some(activity) = activity.as_ref()
                            {
                                activity.run(poll).and_then(core::convert::identity)
                            }
                            else
                            {
                                poll()
                            };
                            result?;
                        }
                    }
                    let dialog = desktop.standard_file.get_mut(owner, handle)?;
                    let Some((revision, query)) = dialog.query()?
                    else
                    {
                        return Ok(None);
                    };
                    Ok::<_, Status>(Some((revision,
                                          query,
                                          dialog.subject.clone().ok_or(WRONG_THREAD)?,
                                          dialog.recovery_activity.clone())))
                })();
                desktop.enter(previous);
                result
            });
            let (revision, query, subject, activity) = match taken
            {
                Ok(Ok(Some(value))) => value,
                Err(_) =>
                {
                    WORK_READY.store(true, Ordering::Release);
                    return;
                }
                Ok(Err(cause)) =>
                {
                    let _ = crate::kernel::with_desktop(|desktop| {
                        let previous = desktop.owner;
                        desktop.enter(owner);
                        let result = (|| {
                            let activity = desktop.standard_file
                                                  .get(owner, handle)?
                                                  .recovery_activity
                                                  .clone()
                                                  .ok_or(WRONG_THREAD)?;
                            activity.run(|| {
                                        desktop.standard_file.get_mut(owner, handle)?.fail(cause);
                                        desktop.sf_sync_sheet(handle)
                                   .and_then(|_| desktop.standard_file_draw(handle, Damage::full()))
                                    })
                                    .and_then(core::convert::identity)
                        })();
                        desktop.enter(previous);
                        result
                    });
                    return;
                }
                Ok(Ok(None)) => return
            };
            let begin = || {
                match query
                {
                    Query::File(call, text) => ResidentRequest::file(&subject, call, text.text()?),
                    Query::Scan(call) => ResidentRequest::scan(&subject, call),
                    Query::PrepareName { volume,
                                         name } =>
                    {
                        ResidentRequest::prepare_name(&subject, volume, name.text()?)
                    }
                }
            };
            let request = if let Some(activity) = activity.as_ref()
            {
                activity.run(begin).and_then(core::convert::identity)
            }
            else
            {
                begin()
            };
            let (request, completed) = match request
            {
                Ok(request) => (Some(request), None),
                Err(cause) => (None, Some(Err(cause)))
            };
            work = Some(Work { handle,
                               owner,
                               revision,
                               request,
                               completed,
                               icon_prepared: false,
                               abandoned: false,
                               refusal: None,
                               icon: None,
                               activity });
        }
        let Some(mut work) = work
        else
        {
            return;
        };
        if work.completed.is_none()
        {
            let reply = match work.request.as_mut()
            {
                Some(request) => request.poll(),
                None => Err(BAD_ABI)
            };
            match reply
            {
                Ok(None) =>
                {
                    InvocationGate::run(|| *IO.lock() = Some(work));
                    WORK_READY.store(true, Ordering::Release);
                    return;
                }
                Ok(Some(reply)) =>
                {
                    work.completed =
                        Some(match reply
                             {
                                 ResidentReply::File { operation,
                                                       index,
                                                       volume_info,
                                                       entry } =>
                                 {
                                     Ok(Reply::File(standard_file::FileResult { operation,
                                                                                index,
                                                                                volume_info,
                                                                                entry }))
                                 }
                                 ResidentReply::Scan(call) => Ok(Reply::Scan(call)),
                                 ResidentReply::PreparedName { bytes,
                                                               length } =>
                                 {
                                     core::str::from_utf8(&bytes[..length]).map_err(|_| BAD_ABI)
                                                                           .and_then(Text::new)
                                                                           .map(Reply::Name)
                                 }
                             })
                }
                Err(cause) =>
                {
                    if work.request.as_ref().is_some_and(ResidentRequest::pending)
                    {
                        work.refusal = Some(cause);
                        if let Some(request) = work.request.as_mut()
                        {
                            request.cancel();
                        }
                        InvocationGate::run(|| *IO.lock() = Some(work));
                        WORK_READY.store(true, Ordering::Release);
                        return;
                    }
                    work.completed = Some(Err(work.refusal.unwrap_or(cause)));
                }
            }
            work.request = None;
            if let Some(cause) = work.refusal
            {
                work.completed = Some(Err(cause));
            }
        }
        if work.abandoned
        {
            drop(work);
            if InvocationGate::run(|| CURRENT.lock().is_some())
            {
                WORK_READY.store(true, Ordering::Release);
            }
            return;
        }
        if !work.icon_prepared
        {
            let candidate = match work.completed.as_ref()
            {
                Some(Ok(Reply::Scan(call))) if call.step == files::SCAN_ENTRY =>
                {
                    match crate::kernel::with_desktop(|desktop| {
                              desktop.standard_file
                                     .get(work.owner, work.handle)
                                     .is_ok_and(|dialog| {
                                         dialog.revision == work.revision && dialog.wants_icon(call)
                                     })
                          })
                    {
                        Ok(true) => Some(call.entry),
                        Ok(false) => None,
                        Err(_) =>
                        {
                            work.request = None;
                            InvocationGate::run(|| *IO.lock() = Some(work));
                            WORK_READY.store(true, Ordering::Release);
                            return;
                        }
                    }
                }
                Some(Ok(Reply::File(call)))
                    if call.operation == files::CATALOG && call.entry.id == files::ROOT_ID =>
                {
                    Some(call.entry)
                }
                _ => None
            };
            let resolve = || {
                candidate.and_then(|entry| {
                             resolve_icon(work.owner, entry).ok().map(|drawing| (entry, drawing))
                         })
            };
            work.icon = if let Some(activity) = work.activity.as_ref()
            {
                activity.run(resolve).ok().flatten()
            }
            else
            {
                resolve()
            };
            work.icon_prepared = true;
        }
        let reply = work.completed.unwrap_or(Err(BAD_ABI));
        let apply =
            || Self::sf_receive(work.handle, work.owner, work.revision, reply, work.icon.clone());
        let applied = if let Some(activity) = work.activity.as_ref()
        {
            activity.run(apply).unwrap_or(false)
        }
        else
        {
            apply()
        };
        if !applied
        {
            InvocationGate::run(|| *IO.lock() = Some(work));
            WORK_READY.store(true, Ordering::Release);
        }
    }

    fn sf_receive(handle: u64, owner: u64, revision: u64, reply: Result<Reply, Status>,
                  icon: Option<(files::CatalogInfo,
                          crate::toolbox::quickdraw::icons_service::Drawing)>)
                  -> bool
    {
        let outcome = crate::kernel::with_desktop(|desktop| {
            let previous = desktop.owner;
            desktop.enter(owner);
            let result = (|| {
                let dialog = desktop.standard_file.get_mut(owner, handle)?;
                if dialog.revision != revision || !dialog.active()
                {
                    return Ok(());
                }
                let before_first = dialog.first;
                let before_selected = dialog.selected;
                let before_sheet = dialog.sheet;
                let before_total = dialog.entries.count();
                let before_ids = core::array::from_fn::<_, { standard_file::page::ROWS }, _>(|i| {
                    dialog.visible_entries().get(i).map(|e| (e.volume, e.id))
                });
                let before_title = dialog.ancestors.first().map(|e| (e.volume, e.id));
                let before_ancestors = dialog.ancestors.len();
                let before_accept = dialog.can_accept();
                let name_changed = matches!(&reply, Ok(Reply::Name(_)));
                let volume_changed =
                    matches!(&reply,Ok(Reply::File(call))if call.operation==files::VOLUME_INFO);
                let changed = match dialog.receive(revision, reply)
                {
                    Ok(changed) => changed,
                    Err(cause) =>
                    {
                        dialog.error = cause;
                        dialog.sheet = Sheet::Error;
                        true
                    }
                };
                if let Some((entry, drawing)) = icon
                {
                    dialog.icons.retain(|(entry, _)| {
                                    dialog.entries.rows[..dialog.entries.length].contains(entry)
                                });
                    if dialog.icons.len() < standard_file::page::ROWS
                       && !dialog.icons.iter().any(|(key, _)| *key == entry)
                    {
                        dialog.icons.push((entry, drawing));
                    }
                }
                if dialog.work_pending()
                {
                    WORK_READY.store(true, Ordering::Release);
                }
                if changed
                {
                    let template = layout(dialog.mode, dialog.sheet)?;
                    let mut damage = Damage::new();
                    if before_sheet != dialog.sheet
                    {
                        damage = Damage::full();
                    }
                    else
                    {
                        let list = template.item(7);
                        if before_first != dialog.first
                        {
                            damage.add(list);
                        }
                        else
                        {
                            for (index, old) in before_ids.into_iter().enumerate()
                            {
                                let after =
                                    dialog.visible_entries().get(index).map(|e| (e.volume, e.id));
                                if old != after
                                {
                                    if let Some(row) =
                                        row_bounds(list, dialog.first, dialog.first + index)
                                    {
                                        damage.add(row);
                                    }
                                }
                            }
                            if before_selected != dialog.selected
                            {
                                for index in
                                    [before_selected, dialog.selected].into_iter().flatten()
                                {
                                    if let Some(row) = row_bounds(list, dialog.first, index)
                                    {
                                        damage.add(row);
                                    }
                                }
                            }
                        }
                        if before_total != dialog.entries.count()
                        {
                            damage.add(Rect::new(list.right - 16,
                                                 list.top,
                                                 list.right,
                                                 list.bottom));
                        }
                        if before_title != dialog.ancestors.first().map(|e| (e.volume, e.id))
                        {
                            damage.add(template.item(8));
                        }
                        if volume_changed
                        {
                            damage.add(Rect::new(238, 0, 344, 30));
                        }
                        if name_changed
                        {
                            let mut edit = template.item(if dialog.sheet == Sheet::NewFolder
                                                         {
                                                             3
                                                         }
                                                         else
                                                         {
                                                             10
                                                         });
                            edit.inset(-3, -3);
                            damage.add(edit);
                        }
                        if before_accept != dialog.can_accept()
                        {
                            let mut button = template.item(1);
                            button.inset(-4, -4);
                            damage.add(button);
                        }
                        if dialog.popup && before_ancestors != dialog.ancestors.len()
                        {
                            let pop = template.item(8);
                            damage.add(Rect::new(pop.left, pop.top, pop.right, pop.top + 12 * 18));
                        }
                    }
                    desktop.sf_sync_sheet(handle)?;
                    desktop.standard_file_draw(handle, damage)?;
                }
                Ok::<_, Status>(())
            })();
            desktop.enter(previous);
            result
        });
        match outcome
        {
            Err(_) => false,
            Ok(Err(status)) =>
            {
                serial!("STANDARD_FILE_CONTINUATION_FAILED app={owner} status={status}");
                true
            }
            Ok(Ok(())) => true
        }
    }
}

/// Resolve file-local artwork only outside Desktop/Files guard nesting. Missing
/// or unavailable custom artwork uses the boot-admitted normal System family.
fn resolve_icon(owner: u64, entry: files::CatalogInfo)
                -> Result<crate::toolbox::quickdraw::icons_service::Drawing, Status>
{
    use crate::toolbox::quickdraw::icons_service::{self, Drawing};
    if entry.is_directory()
    {
        return standard_file::templates::fallback(&entry);
    }
    let destination = Rect::new(0, 0, 16, 16);
    crate::kernel::with_resource_owner(owner, || {
        crate::kernel::with_resources(|resources| {
            let actual = if entry.resource_length != 0
            {
                crate::kernel::with_files(|files| {
                    Drawing::file(files,
                                  resources,
                                  &FileIconCall { operation: ICON_PLOT_FILE,
                                                  volume: entry.volume,
                                                  cnid: entry.id,
                                                  destination,
                                                  ..Default::default() })
                }).ok()
                  .and_then(Result::ok)
            }
            else
            {
                None
            };
            if let Some(drawing) = actual
            {
                return Ok(drawing);
            }
            if let Ok(drawing) =
                Drawing::file_default(entry.file_type, entry.name(), destination, 0)
            {
                return Ok(drawing);
            }
            let mut lookup = IconCall { operation: ICON_FILE,
                                        scope: resources::CHAIN,
                                        creator: entry.creator,
                                        file_type: entry.file_type,
                                        ..Default::default() };
            if icons_service::query(resources, &mut lookup).is_ok()
            {
                if let Ok(drawing) = Drawing::new(resources,
                                                  &IconCall { scope: lookup.origin,
                                                              id: lookup.id,
                                                              destination,
                                                              ..Default::default() },
                                                  Default::default())
                {
                    return Ok(drawing);
                }
            }
            standard_file::templates::fallback(&entry)
        }).ok_or(NOT_SUPPORTED)?
    })?
}
