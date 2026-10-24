//! Desktop adapter; no application callbacks or recursive ECALLs under its guard.
use alloc::string::String;

use mac_abi::{controls::*, dialogs::*, quickdraw::*, *};

use crate::toolbox::{dialogs,
                     quickdraw::{fonts_service, icons_service, service}};
fn checked(status: Status) -> Result<(), Status>
{
    if status == OK { Ok(()) } else { Err(status) }
}
impl super::Desktop
{
    pub(super) fn dialog_input(&mut self, input: crate::toolbox::events::input::Input, when: u32)
    {
        use crate::toolbox::events::{BTN_STATE, input::Input};
        let window = self.dialogs.window;
        let event = match input
        {
            Input::Move(point) =>
            {
                self.events.mouse = point;
                return;
            }
            Input::Button { point,
                            down } =>
            {
                self.events.mouse = point;
                if down
                {
                    self.events.modifiers &= !BTN_STATE;
                }
                else
                {
                    self.events.modifiers |= BTN_STATE;
                }
                if down && !self.dialogs.alert.as_ref().is_some_and(|a| a.bounds.contains(point))
                {
                    return;
                }
                Event { payload: 0,
                        kind: if down { EventKind::MouseDown } else { EventKind::MouseUp },
                        message: window,
                        when,
                        where_: point,
                        modifiers: self.events.modifiers }
            }
            Input::Key { mac_code,
                         character,
                         down,
                         repeat,
                         modifiers } =>
            {
                self.events.modifiers = modifiers | (self.events.modifiers & BTN_STATE);
                if mac_code == 0xff
                {
                    return;
                }
                Event { payload: 0,
                        kind: if !down
                        {
                            EventKind::KeyUp
                        }
                        else if repeat
                        {
                            EventKind::AutoKey
                        }
                        else
                        {
                            EventKind::KeyDown
                        },
                        message: u32::from(mac_code) << 8 | u32::from(character),
                        when,
                        where_: self.events.mouse,
                        modifiers: self.events.modifiers }
            }
        };
        self.events.post_system(event);
    }

    pub fn dialog(&mut self, call: &mut DialogCall) -> Status
    {
        self.dialog_result(call).map_or_else(|s| s, |()| OK)
    }

    fn dialog_result(&mut self, call: &mut DialogCall) -> Result<(), Status>
    {
        call.item = 0;
        match call.operation
        {
            OPEN_ALERT | VALIDATE_ALERT =>
            {
                let resources = crate::kernel::resources().ok_or(NOT_SUPPORTED)?;
                let mut alert = dialogs::load(resources, call)?;
                call.item = alert.items.len() as u16;
                if call.operation == VALIDATE_ALERT
                {
                    return Ok(());
                }
                if self.dialogs.active()
                {
                    return Err(SERVICE_BUSY);
                }
                if alert.position == 0x300a
                {
                    // Inside Macintosh positions the structure in the upper fifth of
                    // the desktop. Obtain its insets from WDEF, never duplicate them.
                    let entry =
                        self.definitions.resolve(*b"WDEF", 0).map_err(crate::loader::status)?;
                    let mut state = definitions::WindowDefinitionState::new(alert.bounds, "");
                    let mut definition =
                        DefinitionCall { header:    Header::for_type::<DefinitionCall>(),
                                         kind:      definitions::WDEF,
                                         id:        0,
                                         variation: 1,
                                         message:   definitions::CALCULATE,
                                         reserved:  0,
                                         param:     0,
                                         object:    core::ptr::from_mut(&mut state).cast(),
                                         surface:   core::ptr::null_mut(),
                                         gate:      crate::machine::traps::gateway };
                    let status =
                        crate::toolbox::resources::definitions::invoke(entry, &mut definition);
                    if status < 0
                    {
                        return Err(status as Status);
                    }
                    let r = state.structure;
                    alert.bounds.offset((640 - (r.right - r.left)) / 2 - r.left,
                                        20 + (460 - (r.bottom - r.top)) / 5 - r.top);
                }
                let mut request = WindowRequest { id:             0,
                                                  bounds:         alert.bounds,
                                                  title:          [0; WINDOW_TITLE_BYTES],
                                                  title_length:   0,
                                                  procedure:      1,
                                                  minimum_width:  1,
                                                  minimum_height: 1,
                                                  reserved:       0 };
                let previous = self.windows.active_window();
                checked(self.window(NEW_WINDOW, &mut request))?;
                call.window = request.id;
                self.dialogs = dialogs::DialogState { previous,
                                                      window: request.id,
                                                      alert: Some(alert),
                                                      ..Default::default() };
                // DrawGrowIcon is an explicit classic operation even on a
                // non-resizable WDEF. Dialogs suppress automatic grow drawing.
                request.bounds = Rect::ZERO;
                request.reserved = 0;
                let drawing =
                    checked(self.window(SET_GROW_ICON_CLIP, &mut request)).and_then(|()| {
                                                                              self.draw_dialog()
                                                                          });
                if let Err(error) = drawing
                {
                    let _ = self.close_dialog();
                    return Err(error);
                }
            }
            CLOSE =>
            {
                if !self.dialogs.active() || self.dialogs.window != call.window
                {
                    return Err(NOT_FOUND);
                }
                self.close_dialog()?;
            }
            HANDLE_EVENT | DRAW =>
            {
                if !self.dialogs.active() || self.dialogs.window != call.window
                {
                    return Err(NOT_FOUND);
                }
                if call.operation == DRAW
                {
                    self.draw_dialog()?;
                }
                else
                {
                    let (item, changed) = self.dialogs.event(call.event);
                    call.item = item;
                    if changed
                       || call.event.kind == EventKind::Update && call.event.message == call.window
                    {
                        self.draw_dialog()?;
                    }
                }
            }
            _ => return Err(NOT_SUPPORTED)
        }
        Ok(())
    }

    fn close_dialog(&mut self) -> Result<(), Status>
    {
        let previous = self.dialogs.previous;
        let mut request = WindowRequest { id:             self.dialogs.window,
                                          bounds:         Rect::ZERO,
                                          title:          [0; WINDOW_TITLE_BYTES],
                                          title_length:   0,
                                          procedure:      1,
                                          minimum_width:  1,
                                          minimum_height: 1,
                                          reserved:       0 };
        checked(self.window(CLOSE_WINDOW, &mut request))?;
        if let Some(previous) = previous.filter(|id| self.windows.window(*id).is_some())
        {
            request.id = previous.0;
            checked(self.window(SELECT_WINDOW, &mut request))?;
        }
        else if previous.is_none()
        {
            checked(self.window(SELECT_DESKTOP, &mut request))?;
        }
        // The dialog consumed activation notifications while modal. Suppress
        // transient focus from disposal/restoration, then deliver the final owner.
        while self.events.next_matching(1 << EventKind::Activate as u16).is_some()
        {}
        if let Some(active) = self.windows.active_window()
        {
            self.events.post_system(Event { payload:   0,
                                            kind:      EventKind::Activate,
                                            message:   active.0,
                                            when:      Self::when(),
                                            where_:    self.events.mouse,
                                            modifiers: self.events.modifiers | ACTIVE_FLAG });
        }
        self.dialogs = Default::default();
        Ok(())
    }

    fn dialog_shape(&mut self, window: u32, bounds: Rect, shape: u32, verb: u32)
                    -> Result<(), Status>
    {
        self.graphics(&mut GraphicsCall { operation: QD_SHAPE,
                                          target:
                                              mac_abi::quickdraw::Target::window(WindowId(window)),
                                          bounds,
                                          shape,
                                          verb,
                                          corner: Point { h: 16,
                                                          v: 16 },
                                          ..Default::default() },
                      &[],
                      &[])
    }

    fn dialog_text(&mut self, window: u32, item: &dialogs::Item) -> Result<(), Status>
    {
        let fonts = crate::kernel::fonts().ok_or(NOT_SUPPORTED)?;
        let selection = FontSelection::default();
        let metrics = fonts.metrics(fonts_service::selection(selection))?;
        let advance =
            metrics.ascent.saturating_add(metrics.descent).saturating_add(metrics.leading).max(1);
        let mut y = item.bounds.top.saturating_add(metrics.ascent);
        let mut line = String::new();
        // Preserve explicit Mac paragraphs, then wrap at resource-defined width.
        for paragraph in item.text.split(['\r', '\n'])
        {
            for word in paragraph.split(' ')
            {
                let candidate = if line.is_empty()
                {
                    String::from(word)
                }
                else
                {
                    alloc::format!("{} {}", line, word)
                };
                if fonts.text_width(fonts_service::selection(selection), &candidate)?
                   > item.bounds.right - item.bounds.left - 2
                   && !line.is_empty()
                {
                    self.font_draw(&mut FontCall { target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                                   selection,
                                                   baseline: Point { h: item.bounds
                                                                            .left
                                                                            .saturating_add(1),
                                                                     v: y },
                                                   ..Default::default() },
                                   &line,
                                   fonts)?;
                    y = y.saturating_add(advance);
                    line.clear();
                }
                if !line.is_empty()
                {
                    line.push(' ');
                }
                line.push_str(word);
            }
            if y <= item.bounds.bottom.saturating_add(2)
            {
                self.font_draw(&mut FontCall { target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                               selection,
                                               baseline: Point { h: item.bounds
                                                                        .left
                                                                        .saturating_add(1),
                                                                 v: y },
                                               ..Default::default() },
                               &line,
                               fonts)?;
            }
            line.clear();
            y = y.saturating_add(advance);
        }
        Ok(())
    }

    fn draw_dialog(&mut self) -> Result<(), Status>
    {
        let window = self.dialogs.window;
        let alert = self.dialogs.alert.clone().ok_or(NOT_FOUND)?;
        let mut request = WindowRequest { id:             window,
                                          bounds:         Rect::ZERO,
                                          title:          [0; WINDOW_TITLE_BYTES],
                                          title_length:   0,
                                          procedure:      1,
                                          minimum_width:  1,
                                          minimum_height: 1,
                                          reserved:       0 };
        checked(self.window(BEGIN_UPDATE, &mut request))?;
        let result = (|| {
            self.graphics(&mut GraphicsCall { operation: QD_RESET,
                                              target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                              ..Default::default() },
                          &[],
                          &[])?;
            self.dialog_shape(window,
                              Rect::new(0,
                                        0,
                                        alert.bounds.right - alert.bounds.left,
                                        alert.bounds.bottom - alert.bounds.top),
                              SHAPE_RECT,
                              VERB_ERASE)?;
            for (index, item) in alert.items.iter().enumerate()
            {
                match item.kind
                {
                    4 =>
                    {
                        let mut standard =
                            definitions::ControlDefinitionState::new(item.bounds, &item.text);
                        standard.enabled = u32::from(item.enabled);
                        standard.highlighted = u32::from(self.dialogs.pressed == Some(index)
                                                         && self.dialogs.highlighted);
                        checked(self.control(&mut ControlCall { operation: DRAW_STANDARD,
                                                                window,
                                                                standard,
                                                                ..Default::default() }))?;
                        if index == 0 && item.enabled
                        {
                            let mut ring = item.bounds;
                            ring.inset(-4, -4);
                            self.graphics(&mut GraphicsCall { operation: QD_PEN_SIZE,
                                                              target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                                              point: Point { h: 3,
                                                                             v: 3 },
                                                              ..Default::default() },
                                          &[],
                                          &[])?;
                            self.dialog_shape(window, ring, SHAPE_ROUND_RECT, VERB_FRAME)?;
                            self.graphics(&mut GraphicsCall { operation: QD_PEN_SIZE,
                                                              target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                                              point: Point { h: 1,
                                                                             v: 1 },
                                                              ..Default::default() },
                                          &[],
                                          &[])?;
                        }
                    }
                    8 =>
                    {
                        let clip = self.content_mut(window)?.port.wire().clip;
                        self.graphics(&mut GraphicsCall { operation: QD_CLIP_RECT,
                                                          target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                                          bounds: item.bounds,
                                                          ..Default::default() },
                                      &[],
                                      &[])?;
                        let result = self.dialog_text(window, item);
                        self.graphics(&mut GraphicsCall { operation: QD_CLIP_RECT,
                                                          target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                                          bounds: clip,
                                                          ..Default::default() },
                                      &[],
                                      &[])?;
                        result?;
                    }
                    32 =>
                    {
                        let call =
                            IconCall { target:
                                           mac_abi::quickdraw::Target::window(WindowId(window)),
                                       scope: resources::CHAIN,
                                       id: item.icon,
                                       destination: item.bounds,
                                       ..Default::default() };
                        let drawing=icons_service::Drawing::new(crate::kernel::resources().ok_or(NOT_SUPPORTED)?,&call,service::Port::default())?;
                        self.icon(&call, drawing)?;
                    }
                    _ => return Err(NOT_SUPPORTED)
                }
            }
            Ok(())
        })();
        checked(self.window(END_UPDATE, &mut request))?;
        result
    }
}
