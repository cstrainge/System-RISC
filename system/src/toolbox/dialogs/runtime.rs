//! Desktop adapter; no application callbacks or recursive ECALLs under its guard.
use mac_abi::{controls::*, dialogs::*, quickdraw::*, *};

use crate::toolbox::{dialogs,
                     quickdraw::{fonts_service, icons_service, service}};
fn checked(status: Status) -> Result<(), Status>
{
    if status == OK { Ok(()) } else { Err(status) }
}
fn request(window: u64, bounds: Rect) -> WindowRequest
{
    WindowRequest { id: window,
                    bounds,
                    title: [0; WINDOW_TITLE_BYTES],
                    title_length: 0,
                    procedure: 1,
                    minimum_width: 1,
                    minimum_height: 1,
                    reserved: 0 }
}
// Initialization owns the protocol before any window or control is published.
// Every fallible exit clears the parent, even if a child WDEF refuses disposal.
struct InitializingAlert<'a>
{
    desktop:   &'a mut super::Desktop,
    committed: bool
}
impl Drop for InitializingAlert<'_>
{
    fn drop(&mut self)
    {
        if !self.committed
        {
            if let Err(status) = self.desktop.close_dialog()
            {
                serial!("ALERT_INITIALIZATION_CHILD_REFUSED: owner={} status={status}; parent \
                         protocol released, child cleanup retained by Window Manager",
                        self.desktop.owner);
            }
        }
    }
}
/// Temporarily select the one System Alert store while preserving the normal
/// renderer and tracking implementation. Nested entries already see that store;
/// restoration is scoped and cannot escape to application code.
struct CoreDialogs<'a>
{
    desktop: &'a mut super::Desktop
}
impl Drop for CoreDialogs<'_>
{
    fn drop(&mut self)
    {
        core::mem::swap(&mut self.desktop.dialogs, &mut self.desktop.core_dialogs);
        self.desktop.core_dialogs_active = false;
    }
}

impl super::Desktop
{
    pub(crate) fn with_core_dialogs<R>(&mut self, operation: impl FnOnce(&mut Self) -> R) -> R
    {
        if self.core_dialogs_active
        {
            return operation(self);
        }
        core::mem::swap(&mut self.dialogs, &mut self.core_dialogs);
        self.core_dialogs_active = true;
        let restore = CoreDialogs { desktop: self };
        operation(&mut *restore.desktop)
    }

    /// Cancel the actual core protocol for this retained session, without
    /// selecting or foregrounding its dead recipient. Window Manager owns any
    /// refused child-disposal ticket and retains the Activity through real ACK.
    pub(crate) fn cancel_core_alert_for_recovery(&mut self, activity: &crate::recovery::Activity)
                                                 -> Result<(), Status>
    {
        activity.run(|| {
                    self.with_core_dialogs(|desktop| {
                            let Some(record) = desktop.dialogs.take_recovery(activity)?
                            else
                            {
                                // Protocol cancellation already completed. This does not
                                // declare its independently retained Window children ACKed.
                                return Ok(());
                            };
                            let result = if record.window == 0
                            {
                                Ok(())
                            }
                            else
                            {
                                checked(desktop.window(CLOSE_WINDOW,
                                                       &mut request(record.window, Rect::ZERO)))
                            };
                            drop(record);
                            result
                        })
                })?
    }

    pub(crate) fn prepare_core_dialogs(&mut self) -> Result<(), Status>
    {
        self.with_core_dialogs(|desktop| desktop.dialogs.prepare_recovery())
    }

    pub(super) fn dialog_input(&mut self, input: crate::toolbox::events::input::Input, when: u32)
    {
        use crate::toolbox::events::{BTN_STATE, input::Input};
        let window = self.dialogs.window;
        let event = match input
        {
            Input::Move(point) =>
            {
                self.events.mouse = point;
                if !self.dialogs.system_menu
                {
                    return;
                }
                Event { kind: EventKind::Null,
                        window,
                        when,
                        where_: point,
                        modifiers: self.events.modifiers,
                        ..Default::default() }
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
                        message: 0,
                        window,
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
                        modifiers: self.events.modifiers,
                        window: 0 }
            }
        };
        if self.dialogs.system_menu
        {
            let mut event = event;
            // About has one dismissal button; Escape also dismisses it.
            if matches!(event.kind, EventKind::KeyDown | EventKind::AutoKey)
               && event.character() == 27
            {
                event.message = (event.message & !0xff) | u32::from(b'\r');
            }
            if let Err(status) = self.service_system_menu_dialog(event)
            {
                serial!("ABOUT_SYSTEM_INPUT_FAILED status={status}; protocol released or owned \
                         cleanup retained");
            }
        }
        else
        {
            self.events.post_system(event);
        }
    }

    pub(super) fn show_about_system(&mut self) -> Result<(), Status>
    {
        if self.owner != self.foreground
        {
            return Err(ACCESS_DENIED);
        }
        if self.dialogs.active()
           || self.core_dialogs.active()
           || self.standard_file.active(self.owner).is_some()
           || (self.crash.info().window.0 != 0
               && self.crash.info().dismissed == 0
               && self.windows.window(self.crash.info().window).is_some())
        {
            return Err(SERVICE_BUSY);
        }
        let mut call = DialogCall { operation: OPEN_ALERT,
                                    scope: resources::SYSTEM,
                                    resource: -16463,
                                    ..Default::default() };
        let version = env!("CARGO_PKG_VERSION").as_bytes();
        call.text[..version.len()].copy_from_slice(version);
        call.text_length = version.len() as u32;
        let account = self.processes.allocation(self.owner)?;
        crate::allocator::with_target(crate::allocator::Target::Owner {
            account, class: mac_kernel_model::policy::MemoryClass::Kernel
        }, || {
            self.dialog_result(&mut call)?;
            self.dialogs.state_mut()?.system_menu = true;
            Ok(())
        })
    }

    /// Physical input and exposure may run on another application's polling
    /// thread. Keep menu-dialog allocations charged to its recorded recipient.
    pub(super) fn service_system_menu_dialog(&mut self, event: Event) -> Result<(), Status>
    {
        let account = self.processes.allocation(self.owner)?;
        crate::allocator::with_target(crate::allocator::Target::Owner {
            account, class: mac_kernel_model::policy::MemoryClass::Kernel
        }, || {
            let result = if event.kind == EventKind::Update
            {
                self.draw_dialog().map(|_| 0)
            }
            else { self.alert_event(event) };
            match result
            {
                Ok(0) => Ok(()),
                Ok(_) => self.close_dialog(),
                Err(original) => {
                    if let Err(cleanup) = self.close_dialog()
                    {
                        serial!("ABOUT_SYSTEM_CLOSE_FAILED original={original} cleanup={cleanup}; protocol released, Window Manager retains child cleanup");
                    }
                    Err(original)
                }
            }
        })
    }

    pub fn dialog(&mut self, call: &mut DialogCall) -> Status
    {
        // System handles its own menu dialog; app modal loops cannot consume it.
        if self.dialogs.system_menu
        {
            return SERVICE_BUSY;
        }
        self.dialog_result(call).map_or_else(|s| s, |()| OK)
    }

    fn dialog_result(&mut self, call: &mut DialogCall) -> Result<(), Status>
    {
        if call.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        call.item = 0;
        match call.operation
        {
            OPEN_ALERT | VALIDATE_ALERT =>
            {
                let mut alert = if dialogs::core_alerts::supported(call.scope, call.resource)
                {
                    dialogs::core_alerts::instantiate(call)?
                }
                else
                {
                    crate::kernel::with_resources_in(call.scope, |resources| {
                        dialogs::load(resources, call)
                    })??
                };
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
                    alert.bounds.offset((crate::display::WIDTH as i16 - (r.right - r.left)) / 2
                                        - r.left,
                                        20
                                        + ((crate::display::HEIGHT as i16 - 20)
                                           - (r.bottom - r.top))
                                          / 5
                                        - r.top);
                }
                let previous = self.windows.active_window();
                self.dialogs.begin(alert, previous)?;
                let mut guard = InitializingAlert { desktop:   self,
                                                    committed: false };
                guard.desktop.initialize_alert()?;
                call.window = guard.desktop.dialogs.window;
                guard.committed = true;
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
                    call.item = self.alert_event(call.event)?;
                    if call.event.kind == EventKind::Update && call.event.window == call.window
                    {
                        self.draw_dialog()?;
                    }
                }
            }
            _ => return Err(NOT_SUPPORTED)
        }
        Ok(())
    }

    fn initialize_alert(&mut self) -> Result<(), Status>
    {
        let bounds = self.dialogs.alert.as_ref().ok_or(NOT_FOUND)?.bounds;
        let mut request = request(0, bounds);
        checked(self.window(NEW_WINDOW, &mut request))?;
        self.dialogs.state_mut()?.window = request.id;
        // DrawGrowIcon is explicit even on non-resizable classic WDEFs.
        request.bounds = Rect::ZERO;
        checked(self.window(SET_GROW_ICON_CLIP, &mut request))?;
        let alert = self.dialogs.alert.clone().ok_or(NOT_FOUND)?;
        for (index, item) in alert.items.iter().enumerate().filter(|(_, item)| item.kind == 4)
        {
            let mut standard = definitions::ControlDefinitionState::new(item.bounds, &item.text);
            standard.enabled = u32::from(item.enabled);
            if index == 0 && item.enabled
            {
                standard.bounds.inset(-4, -4);
                standard.flags = definitions::DEFAULT_BUTTON;
            }
            let mut call = ControlCall { operation: CREATE_STANDARD,
                                         window: request.id,
                                         standard,
                                         ..Default::default() };
            self.managed_control(&mut call)?;
            self.dialogs.state_mut()?.controls[index] = Some(call.id);
        }
        self.draw_dialog()
    }

    fn alert_event(&mut self, event: Event) -> Result<u16, Status>
    {
        let alert = self.dialogs.alert.as_ref().ok_or(NOT_FOUND)?;
        let point = Point { h: event.where_.h.saturating_sub(alert.bounds.left),
                            v: event.where_.v.saturating_sub(alert.bounds.top) };
        let window = self.dialogs.window;
        if event.kind == EventKind::MouseDown && event.window == window
        {
            for index in 0..self.dialogs.controls.len()
            {
                let Some(id) = self.dialogs.controls[index]
                else
                {
                    continue;
                };
                let mut call = ControlCall { operation: BEGIN_TRACKING,
                                             id,
                                             window,
                                             point,
                                             ticks: u64::from(event.when),
                                             ..Default::default() };
                self.managed_control(&mut call)?;
                if call.flags & TRACKING != 0
                {
                    self.dialogs.state_mut()?.tracking = Some(index);
                    break;
                }
            }
        }
        else if matches!(event.kind, EventKind::MouseUp | EventKind::Null)
        {
            if let Some(index) = self.dialogs.tracking
            {
                let id = self.dialogs.controls[index].ok_or(BAD_ABI)?;
                let down = event.kind != EventKind::MouseUp
                           && event.modifiers & crate::toolbox::events::BTN_STATE == 0;
                let mut call = ControlCall { operation: POLL_TRACKING,
                                             id,
                                             window,
                                             point,
                                             ticks: u64::from(event.when),
                                             flags: if down { BUTTON_DOWN } else { 0 },
                                             ..Default::default() };
                self.managed_control(&mut call)?;
                if call.flags & TRACKING == 0
                {
                    self.dialogs.state_mut()?.tracking = None;
                }
                if call.flags & ACTION != 0
                {
                    return Ok(index as u16 + 1);
                }
            }
        }
        else if matches!(event.kind, EventKind::KeyDown | EventKind::AutoKey)
        {
            let chosen = match event.character()
            {
                b'\r' | 3 => Some(0),
                27 => Some(1),
                _ => None
            };
            if let Some(index) = chosen.filter(|&index| {
                                           self.dialogs.alert.as_ref().is_some_and(|alert| {
                                                                          alert.items
                                                                         .get(index)
                                                                         .is_some_and(|item| {
                                                                             item.kind == 4
                                                                             && item.enabled
                                                                         })
                                                                      })
                                       })
            {
                self.cancel_alert_tracking()?;
                return Ok(index as u16 + 1);
            }
        }
        else if event.kind == EventKind::Activate && event.modifiers & ACTIVE_FLAG == 0
        {
            self.cancel_alert_tracking()?;
        }
        Ok(0)
    }

    fn cancel_alert_tracking(&mut self) -> Result<(), Status>
    {
        if let Some(index) = self.dialogs.tracking
        {
            let id = self.dialogs.controls[index].ok_or(BAD_ABI)?;
            let mut call = ControlCall { operation: CANCEL_TRACKING,
                                         id,
                                         ..Default::default() };
            self.managed_control(&mut call)?;
            self.dialogs.state_mut()?.tracking = None;
        }
        Ok(())
    }

    pub(super) fn close_dialog(&mut self) -> Result<(), Status>
    {
        let record = self.dialogs.take().ok_or(NOT_FOUND)?;
        if self.core_dialogs_active
           && let Some(activity) = crate::context::recovery_activity()
        {
            activity.close();
        }
        let result = self.finish_alert_window(record.window, record.previous);
        drop(record);
        result
    }

    fn finish_alert_window(&mut self, window: u64, previous: Option<WindowId>)
                           -> Result<(), Status>
    {
        if window == 0
        {
            return Ok(());
        }
        let result = checked(self.window(CLOSE_WINDOW, &mut request(window, Rect::ZERO)));
        let restored = self.restore_alert_focus(previous);
        result.and(restored)
    }

    pub(super) fn restore_alert_focus(&mut self, previous: Option<WindowId>) -> Result<(), Status>
    {
        let mut call = request(0, Rect::ZERO);
        let result = if let Some(previous) =
            previous.filter(|id| self.windows.window(*id).is_some())
        {
            call.id = previous.0;
            checked(self.window(SELECT_WINDOW, &mut call))
        }
        else if previous.is_none()
        {
            checked(self.window(SELECT_DESKTOP, &mut call))
        }
        else
        {
            Ok(())
        };
        // Modal handling consumed transient activation; publish the final focus.
        while self.events.next_matching(1 << EventKind::Activate as u16).is_some()
        {}
        if let Some(active) = self.windows.active_window()
        {
            self.events.post_system(Event { payload:   0,
                                            kind:      EventKind::Activate,
                                            message:   0,
                                            window:    active.0,
                                            when:      Self::when(),
                                            where_:    self.events.mouse,
                                            modifiers: self.events.modifiers | ACTIVE_FLAG });
        }
        result
    }

    fn dialog_shape(&mut self, window: u64, bounds: Rect, shape: u32, verb: u32)
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

    fn dialog_text(&mut self, window: u64, item: &dialogs::Item) -> Result<(), Status>
    {
        let fonts = crate::kernel::fonts().ok_or(NOT_SUPPORTED)?;
        let selection = FontSelection::default();
        let metrics = fonts.metrics(fonts_service::selection(selection))?;
        let advance =
            metrics.ascent.saturating_add(metrics.descent).saturating_add(metrics.leading).max(1);
        let mut y = item.bounds.top.saturating_add(metrics.ascent);
        // Preserve the original paragraph/wrap rules without candidate copies.
        dialogs::wrapped_lines(&item.text,
                               item.bounds.right - item.bounds.left - 2,
                               |candidate| {
                                   fonts.text_width(fonts_service::selection(selection), candidate)
                               },
                               |line, wrapped| {
                                   if wrapped || y <= item.bounds.bottom.saturating_add(2)
                                   {
                                       self.font_draw(&mut FontCall { target: mac_abi::quickdraw::Target::window(WindowId(window)),
                                                                       selection,
                                                                       baseline: Point { h: item.bounds.left.saturating_add(1), v: y },
                                                                       ..Default::default() },
                                                       line,
                                                       fonts)?;
                                   }
                                   y = y.saturating_add(advance);
                                   Ok(())
                               })?;
        Ok(())
    }

    /// Discard only this System-owned paint transaction. Its committed scene and
    /// hover bindings remain live; consumed full-update damage is restored for retry.
    pub(super) fn cancel_system_window_update(&mut self, id: WindowId) -> Result<(), Status>
    {
        let window = self.windows.window(id).ok_or(NOT_FOUND)?;
        if !self.owns(id)
        {
            return Err(ACCESS_DENIED);
        }
        let bounds = window.local_bounds();
        let index =
            self.pending_content.iter().position(|content| content.id == id).ok_or(BAD_ARGUMENT)?;
        let full = !self.pending_content[index].is_patch();
        self.pending_content.swap_remove(index);
        self.textedit.begin_cursor_update(id);
        self.windows.end_update(id);
        if full
        {
            self.windows.invalidate(id, bounds);
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
                        let id = self.dialogs.controls[index].ok_or(BAD_ABI)?;
                        self.managed_control(&mut ControlCall { operation: DRAW_OBJECT,
                                                                id,
                                                                ..Default::default() })?;
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
                        let drawing = crate::kernel::with_resources(|resources| {
                                          icons_service::Drawing::new(resources,
                                                                      &call,
                                                                      service::Port::default())
                                      }).ok_or(NOT_SUPPORTED)??;
                        self.icon(&call, drawing)?;
                    }
                    _ => return Err(NOT_SUPPORTED)
                }
            }
            Ok(())
        })();
        let finished = if result.is_ok()
        {
            checked(self.window(END_UPDATE, &mut request))
        }
        else
        {
            self.cancel_system_window_update(WindowId(window))
        };
        if let (Err(original), Err(cleanup)) = (result, finished)
        {
            serial!("ALERT_PAINT_CANCEL_REFUSED window={} original={} cleanup={}", window,
                    original, cleanup);
        }
        result?;
        finished
    }
}

impl super::Desktop
{
    pub(crate) fn release_alert(&mut self, ticket: mac_object_table::domain::ReleasedResource)
                                -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        let owner = ticket.key().owner().incarnation();
        let Some(manager) = self.alerts_for_owner(owner)
        else
        {
            return Err(<mac_object_table::domain::CompletionError as mac_object_table::registry::CleanupFailure>::registration_mismatch(ticket).into());
        };
        let record = manager.release(ticket)?;
        if owner == self.owner
        {
            if let Err(status) = self.finish_alert_window(record.window, record.previous)
            {
                serial!("ALERT_CLEANUP_CHILD_REFUSED: owner={owner} status={status}; parent \
                         released, Window Manager retains child cleanup");
            }
        }
        // Saved-context cleanup never foregrounds another app. Its registered
        // windows and controls are independently released by the same walk.
        drop(record);
        Ok(())
    }

    pub(crate) fn release_alert_storage(&mut self,
                                        ticket: mac_object_table::domain::ReleasedResource)
                                        -> Result<(), mac_object_table::domain::CompletionError>
    {
        let owner = ticket.key().owner().incarnation();
        match self.alerts_for_owner(owner) {
            Some(manager)=>manager.release_storage(ticket),
            None=>Err(<mac_object_table::domain::CompletionError as mac_object_table::registry::CleanupFailure>::registration_mismatch(ticket))
        }
    }
}
