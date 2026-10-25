//! Standard File Package UI. The caller waits; System owns every event and pixel.
use alloc::{string::String, vec::Vec};

use mac_abi::{quickdraw::*, standard_file::*, *};

use crate::toolbox::{events::input::Input,
                     standard_file::{Action, Sheet, layout::Layout}};
fn checked(s: Status) -> Result<(), Status>
{
    if s == OK { Ok(()) } else { Err(s) }
}
fn layout(mode: u32, sheet: Sheet) -> Result<Layout, Status>
{
    let id = match sheet
    {
        Sheet::Main =>
        {
            if mode == GET
            {
                -6042
            }
            else
            {
                -6043
            }
        }
        Sheet::NewFolder => -6046,
        Sheet::Replace => -6045,
        Sheet::Error => -6044
    };
    let r = crate::kernel::resources().ok_or(NOT_SUPPORTED)?;
    let (_, d) = r.get(resources::SYSTEM,
                       ResourceKey { kind: *b"DLOG",
                                     id })?;
    let (_, l) = r.get(resources::SYSTEM,
                       ResourceKey { kind: *b"DITL",
                                     id })?;
    Layout::parse(&d.data, &l.data)
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
fn label(index: u32) -> Result<String, Status>
{
    let mut bytes = [0; 768];
    let mut c = resources::ResourceCall { operation: resources::STRING,
                                          scope: resources::SYSTEM,
                                          key: ResourceKey { kind: *b"STR#",
                                                             id:   -16490 },
                                          index,
                                          ..Default::default() };
    crate::kernel::resources().ok_or(NOT_SUPPORTED)?.request(&mut c, "", &mut bytes)?;
    Ok(String::from(core::str::from_utf8(&bytes[..c.transferred]).map_err(|_| BAD_ARGUMENT)?))
}
// Initialization owns the published record until every window/font/drawing
// step succeeds. No early return can strand an active modal dialog.
struct InitializingDialog<'a>
{
    desktop:   &'a mut super::Desktop,
    handle:    u64,
    failure:   Option<Status>,
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
                if self.desktop.standard_file.get(self.handle).is_err()
                {
                    // The parent protocol is disposed. A refused child WDEF is
                    // retained by its own registered quarantine, not this dialog.
                    serial!("STANDARD_FILE_ROLLBACK_CHILD_REFUSED: app={} handle={} \
                             initialization={:?} cleanup={}",
                            self.desktop.owner,
                            self.handle,
                            self.failure,
                            status);
                }
                else
                {
                    if let Ok(dialog) = self.desktop.standard_file.get_mut(self.handle)
                    {
                        dialog.state = CANCELLED;
                    }
                    serial!("STANDARD_FILE_ROLLBACK_RETAINED: app={} handle={} \
                             initialization={:?} cleanup={}; inactive protocol remains \
                             registered for owner cleanup",
                            self.desktop.owner,
                            self.handle,
                            self.failure,
                            status);
                }
            }
        }
    }
}
impl super::Desktop
{
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
        crate::kernel::with_files(|files| self.standard_file.request(&ledger, call, files))
            .map_err(|error| error.status())??;
        if call.operation == BEGIN
        {
            let mut guard = InitializingDialog { desktop:   self,
                                                 handle:    call.handle,
                                                 failure:   None,
                                                 committed: false };
            let result = guard.desktop.initialize_file_dialog(call);
            match result
            {
                Ok(()) => guard.committed = true,
                Err(status) => guard.failure = Some(status)
            }
            return result;
        }
        Ok(())
    }

    fn dispose_file_dialog(&mut self, handle: u64) -> Result<(), Status>
    {
        // Invalidate the parent first and pin it across child disposal.
        // Child refusals retain their own registered WDEF tickets.
        let dialog = self.standard_file.take(self.owner, handle)?;
        let windows = [dialog.auxiliary, dialog.window];
        let mut failure = None;
        for window in windows
        {
            if self.windows.window(window).is_some()
            {
                if let Err(status) = checked(self.window(CLOSE_WINDOW, &mut request(window)))
                {
                    failure.get_or_insert(status);
                }
            }
        }
        drop(dialog);
        failure.map_or(Ok(()), Err)
    }

    fn initialize_file_dialog(&mut self, call: &StandardFileCall) -> Result<(), Status>
    {
        let l = layout(call.mode, Sheet::Main)?;
        let height = l.bounds.bottom - l.bounds.top;
        let mut w = request(WindowId(0));
        let top = if call.mode == GET { 84 } else { 80 };
        w.bounds = Rect::new(148, top, 492, top + height);
        w.procedure = l.procedure;
        checked(self.window(NEW_WINDOW, &mut w))?;
        let d = self.standard_file.get_mut(call.handle)?;
        d.window = WindowId(w.id);
        d.editor
         .set_font(FontSelection::default(), crate::kernel::fonts().ok_or(NOT_SUPPORTED)?)?;
        // Filename input is bounded to MacRoman HFS names. Prepare every
        // accepted scalar once so later keys and changed default names use
        // the resident font's actual advances, without per-key font work.
        let mut filename_characters = String::new();
        filename_characters.try_reserve(768).map_err(|_| NO_MEMORY)?;
        for byte in 32..=255
        {
            filename_characters.push(mac_formats::text::character(byte));
        }
        d.editor
         .prepare_font_input(&filename_characters, crate::kernel::fonts().ok_or(NOT_SUPPORTED)?)?;
        let mut clip = request(WindowId(w.id));
        checked(self.window(SET_GROW_ICON_CLIP, &mut clip))?;
        checked(self.window(SELECT_WINDOW, &mut w))?;
        self.standard_file_draw(call.handle)?;
        serial!("STANDARD_FILE_BEGIN app={} handle={} mode={} window={}", self.owner, call.handle,
                call.mode, w.id);
        Ok(())
    }

    fn sf_shape(&mut self, window: u64, bounds: Rect, verb: u32) -> Result<(), Status>
    {
        self.graphics(&mut GraphicsCall { operation: QD_SHAPE,
                                          target:
                                              mac_abi::quickdraw::Target::window(WindowId(window)),
                                          bounds,
                                          shape: SHAPE_RECT,
                                          verb,
                                          ..Default::default() },
                      &[],
                      &[])
    }

    fn sf_text(&mut self, window: u64, x: i16, y: i16, text: &str) -> Result<(), Status>
    {
        self.font_draw(&mut FontCall { target:
                                           mac_abi::quickdraw::Target::window(WindowId(window)),
                                       selection: FontSelection::default(),
                                       baseline: Point { h: x,
                                                         v: y },
                                       ..Default::default() },
                       text,
                       crate::kernel::fonts().ok_or(NOT_SUPPORTED)?)
    }

    fn sf_button(&mut self, window: u64, bounds: Rect, label: &str, enabled: bool, pressed: bool,
                 default: bool)
                 -> Result<(), Status>
    {
        let mut standard = definitions::ControlDefinitionState::new(bounds, label);
        standard.enabled = u32::from(enabled);
        standard.highlighted = u32::from(pressed);
        checked(self.control(&mut controls::ControlCall { operation: controls::DRAW_STANDARD,
                                                          window,
                                                          standard,
                                                          ..Default::default() }))?;
        if default
        {
            self.graphics(&mut GraphicsCall { operation: QD_PEN_SIZE,
                                              target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                              point: Point { h: 3,
                                                             v: 3 },
                                              ..Default::default() },
                          &[],
                          &[])?;
            self.graphics(&mut GraphicsCall { operation: QD_SHAPE,
                                              target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                              bounds: Rect::new(bounds.left - 4,
                                                                bounds.top - 4,
                                                                bounds.right + 4,
                                                                bounds.bottom + 4),
                                              shape: SHAPE_ROUND_RECT,
                                              verb: VERB_FRAME,
                                              corner: Point { h: 16,
                                                              v: 16 },
                                              ..Default::default() },
                          &[],
                          &[])?;
            self.graphics(&mut GraphicsCall { operation: QD_PEN_SIZE,
                                              target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                              point: Point { h: 1,
                                                             v: 1 },
                                              ..Default::default() },
                          &[],
                          &[])?;
        }
        Ok(())
    }

    fn sf_wrapped_text(&mut self, window: u64, bounds: Rect, text: &str) -> Result<(), Status>
    {
        let fonts = crate::kernel::fonts().ok_or(NOT_SUPPORTED)?;
        let font = crate::toolbox::quickdraw::fonts_service::selection(FontSelection::default());
        let metrics = fonts.metrics(font)?;
        let advance = (metrics.ascent + metrics.descent + metrics.leading).max(1);
        let mut baseline = bounds.top + metrics.ascent;
        let mut line = String::new();
        for word in text.split_whitespace()
        {
            let extra = if line.is_empty() { 0 } else { fonts.text_width(font, " ")? };
            if !line.is_empty()
               && fonts.text_width(font, &line)? + extra + fonts.text_width(font, word)?
                  > bounds.right - bounds.left
            {
                self.sf_text(window, bounds.left, baseline, &line)?;
                baseline += advance;
                line.clear();
            }
            if baseline > bounds.bottom
            {
                break;
            }
            if !line.is_empty()
            {
                line.push(' ');
            }
            line.push_str(word);
        }
        if !line.is_empty()
        {
            self.sf_text(window, bounds.left, baseline, &line)?;
        }
        Ok(())
    }

    fn sf_sync_sheet(&mut self, id: u64) -> Result<(), Status>
    {
        let d = self.standard_file.get(id)?;
        let sheet = d.sheet;
        let mode = d.mode;
        let old = d.auxiliary;
        let previous = d.auxiliary_sheet;
        let main = d.window;
        if sheet != previous
        {
            if old.0 != 0 && self.windows.window(old).is_some()
            {
                let result = checked(self.window(CLOSE_WINDOW, &mut request(old)));
                let dialog = self.standard_file.get_mut(id)?;
                dialog.auxiliary = WindowId(0);
                dialog.auxiliary_sheet = Sheet::Main;
                result?;
            }
            let mut auxiliary = WindowId(0);
            if sheet != Sheet::Main
            {
                let l = layout(mode, sheet)?;
                let width = l.bounds.right - l.bounds.left;
                let height = l.bounds.bottom - l.bounds.top;
                let mut w = request(WindowId(0));
                let left = (crate::display::WIDTH as i16 - width) / 2;
                w.bounds = Rect::new(left, 100, left + width, 100 + height);
                w.procedure = l.procedure;
                checked(self.window(NEW_WINDOW, &mut w))?;
                auxiliary = WindowId(w.id);
                checked(self.window(SET_GROW_ICON_CLIP, &mut request(auxiliary)))?;
                checked(self.window(SELECT_WINDOW, &mut w))?;
                let d = self.standard_file.get_mut(id)?;
                if sheet == Sheet::NewFolder
                {
                    let name = l.items.get(2).ok_or(BAD_ARGUMENT)?.label.as_str();
                    d.editor.set_text(name)?;
                    d.editor.select(0, name.len() as u32)?;
                }
            }
            else
            {
                checked(self.window(SELECT_WINDOW, &mut request(main)))?;
            }
            let d = self.standard_file.get_mut(id)?;
            d.auxiliary = auxiliary;
            d.auxiliary_sheet = sheet;
            d.pressed = None;
            d.hot = false;
        }
        let d = self.standard_file.get_mut(id)?;
        let view = if sheet == Sheet::NewFolder
        {
            layout(mode, sheet)?.item(3)
        }
        else
        {
            Rect::new(15, 157, 227, 173)
        };
        if d.editor.info().view != view
        {
            d.editor
             .set_rects(Rect::new(view.left, view.top, view.left + 4096, view.bottom), view)?;
        }
        Ok(())
    }

    fn standard_file_icon(&mut self, id: u64, entry: files::CatalogInfo, destination: Rect)
                          -> Result<crate::toolbox::quickdraw::icons_service::Drawing, Status>
    {
        use crate::toolbox::quickdraw::icons_service::{self, Drawing};
        let dialog = self.standard_file.get_mut(id)?;
        if let Some((_, drawing)) = dialog.icons.iter().find(|(key, _)| *key == entry)
        {
            let mut drawing = drawing.clone();
            drawing.destination = destination;
            return Ok(drawing);
        }
        let resources = crate::kernel::with_resource_owner(dialog.owner, crate::kernel::resources)?
                            .ok_or(NOT_SUPPORTED)?;
        let mut call = IconCall { target:
                                      mac_abi::quickdraw::Target::window(WindowId(dialog.window.0)),
                                  scope: resources::SYSTEM,
                                  id: if entry.is_directory() { -3999 } else { -4000 },
                                  destination,
                                  ..Default::default() };
        let actual = if entry.resource_length != 0 && !entry.is_directory()
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
        let actual =
            actual.or_else(|| {
                      if entry.is_directory()
                      {
                          None
                      }
                      else
                      {
                          Drawing::file_default(entry.file_type, entry.name(), destination, 0).ok()
                      }
                  });
        let drawing =
            if let Some(drawing) = actual
            {
                drawing
            }
            else
            {
                if !entry.is_directory()
                {
                    let mut lookup = IconCall { operation: ICON_FILE,
                                                scope: resources::CHAIN,
                                                creator: entry.creator,
                                                file_type: entry.file_type,
                                                ..Default::default() };
                    if icons_service::query(resources, &mut lookup).is_ok()
                    {
                        call.id = lookup.id;
                        call.scope = lookup.origin;
                    }
                }
                Drawing::new(resources, &call, Default::default()).or_else(|_| {
                    call.scope = resources::SYSTEM;
                    call.id = if entry.is_directory() { -3999 } else { -4000 };
                    Drawing::new(resources, &call, Default::default())
                })?
            };
        // Retain only visible rows. Immutable families are shared with retained
        // drawing commands, so selection/caret updates do no disk I/O or decoding.
        dialog.icons.try_reserve(1).map_err(|_| NO_MEMORY)?;
        dialog.icons.push((entry, drawing.clone()));
        Ok(drawing)
    }

    fn standard_file_draw(&mut self, id: u64) -> Result<(), Status>
    {
        let d = self.standard_file.get(id)?;
        let sheet = d.sheet;
        let window = if sheet == Sheet::Main { d.window } else { d.auxiliary };
        let mode = d.mode;
        let l = layout(mode, sheet)?;
        let error = d.error;
        let pressed = d.pressed;
        let hot = d.hot;
        let can_accept = d.can_accept();
        let first = d.first;
        let selected = d.selected;
        let page = d.page();
        let mut rows = Vec::new();
        rows.try_reserve(page).map_err(|_| NO_MEMORY)?;
        for (i, e) in d.entries.iter().enumerate().skip(first).take(page)
        {
            rows.push((i, *e));
        }
        let total = d.entries.len();
        let location = d.location;
        let desktop_label = label(0)?;
        let title = d.ancestors.first().map_or(desktop_label.as_str(), |e| e.name());
        let title = String::from(title);
        let volume = if location.0 == 0
        {
            String::new()
        }
        else
        {
            crate::kernel::with_files(|f| {
                let mut c = files::FileCall { operation: files::VOLUME_INFO,
                                              volume: location.0,
                                              ..Default::default() };
                f.request(&mut c, "", &mut [])?;
                Ok::<_, Status>(String::from(c.volume_info.name()))
            }).map_err(|e| e.status())??
        };
        let prompt = if d.prompt.is_empty() { label(2)? } else { String::from(d.prompt.as_str()) };
        let name = String::from(d.name());
        let focus = d.focus_name;
        let editor_info = d.editor.info();
        let caret = d.editor.caret_rect();
        let mut selection = Vec::new();
        selection.try_reserve(2).map_err(|_| NO_MEMORY)?;
        d.editor.selection_rects(|r| selection.push(r));
        let popup = d.popup;
        let ancestors: Vec<_> = d.ancestors.iter().map(|e| String::from(e.name())).collect();
        self.standard_file
            .get_mut(id)?
            .icons
            .retain(|(entry, _)| rows.iter().any(|(_, e)| e == entry));
        checked(self.window(BEGIN_UPDATE, &mut request(window)))?;
        let result = (|| {
            self.graphics(&mut GraphicsCall { operation: QD_RESET,
                                              target: mac_abi::quickdraw::Target::window(WindowId(window.0)),
                                              ..Default::default() },
                          &[],
                          &[])?;
            self.sf_shape(window.0,
                          Rect::new(0,
                                    0,
                                    l.bounds.right - l.bounds.left,
                                    l.bounds.bottom - l.bounds.top),
                          VERB_ERASE)?;
            if sheet != Sheet::Main
            {
                let message = match sheet
                {
                    Sheet::Replace =>
                    {
                        l.items.get(2).ok_or(BAD_ARGUMENT)?.label.replace("^0", &name)
                    }
                    Sheet::NewFolder => l.items.get(3).ok_or(BAD_ARGUMENT)?.label.clone(),
                    Sheet::Error =>
                    {
                        label(match error
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
                              })?
                    }
                    _ => unreachable!()
                };
                let message_bounds = l.item(if sheet == Sheet::NewFolder
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
                                            });
                self.sf_wrapped_text(window.0, message_bounds, &message)?;
                if sheet == Sheet::NewFolder
                {
                    let edit = l.item(3);
                    self.sf_shape(window.0,
                                  Rect::new(edit.left - 3,
                                            edit.top - 3,
                                            edit.right + 3,
                                            edit.bottom + 3),
                                  VERB_FRAME)?;
                    self.graphics(&mut GraphicsCall { operation: QD_CLIP_RECT,
                                                      target: mac_abi::quickdraw::Target::window(WindowId(window.0)),
                                                      bounds: edit,
                                                      ..Default::default() },
                                  &[],
                                  &[])?;
                    self.sf_text(window.0,
                                 editor_info.dest.left,
                                 editor_info.dest.top + editor_info.ascent,
                                 &name)?;
                    for r in selection
                    {
                        self.sf_shape(window.0, r, VERB_INVERT)?;
                    }
                    self.sf_shape(window.0, caret, VERB_INVERT)?;
                    self.graphics(&mut GraphicsCall { operation: QD_CLIP_RECT,
                                                      target: mac_abi::quickdraw::Target::window(WindowId(window.0)),
                                                      bounds: Rect::new(0,
                                                                        0,
                                                                        l.bounds.right
                                                                        - l.bounds.left,
                                                                        l.bounds.bottom
                                                                        - l.bounds.top),
                                                      ..Default::default() },
                                  &[],
                                  &[])?;
                }
                for n in 1..=if sheet == Sheet::Error { 1 } else { 2 }
                {
                    self.sf_button(window.0,
                                   l.item(n),
                                   &l.items[n - 1].label,
                                   true,
                                   pressed == Some(n) && hot,
                                   n == 1)?;
                }
                return Ok(());
            }
            for (i, item) in l.items.iter().enumerate()
            {
                let n = i + 1;
                if item.kind & 0x7f == 4
                {
                    self.sf_button(window.0,
                                   item.bounds,
                                   &item.label,
                                   n != 5 && (n != 1 || can_accept),
                                   pressed == Some(n) && hot,
                                   n == 1)?;
                }
            }
            // Original templates use user items for the popup, list and New Folder.
            let pop = l.item(8);
            self.sf_shape(window.0, pop, VERB_FRAME)?;
            self.sf_text(window.0, pop.left + 8, pop.top + 14, &title)?;
            let points = [Point { h: pop.right - 14,
                                  v: pop.top + 7 },
                          Point { h: pop.right - 6,
                                  v: pop.top + 7 },
                          Point { h: pop.right - 10,
                                  v: pop.top + 11 }];
            self.graphics(&mut GraphicsCall { operation: QD_SHAPE,
                                              target: mac_abi::quickdraw::Target::window(WindowId(window.0)),
                                              shape: SHAPE_POLYGON,
                                              verb: VERB_PAINT,
                                              points: points.as_ptr(),
                                              point_count: points.len(),
                                              ..Default::default() },
                          &points,
                          &[])?;
            self.sf_text(window.0, 239, 21, &volume)?;
            let list = l.item(7);
            self.sf_shape(window.0, list, VERB_FRAME)?;
            for (i, e) in rows
            {
                let y = list.top + 1 + (i - first) as i16 * 16;
                if selected == Some(i)
                {
                    self.sf_shape(window.0,
                                  Rect::new(list.left + 1, y, list.right - 16, y + 16),
                                  VERB_PAINT)?;
                    self.graphics(&mut GraphicsCall { operation: QD_COLORS,
                                                      target: mac_abi::quickdraw::Target::window(WindowId(window.0)),
                                                      state: PortState { foreground:
                                                                             0xffffff,
                                                                         background: 0,
                                                                         ..Default::default() },
                                                      ..Default::default() },
                                  &[],
                                  &[])?;
                }
                let c =
                    IconCall { target: mac_abi::quickdraw::Target::window(WindowId(window.0)),
                               destination: Rect::new(list.left + 2, y, list.left + 18, y + 16),
                               ..Default::default() };
                let drawing = self.standard_file_icon(id, e, c.destination)?;
                self.icon(&c, drawing)?;
                self.graphics(&mut GraphicsCall { operation: QD_CLIP_RECT,
                                                  target: mac_abi::quickdraw::Target::window(WindowId(window.0)),
                                                  bounds: Rect::new(list.left + 20,
                                                                    y,
                                                                    list.right - 17,
                                                                    y + 16),
                                                  ..Default::default() },
                              &[],
                              &[])?;
                self.font_draw(&mut FontCall { target: mac_abi::quickdraw::Target::window(WindowId(window.0)),
                                               selection: FontSelection::default(),
                                               baseline: Point { h: list.left + 21,
                                                                 v: y + 13 },
                                               color: if selected == Some(i)
                                               {
                                                   0xffffff
                                               }
                                               else
                                               {
                                                   0
                                               },
                                               ..Default::default() },
                               e.name(),
                               crate::kernel::fonts().ok_or(NOT_SUPPORTED)?)?;
                self.graphics(&mut GraphicsCall { operation: QD_CLIP_RECT,
                                                  target: mac_abi::quickdraw::Target::window(WindowId(window.0)),
                                                  bounds: Rect::new(0, 0, 344, 188),
                                                  ..Default::default() },
                              &[],
                              &[])?;
                self.graphics(&mut GraphicsCall { operation: QD_COLORS,
                                                  target: mac_abi::quickdraw::Target::window(WindowId(window.0)),
                                                  state: PortState { foreground: 0,
                                                                     background: 0xffffff,
                                                                     ..Default::default() },
                                                  ..Default::default() },
                              &[],
                              &[])?;
            }
            let scrollbar = controls::ScrollbarState { bounds: Rect::new(list.right - 16,
                                                                         list.top,
                                                                         list.right,
                                                                         list.bottom),
                                                       maximum: total.saturating_sub(page)
                                                                as i16,
                                                       value: first as i16,
                                                       active: u32::from(total > page),
                                                       ..Default::default() };
            checked(self.control(&mut controls::ControlCall { operation:
                                                                  controls::DRAW_SCROLLBAR,
                                                              window: window.0,
                                                              scrollbar,
                                                              ..Default::default() }))?;
            self.sf_shape(window.0, l.item(9), VERB_PAINT)?;
            if mode == PUT
            {
                self.sf_button(window.0,
                               l.item(12),
                               &label(1)?,
                               location.0 != 0,
                               pressed == Some(12) && hot,
                               false)?;
                self.sf_text(window.0, l.item(11).left, l.item(11).top + 13, &prompt)?;
                let edit = l.item(10);
                self.sf_shape(window.0,
                              Rect::new(edit.left - 3,
                                        edit.top - 3,
                                        edit.right + 3,
                                        edit.bottom + 3),
                              VERB_FRAME)?;
                self.graphics(&mut GraphicsCall { operation: QD_CLIP_RECT,
                                                  target: mac_abi::quickdraw::Target::window(WindowId(window.0)),
                                                  bounds: edit,
                                                  ..Default::default() },
                              &[],
                              &[])?;
                self.sf_text(window.0,
                             editor_info.dest.left,
                             editor_info.dest.top + editor_info.ascent,
                             &name)?;
                if focus
                {
                    for r in selection
                    {
                        self.sf_shape(window.0, r, VERB_INVERT)?;
                    }
                    self.sf_shape(window.0, caret, VERB_INVERT)?;
                }
                self.graphics(&mut GraphicsCall { operation: QD_CLIP_RECT,
                                                  target: mac_abi::quickdraw::Target::window(WindowId(window.0)),
                                                  bounds: Rect::new(0, 0, 344, 188),
                                                  ..Default::default() },
                              &[],
                              &[])?;
            }
            if popup
            {
                let height = (ancestors.len() + 1).min(12) as i16 * 18;
                let area = Rect::new(pop.left, pop.top, pop.right, pop.top + height);
                self.sf_shape(window.0, area, VERB_ERASE)?;
                self.sf_shape(window.0, area, VERB_FRAME)?;
                for (i, title) in ancestors.iter().take(11).enumerate()
                {
                    self.sf_text(window.0, pop.left + 8, pop.top + 14 + i as i16 * 18, title)?;
                }
                self.sf_text(window.0,
                             pop.left + 8,
                             pop.top + 14 + ancestors.len().min(11) as i16 * 18,
                             &desktop_label)?;
            }
            Ok(())
        })();
        checked(self.window(END_UPDATE, &mut request(window)))?;
        result
    }

    fn sf_action(&mut self, id: u64, action: Action, when: u32)
    {
        let now = crate::machine::clock::now().ok().and_then(|t| t.mac_seconds()).unwrap_or(0);
        let timestamp =
            if action == Action::Confirm || action == Action::Accept { now } else { when };
        let result = crate::kernel::with_files(|f| {
                         self.standard_file.action(id, f, action, timestamp)
                     }).map_err(|e| e.status())
                       .and_then(|r| r);
        if let Ok(d) = self.standard_file.get_mut(id)
        {
            let view = d.editor.info().view;
            let info = d.editor.info();
            if let Ok(p) = d.editor.point(info.selection_end)
            {
                let dx = if info.selection_start == 0
                {
                    view.left - info.dest.left
                }
                else if p.h >= view.right
                {
                    view.right - 1 - p.h
                }
                else if p.h < view.left
                {
                    view.left - p.h
                }
                else
                {
                    0
                };
                if dx != 0
                {
                    let _ = d.editor.scroll(Point { h: dx,
                                                    v: 0 });
                }
            }
        }
        if let Err(e) = result
           && let Ok(d) = self.standard_file.get_mut(id)
        {
            d.error = e;
            d.sheet = Sheet::Error;
        }
        if let Ok(d) = self.standard_file.get(id)
           && d.state != PENDING
        {
            serial!("STANDARD_FILE_DONE app={} state={} volume={} parent={} cnid={} replacing={} \
                     name={}",
                    d.owner,
                    d.state,
                    d.result.volume,
                    d.result.parent,
                    d.result.id,
                    u32::from(d.replacing),
                    d.result.name());
        }
        if let Err(status) = self.sf_sync_sheet(id)
        {
            serial!("STANDARD_FILE_FAILED app={} status={}", self.owner, status);
        }
        let _ = self.standard_file_draw(id);
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
        let Ok(d) = self.standard_file.get(id)
        else
        {
            return false;
        };
        let sheet = d.sheet;
        let window = if sheet == Sheet::Main { d.window } else { d.auxiliary };
        let mode = d.mode;
        let focus = d.focus_name;
        let Ok(l) = layout(mode, sheet)
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
                if let Some(a) = action
                {
                    self.sf_action(id, a, when);
                }
            }
            Input::Button { point,
                            down } =>
            {
                let Some(w) = self.windows.window(window)
                else
                {
                    return true;
                };
                let p = Point { h: point.h - w.bounds.left,
                                v: point.v - w.bounds.top };
                if down
                {
                    if sheet == Sheet::NewFolder && l.item(3).contains(p)
                    {
                        let d = self.standard_file.get_mut(id).unwrap();
                        let _ = d.editor.click(p, self.events.modifiers & SHIFT_KEY != 0);
                        let _ = self.standard_file_draw(id);
                        return true;
                    }
                    if sheet == Sheet::Main
                    {
                        let popup = self.standard_file.get(id).is_ok_and(|d| d.popup);
                        if popup
                        {
                            let row = ((p.v - l.item(8).top) / 18).max(0) as usize;
                            let d = self.standard_file.get_mut(id).unwrap();
                            d.popup = false;
                            let dest =
                                d.ancestors.get(row).map(|e| (e.volume, e.id)).unwrap_or((0, 0));
                            let _ = crate::kernel::with_files(|f| {
                                self.standard_file.navigate_to(id, f, dest)
                            });
                            let _ = self.standard_file_draw(id);
                            return true;
                        }
                        if l.item(8).contains(p)
                        {
                            self.standard_file.get_mut(id).unwrap().popup = true;
                            let _ = self.standard_file_draw(id);
                            return true;
                        }
                        let list = l.item(7);
                        if list.contains(p)
                        {
                            let d = self.standard_file.get(id).unwrap();
                            if p.h >= list.right - 16
                            {
                                self.sf_action(id,
                                               Action::Page(if p.v < list.top + 16
                                                            {
                                                                -1
                                                            }
                                                            else
                                                            {
                                                                1
                                                            }),
                                               when);
                                return true;
                            }
                            let row = d.first + ((p.v - list.top - 1).max(0) / 16) as usize;
                            if row < d.entries.len()
                            {
                                let double =
                                    d.last_click
                                     .is_some_and(|(i, t)| i == row && when.wrapping_sub(t) <= 30);
                                self.sf_action(id,
                                               if double
                                               {
                                                   Action::Open(row)
                                               }
                                               else
                                               {
                                                   Action::Select(row)
                                               },
                                               when);
                                self.standard_file.get_mut(id).unwrap().last_click =
                                    if double { None } else { Some((row, when)) };
                            }
                            return true;
                        }
                        if mode == PUT && l.item(10).contains(p)
                        {
                            let d = self.standard_file.get_mut(id).unwrap();
                            d.focus_name = true;
                            d.selected = None;
                            let _ = d.editor.click(p, self.events.modifiers & SHIFT_KEY != 0);
                            let _ = self.standard_file_draw(id);
                            return true;
                        }
                        if l.item(4).contains(p)
                        {
                            self.sf_action(id, Action::Parent, when);
                            return true;
                        }
                    }
                    let button = if sheet == Sheet::Main
                    {
                        [1, 2, 5, 6, 12].into_iter().find(|&n| {
                                                        (n != 12 || mode == PUT)
                                                        && l.item(n).contains(p)
                                                        && n != 5
                                                    })
                    }
                    else
                    {
                        [1, 2].into_iter().find(|&n| l.item(n).contains(p))
                    };
                    let d = self.standard_file.get_mut(id).unwrap();
                    d.pressed = button;
                    d.hot = button.is_some();
                    let _ = self.standard_file_draw(id);
                }
                else
                {
                    let d = self.standard_file.get_mut(id).unwrap();
                    let pressed = d.pressed.take();
                    let hot = d.hot;
                    d.hot = false;
                    if let Some(button) = pressed.filter(|_| hot)
                    {
                        let a = match button
                        {
                            1 => Action::Accept,
                            2 => Action::Cancel,
                            6 => Action::Desktop,
                            12 => Action::NewFolder,
                            _ => return true
                        };
                        self.sf_action(id, a, when);
                    }
                    else
                    {
                        let _ = self.standard_file_draw(id);
                    }
                }
            }
            Input::Move(point) =>
            {
                let Some(w) = self.windows.window(window)
                else
                {
                    return true;
                };
                let p = Point { h: point.h - w.bounds.left,
                                v: point.v - w.bounds.top };
                let d = self.standard_file.get_mut(id).unwrap();
                if let Some(n) = d.pressed
                {
                    let bounds = l.item(n);
                    let hot = bounds.contains(p);
                    if hot != d.hot
                    {
                        d.hot = hot;
                        let _ = self.standard_file_draw(id);
                    }
                }
            }
        }
        true
    }
}
