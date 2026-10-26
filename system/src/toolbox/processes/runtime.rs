//! App-local state is swapped only at the interrupt-masked service boundary.
//! The compositor/window registry are shared; applications never select identity.
#![forbid(unsafe_code)]
use alloc::{boxed::Box, vec::Vec};

use mac_abi::{processes::*, *};

use crate::toolbox::{events::EventManager,
                     processes::menu::{Action, Entry},
                     runtime::Desktop,
                     windows::{GroupFocus, WindowId}};

pub(super) struct Saved
{
    id:           u64,
    crash:        crate::toolbox::crashes::Manager,
    dialogs:      crate::toolbox::dialogs::Alerts,
    menus:        crate::toolbox::menus::Manager,
    cursors:      crate::toolbox::quickdraw::cursors::Cursors,
    classic_font: mac_abi::quickdraw::FontSelection,
    events:       Box<EventManager>,
    textedit:     crate::toolbox::textedit::Manager
}
impl Saved
{
    pub fn try_new(owner: crate::toolbox::handles::OwnerRef, mdef: DefinitionEntry)
                   -> Result<Self, Status>
    {
        let id = owner.owner().incarnation();
        Ok(Self { id,
                  crash: Default::default(),
                  dialogs: crate::toolbox::dialogs::Alerts::new(crate::toolbox::handles::alert_kinds()?, owner.clone())?,
                  menus:
                      crate::toolbox::menus::Manager::new(crate::toolbox::handles::menu_kinds()?,
                                                          mdef,
                                                          crate::machine::traps::gateway)?,
                  cursors: Default::default(),
                  classic_font: Default::default(),
                  events: Box::try_new(EventManager::new()).map_err(|_| NO_MEMORY)?,
                  textedit: crate::toolbox::textedit::Manager::new(crate::toolbox::handles::editor_kinds()?, owner)? })
    }
}

/// A real provisional owner and saved service context, not an invented neutral
/// process. Lost preparations stop admission and enter the ordinary reaper.
pub(crate) struct FinderAdmission
{
    old:      u64,
    owner:    crate::toolbox::handles::OwnerRef,
    original: crate::images::Binding,
    active:   bool
}
impl FinderAdmission
{
    pub(crate) fn old(&self) -> u64
    {
        self.old
    }

    pub(crate) fn owner(&self) -> &crate::toolbox::handles::OwnerRef
    {
        &self.owner
    }

    pub(crate) fn id(&self) -> u64
    {
        self.owner.owner().incarnation()
    }

    pub(crate) fn original(&self) -> crate::images::Binding
    {
        self.original
    }

    pub(crate) fn started(&mut self)
    {
        self.active = false;
    }
}
impl Drop for FinderAdmission
{
    fn drop(&mut self)
    {
        if self.active
        {
            self.owner.stop_admission();
            crate::toolbox::processes::retirement::resources_released();
        }
    }
}
impl Desktop
{
    pub(crate) fn reserve_finder(&mut self, old: u64) -> Result<FinderAdmission, Status>
    {
        if old != crate::context::finder_app().0
        {
            return Err(ACCESS_DENIED);
        }
        let binding = self.processes.resource_binding(old)?;
        let original = binding.image().ok_or(BAD_ABI)?;
        self.locals.try_reserve(1).map_err(|_| NO_MEMORY)?;
        self.enter(old);
        let snapshot = self.crash.snapshot()?;
        let id = self.processes.provision("Finder", 0, 0)?;
        let owner = self.processes.ledger(id)?;
        let mut local = match Saved::try_new(owner.clone(), self.mdef)
        {
            Ok(local) => local,
            Err(status) =>
            {
                self.retire_unstarted(id)?;
                return Err(status);
            }
        };
        local.crash = crate::toolbox::crashes::Manager::from_snapshot(snapshot);
        self.locals.push(local);
        Ok(FinderAdmission { old,
                             owner,
                             original,
                             active: true })
    }

    pub(crate) fn bind_finder_image(&mut self, admission: &FinderAdmission,
                                    image: crate::images::Binding)
                                    -> Result<(), Status>
    {
        if admission.old != crate::context::finder_app().0
        {
            return Err(SERVICE_BUSY);
        }
        self.processes.bind_image(admission.id(), image)
    }

    pub(crate) fn commit_finder_handoff(&mut self, admission: &FinderAdmission)
                                        -> Result<(), Status>
    {
        let old = admission.old;
        let next = admission.id();
        if !self.locals.iter().any(|local| local.id == next)
           || self.processes.get(next)?.image.is_none()
        {
            return Err(BAD_ABI);
        }
        // Resolve every required owner/capability before changing the role.
        let file = self.processes.snapshot(old)?;
        let ledger = self.processes.ledger(old)?;
        let checkpoint = self.processes.cleanup_checkpoint(old)?;
        let binding = self.processes.resource_binding(old)?;
        crate::context::replace_finder_app(old, &admission.owner)?;
        self.stop_process(file, ledger, checkpoint, binding, Some(next));
        serial!("FINDER_HANDOFF old={old} new={next} state=waiting-for-old-stack");
        Ok(())
    }

    pub(crate) fn finder_started(&mut self, admission: &FinderAdmission, thread: u64)
                                 -> Result<(), Status>
    {
        let id = admission.id();
        self.processes.get_mut(id)?.info.thread = thread;
        self.enter(id);
        // A temporary System recovery report never leaves the healthy Finder's
        // zero-mask waits permanently ready after it starts.
        let window = self.crash.info().window;
        if window.0 != 0
        {
            let status = self.window(CLOSE_WINDOW, &mut window_request(window));
            if status != OK && status != NOT_FOUND
            {
                return Err(status);
            }
        }
        self.crash = Default::default();
        self.activate_process(id)?;
        self.refresh_app_menu();
        serial!("PROCESS_REGISTERED app={id} name=Finder");
        serial!("FINDER_RECOVERED old={} new={id} thread={thread}", admission.old);
        Ok(())
    }

    pub(super) fn alerts_for_owner(&mut self, owner: u64)
                                   -> Option<&mut crate::toolbox::dialogs::Alerts>
    {
        if owner == self.owner
        {
            Some(&mut self.dialogs)
        }
        else
        {
            self.locals.iter_mut().find(|local| local.id == owner).map(|local| &mut local.dialogs)
        }
    }

    pub(crate) fn spawn_process(&self, id: u64, loaded: crate::kernel::LoadedImage)
                                -> Result<u64, Status>
    {
        crate::machine::scheduler::spawn(self.processes.ledger(id)?, loaded)
    }

    pub(super) fn post_drag_notice(&mut self, notice: crate::toolbox::drag::Notice)
    {
        if matches!(notice.event.message & 0xff00_0000,
                    mac_abi::drag::CANCEL_REQUESTED_EVENT | mac_abi::drag::FINISHED_EVENT)
        {
            super::super::processes::documents::cancel_drag(&mut self.processes,
                                                            notice.event.payload as u32);
        }
        let position = self.event_position(notice.event, notice.owner);
        if notice.owner == self.owner
        {
            self.events.post_drag(notice.event, position);
        }
        else if let Some(local) = self.locals.iter_mut().find(|s| s.id == notice.owner)
        {
            local.events.post_drag(notice.event, position);
        }
    }

    pub(crate) fn notify_document_batch(&mut self, owner: u64, handle: u64) -> Result<(), Status>
    {
        let event = Event { kind: EventKind::OsEvent,
                            message: mac_abi::processes::DOCUMENT_AVAILABLE,
                            payload: u64::from(handle),
                            when: Self::when(),
                            ..Default::default() };
        if owner == self.owner
        {
            self.events.post_transfer(event);
        }
        else if let Some(local) = self.locals.iter_mut().find(|s| s.id == owner)
        {
            local.events.post_transfer(event);
        }
        else
        {
            return Err(NOT_FOUND);
        }
        Ok(())
    }

    pub(crate) fn notify_file_transfer(&mut self, owner: u64, handle: u64) -> Result<(), Status>
    {
        let event = Event { kind: EventKind::OsEvent,
                            message: mac_abi::events::FILE_TRANSFER_CHANGED,
                            payload: u64::from(handle),
                            when: Self::when(),
                            ..Default::default() };
        if owner == self.owner
        {
            self.events.post_transfer(event);
        }
        else if let Some(local) = self.locals.iter_mut().find(|s| s.id == owner)
        {
            local.events.post_transfer(event);
        }
        else
        {
            return Err(NOT_FOUND);
        }
        Ok(())
    }

    pub(super) fn forget_cursor_window(&mut self, window: WindowId)
    {
        self.textedit.forget_cursor_window(window);
        for local in &mut self.locals
        {
            local.textedit.forget_cursor_window(window);
        }
    }

    pub(super) fn observe_cursor_motion(&mut self, point: Point)
    {
        self.cursor_dirty |= self.cursors.mouse_moved(point);
        for local in &mut self.locals
        {
            local.cursors.mouse_moved(point);
        }
    }

    /// Advance only the foreground cursor, without swapping app-local UI state
    /// or posting synthetic events. The timer presents through the cursor overlay.
    pub(super) fn tick_cursor(&mut self) -> bool
    {
        self.drag_tick();
        let now = Self::when();
        let changed = if self.owner == self.foreground
        {
            self.cursors.animation_tick(now)
        }
        else
        {
            self.locals
                .iter_mut()
                .find(|local| local.id == self.foreground)
                .is_some_and(|local| local.cursors.animation_tick(now))
        };
        self.cursor_dirty |= changed;
        changed
    }

    /// Read waiting queues in place. A timer tick must not swap each app's UI
    /// state just to find out whether its thread can remain asleep.
    pub fn wake_waiters(&mut self, waiting: &[(u64, u16); 8]) -> ([bool; 8], u64, bool)
    {
        let previous_activity = self.activity;
        let pointer_activity = self.poll_inputs();
        let fresh_input = previous_activity != self.activity;
        let cursor_activity = self.tick_cursor();
        let now = Self::when();
        self.cursor_dirty |=
            self.responsiveness
                .tick(self.foreground, now, waiting.iter().any(|(app, _)| *app == self.foreground));
        if self.responsiveness.active()
        {
            self.cursor_dirty |= self.busy_cursor.animation_tick(now);
        }
        self.present_cursor_overlay();
        self.route_events();
        let pointer_consumer = if waiting.iter().any(|(app, _)| *app == self.foreground)
        {
            self.foreground
        }
        else
        {
            waiting.iter().find(|(app, _)| *app != 0).map_or(0, |(app, _)| *app)
        };
        let when = Self::when();
        let ready = core::array::from_fn(|index| {
            let (app, mask) = waiting[index];
            if app == 0
            {
                return false;
            }
            if (pointer_activity || cursor_activity) && app == pointer_consumer
            {
                return true;
            }
            let (events, dialogs, crash) = if app == self.owner
            {
                (&*self.events, &self.dialogs, &self.crash)
            }
            else if let Some(local) = self.locals.iter().find(|local| local.id == app)
            {
                (&*local.events, &local.dialogs, &local.crash)
            }
            else
            {
                return false;
            };
            // A dismissed System-owned report wakes the parked application
            // even though its handler deliberately waits with an empty event mask.
            if crash.info().dismissed != 0
            {
                return true;
            }
            if events.peek_matching(mask).is_some()
            {
                return true;
            }
            if mask & (1 << EventKind::Update as u16) == 0
            {
                return false;
            }
            if dialogs.active()
            {
                self.windows.next_update_for(WindowId(dialogs.window), events, when).is_some()
            }
            else
            {
                self.windows
                    .next_update_matching(events, when, |id| self.window_owner(id) == Some(app))
                    .is_some()
            }
        });
        (ready, self.foreground, fresh_input)
    }

    pub fn started(&mut self, id: u64, thread: u64)
    {
        if let Ok(p) = self.processes.get_mut(id)
        {
            p.info.thread = thread;
        }
        serial!("PROCESS_REGISTERED app={} name=Finder", id);
        self.refresh_app_menu();
    }

    pub fn enter(&mut self, id: u64)
    {
        if self.owner == id
        {
            return;
        }
        let Some(old) = self.locals.iter().position(|s| s.id == self.owner)
        else
        {
            return;
        };
        let Some(next) = self.locals.iter().position(|s| s.id == id)
        else
        {
            return;
        };
        // Queues are app-local; physical input is machine state. A newly
        // prepared context must not restore its default pointer/modifier values.
        let physical = (self.events.mouse, self.events.modifiers, self.events.keys);
        macro_rules! swap {
            ($field:ident) => {
                core::mem::swap(&mut self.$field, &mut self.locals[old].$field);
                core::mem::swap(&mut self.$field, &mut self.locals[next].$field);
            };
        }
        swap!(crash);
        swap!(dialogs);
        swap!(menus);
        swap!(cursors);
        swap!(classic_font);
        swap!(events);
        swap!(textedit);
        self.events.mouse = physical.0;
        self.events.modifiers = physical.1;
        self.events.keys = physical.2;
        self.owner = id;
    }

    pub fn enter_caller(&mut self)
    {
        self.enter(crate::context::current_thread_context().identity().app.0);
    }

    pub fn window_owner(&self, id: WindowId) -> Option<u64>
    {
        self.windows.owner(id)
    }

    pub fn owns(&self, id: WindowId) -> bool
    {
        self.window_owner(id) == Some(self.owner)
    }

    pub fn front_owned(&self) -> Option<WindowId>
    {
        self.windows.front_for_owner(self.owner)
    }

    pub fn route_events(&mut self)
    {
        while let Some((event, _, captured_owner)) = self.events.next_system()
        {
            let id = (event.window != 0).then_some(WindowId(event.window));
            let target = if captured_owner != 0
            {
                captured_owner
            }
            else
            {
                id.and_then(|id| self.window_owner(id)).unwrap_or(self.owner)
            };
            crate::trace::record(crate::trace::ROUTE,
                                 self.owner,
                                 target,
                                 event.kind as u64,
                                 event.message as u64,
                                 event.modifiers as u64);
            let position = self.event_position(event, target);
            if target == self.owner
            {
                // post_positioned clears the trusted-routing flag. Requeue
                // immediately: next_system skips it, preserving the same FIFO
                // result without allocating a temporary Vec per input event.
                self.events.post_positioned(event, position);
            }
            else if let Some(saved) = self.locals.iter_mut().find(|s| s.id == target)
            {
                saved.events.post_positioned(event, position);
            }
        }
    }

    pub fn sync_input_state(&mut self)
    {
        let mouse = self.events.mouse;
        let modifiers = self.events.modifiers;
        let keys = self.events.keys;
        for local in &mut self.locals
        {
            local.events.mouse = mouse;
            local.events.modifiers = modifiers;
            local.events.keys = keys;
        }
    }

    fn application_icon(index: Option<crate::images::Binding>) -> mac_abi::menus::MenuIcon
    {
        let fallback = mac_abi::menus::MenuIcon::new(mac_abi::resources::SYSTEM, -3996);
        let Some(index) = index
        else
        {
            return fallback;
        };
        let scope = mac_abi::resources::Scope::image(index.id());
        crate::kernel::with_image(index, |image| {
            if let Ok(id) = crate::toolbox::quickdraw::icons_service::file_id(&image.resources,
                                                                              image.creator,
                                                                              image.file_type)
            {
                return mac_abi::menus::MenuIcon::new(scope, id);
            }
            crate::kernel::with_resources(|manager| {
                let mut call = mac_abi::quickdraw::IconCall { operation:
                                                                  mac_abi::quickdraw::ICON_FILE,
                                                              scope: mac_abi::resources::SYSTEM,
                                                              creator: image.creator,
                                                              file_type: image.file_type,
                                                              ..Default::default() };
                crate::toolbox::quickdraw::icons_service::query(manager, &mut call).ok()
                    .map(|_| mac_abi::menus::MenuIcon::new(call.origin, call.id))
            }).flatten()
              .unwrap_or(fallback)
        }).unwrap_or(fallback)
    }

    /// Lifecycle changes notify file identity; Finder damages only that icon.
    fn notify_finder_file_state(&mut self, volume: u32, cnid: u32, running: bool)
    {
        self.notify_inspectors();
        let event = Event { kind: EventKind::OsEvent,
                            message: FILE_STATE_EVENT | u32::from(running),
                            payload: u64::from(volume) | (u64::from(cnid) << 32),
                            when: Self::when(),
                            ..Default::default() };
        if self.owner == crate::context::finder_app().0
        {
            self.events.post(event);
        }
        else if let Some(local) =
            self.locals.iter_mut().find(|local| local.id == crate::context::finder_app().0)
        {
            local.events.post(event);
        }
    }

    /// One coalesced notice per membership change. No timers, subscriptions to
    /// raw input, or per-inspector snapshot allocation. Background apps leave
    /// the advisory notice queued until their next foreground refresh.
    pub(crate) fn notify_inspectors(&mut self)
    {
        use crate::toolbox::inspection::Subject;
        let event = Event { kind: EventKind::OsEvent,
                            message: mac_abi::inspection::CHANGED_EVENT,
                            when: Self::when(),
                            ..Default::default() };
        for process in self.processes.iter()
        {
            if process.authority() & mac_abi::inspection::INSPECT_DOMAINS == 0
            {
                continue;
            }
            if process.info.id == self.owner
            {
                self.events.post_transfer(event);
            }
            else if let Some(local) =
                self.locals.iter_mut().find(|local| local.id == process.info.id)
            {
                local.events.post_transfer(event);
            }
        }
    }

    fn refresh_app_menu(&mut self)
    {
        self.menu_cache.invalidate();
        let entries: Vec<_> = self.processes
                                  .iter()
                                  .map(|p| {
                                      Entry { id:     p.info.id,
                                              name:   p.info.name(),
                                              hidden: p.info.hidden != 0,
                                              icon:   Self::application_icon(p.image) }
                                  })
                                  .collect();
        if self.processes.get(self.foreground).is_ok_and(|process| process.info.thread == 0)
        {
            // A real saved recovery context can park the dying shell before
            // its replacement thread is published. No stale menu actions live
            // during that short, explicitly suspended presentation.
            self.app_menu.suspend();
            self.redraw = true;
            return;
        }
        if let Err(status) = self.app_menu.refresh(&entries, self.foreground)
        {
            serial!("APPLICATION_MENU_FAILED status={}", status);
        }
        self.redraw = true;
    }

    pub fn activate_process(&mut self, id: u64) -> Result<(), Status>
    {
        self.activate_process_focus(id, GroupFocus::Front)
    }

    pub fn activate_process_window(&mut self, id: u64, front: Option<WindowId>)
                                   -> Result<(), Status>
    {
        self.activate_process_focus(id, front.map_or(GroupFocus::Desktop, GroupFocus::Window))
    }

    fn activate_process_focus(&mut self, id: u64, focus: GroupFocus) -> Result<(), Status>
    {
        if let GroupFocus::Window(window) = focus
           && self.window_owner(window) != Some(id)
        {
            return Err(NOT_FOUND);
        }
        crate::trace::record(crate::trace::ACTIVATE, self.owner, id, self.foreground, 0, 0);
        self.processes.get(id)?;
        if self.foreground == id
           && self.processes.get(id)?.info.hidden == 0
           && focus == GroupFocus::Front
        {
            return Ok(());
        }
        self.processes.activate(id)?;
        self.foreground = id;
        self.cursor_dirty |= self.responsiveness.foreground(id, Self::when());
        self.windows.set_owner_hidden(id, false);
        let ids: Vec<_> = self.windows
                              .back_to_front()
                              .filter(|w| self.window_owner(w.id) == Some(id))
                              .map(|w| w.id)
                              .collect();
        self.windows.select_group_focus(&ids, focus, &mut self.events, Self::when());
        self.route_events();
        self.sync_input_state();
        self.enter(id);
        self.refresh_app_menu();
        serial!("PROCESS_ACTIVE app={}", id);
        Ok(())
    }

    fn hide_process(&mut self, id: u64) -> Result<(), Status>
    {
        if self.processes.get(id)?.info.hidden != 0
        {
            return Ok(());
        }
        if !self.processes.iter().any(|p| p.info.id != id && p.info.hidden == 0)
        {
            return Err(ACCESS_DENIED);
        }
        self.processes.get_mut(id)?.info.hidden = 1;
        self.windows.set_owner_hidden(id, true);
        if self.foreground == id
        {
            let previous = self.processes.previous(id);
            self.activate_process(previous)?;
        }
        self.refresh_app_menu();
        Ok(())
    }

    pub fn application_action(&mut self, action: Action)
    {
        let result = match action
        {
            Action::Activate(id) => self.activate_process(id),
            Action::HideCurrent => self.hide_process(self.foreground),
            Action::HideOthers =>
            {
                let ids: Vec<_> = self.processes
                                      .iter()
                                      .filter(|p| p.info.id != self.foreground)
                                      .map(|p| p.info.id)
                                      .collect();
                for id in ids
                {
                    let _ = self.hide_process(id);
                }
                Ok(())
            }
            Action::ShowAll =>
            {
                let changed = self.processes.iter().any(|p| p.info.hidden != 0);
                for p in self.processes.values_mut()
                {
                    p.info.hidden = 0;
                }
                self.windows.set_all_hidden(false);
                if changed
                {
                    self.refresh_app_menu();
                }
                Ok(())
            }
        };
        if let Err(status) = result
        {
            serial!("PROCESS_ACTION_FAILED status={}", status);
        }
    }

    /// Until coordinated Quit/Cancel negotiation lands, do not discard another
    /// application's unsaved documents or interrupt its teardown walk.
    pub(crate) fn power_applications_ready(&self) -> Result<(), Status>
    {
        if self.processes.iter().any(|process| process.info.id != crate::context::finder_app().0)
        {
            Err(SERVICE_BUSY)
        }
        else
        {
            Ok(())
        }
    }

    pub(crate) fn activate_existing_application(&mut self, call: &mut ProcessCall)
                                                -> Result<bool, Status>
    {
        let existing = self.processes
                           .iter()
                           .find(|p| p.info.volume == call.volume && p.info.cnid == call.cnid)
                           .map(|p| p.info.id);
        if let Some(id) = existing
        {
            self.activate_process(id)?;
            call.info = self.processes.snapshot(id)?;
            return Ok(true);
        }
        // Reload would overwrite the old process's writable image. Refuse
        // while its stack or quarantined callbacks still retain that image.
        if self.processes.file_stopping(call.volume, call.cnid)
        {
            return Err(SERVICE_BUSY);
        }
        if self.processes.len() >= 8
        {
            return Err(NO_MEMORY);
        }
        Ok(false)
    }

    pub(crate) fn launch_application(&mut self, call: &mut ProcessCall,
                                     loaded: crate::toolbox::files::application::Loaded)
                                     -> Result<(), Status>
    {
        // Another caller may have launched this file while its fork was read.
        if self.activate_existing_application(call)?
        {
            return Ok(());
        }
        self.locals.try_reserve(1).map_err(|_| NO_MEMORY)?;
        let crate::toolbox::files::application::Loaded { catalog,
                                                         resources } = loaded;
        // Storage reads finish before provisioning: a deferred I/O replay must
        // not create another child. Image admission now has a genuine subject
        // domain ready for its registered aliases and future account charges.
        let id = self.processes.provision(catalog.name(), call.volume, call.cnid)?;
        let prepared = (|| {
            let authority = self.processes.ledger(id)?;
            let loaded = crate::kernel::load_application(&authority, catalog.creator,
                                                         catalog.file_type, resources)?;
            self.processes.bind_image(id, loaded.binding())?;
            let local = Saved::try_new(authority.clone(), self.mdef)?;
            Ok::<_, Status>((loaded, local, authority))
        })();
        let (loaded, local, authority) = match prepared
        {
            Ok(local) => local,
            Err(error) =>
            {
                self.retire_unstarted(id)?;
                return Err(error);
            }
        };
        // RAM-only pin admission closes launch-vs-transfer races. No storage
        // operation or provider callback runs under this desktop guard.
        let pinned = crate::kernel::with_files(|files| {
                         files.pin_executable(&authority, call.volume, call.cnid)
                     }).map_err(crate::state::AccessError::status)
                       .and_then(|r| r);
        if let Err(error) = pinned
        {
            self.locals.push(local);
            self.retire_unstarted(id)?;
            return Err(error);
        }
        let thread = match self.spawn_process(id, loaded)
        {
            Ok(t) => t,
            Err(e) =>
            {
                // Keep saved managers reachable for the same owner walk used
                // by EXIT. A refused cleanup retains its domain for retry.
                self.locals.push(local);
                self.retire_unstarted(id)?;
                return Err(e);
            }
        };
        self.locals.push(local);
        self.processes.get_mut(id)?.info.thread = thread;
        self.activate_process(id)?;
        call.info = self.processes.snapshot(id)?;
        self.notify_finder_file_state(call.volume, call.cnid, true);
        serial!("PROCESS_LAUNCHED app={} thread={} name={}", id, thread, catalog.name());
        Ok(())
    }

    fn retire_unstarted(&mut self, owner: u64) -> Result<(), Status>
    {
        let ledger = self.processes.ledger(owner)?;
        ledger.stop_admission();
        let checkpoint = self.processes.cleanup_checkpoint(owner)?;
        let binding = self.processes.resource_binding(owner)?;
        serial!("PROCESS_LAUNCH_ABORT app={owner} resource_scope={}",
                if binding.image().is_some() { "application" } else { "system-only" });
        match crate::kernel::with_process_resources(&binding, || {
                  checkpoint.run(&ledger, || self.walk_owner(&ledger, false))
              })
        {
            Ok(Ok(_)) => (),
            Ok(Err(error)) =>
            {
                serial!("LAUNCH_CLEANUP_RETAINED: owner={owner} {error}; {checkpoint}")
            }
            Err(status) =>
            {
                serial!("LAUNCH_CLEANUP_RETAINED: owner={owner} status={status}; subject resource \
                         context unavailable")
            }
        }
        crate::toolbox::processes::retirement::resources_released();
        Ok(())
    }

    fn walk_owner(&mut self, owner: &crate::toolbox::handles::OwnerRef, retry: bool)
                  -> Result<mac_object_table::walk::Report, mac_object_table::walk::Stopped>
    {
        let resolve = |_: &(dyn crate::toolbox::handles::cleanup::context::Context + 'static),
                       key: mac_object_table::domain::ResourceKey| {
            crate::toolbox::handles::registry(|registry| registry.dispatch(key.kind()))
                .and_then(|result| result.map_err(crate::toolbox::handles::KindAccessError::Registry))
        };
        let failed = |key: mac_object_table::domain::ResourceKey, error: &mac_object_table::walk::Failure<
            crate::toolbox::handles::KindAccessError, crate::toolbox::handles::cleanup::Cause>| {
            serial!("OWNER_CLEANUP_QUARANTINED: owner={} kind={} slot={} generation={} {error}",
                key.owner(), key.kind(), key.slot(), key.generation());
        };
        if retry
        {
            mac_object_table::walk::retry(self as &mut dyn crate::toolbox::handles::cleanup::context::Context, owner, resolve, failed)
        }
        else
        {
            mac_object_table::walk::drain(self as &mut dyn crate::toolbox::handles::cleanup::context::Context, owner, resolve, failed)
        }
    }

    pub fn process(&mut self, call: &mut ProcessCall) -> Status
    {
        let caller = crate::context::current_thread_context().identity().app.0;
        self.process_request(call, caller).map_or_else(|e| e, |_| OK)
    }

    fn process_request(&mut self, call: &mut ProcessCall, caller: u64) -> Result<(), Status>
    {
        match call.operation
        {
            CURRENT => call.info = self.processes.snapshot(caller)?,
            IS_RUNNING =>
            {
                call.index =
                    u32::from(self.processes
                                  .iter()
                                  .any(|p| {
                                      p.info.volume == call.volume && p.info.cnid == call.cnid
                                  }))
            }
            COUNT => call.index = self.processes.len() as u32,
            INDEX =>
            {
                let id = self.processes.iter().nth(call.index as usize).ok_or(NOT_FOUND)?.info.id;
                call.info = self.processes.snapshot(id)?;
            }
            // Storage must pass through processes::request before acquiring
            // this desktop guard, including requests from the classic adapter.
            LAUNCH => return Err(BAD_ABI),
            ACTIVATE => self.activate_process(call.id)?,
            HIDE => self.hide_process(call.id)?,
            HIDE_OTHERS => self.application_action(Action::HideOthers),
            SHOW_ALL => self.application_action(Action::ShowAll),
            EXIT =>
            {
                if caller == crate::context::finder_app().0
                {
                    return Err(ACCESS_DENIED);
                }
                let file = self.processes.snapshot(caller)?;
                let ledger = self.processes.ledger(caller)?;
                let checkpoint = self.processes.cleanup_checkpoint(caller)?;
                let binding = self.processes.resource_binding(caller)?;
                self.stop_process(file, ledger, checkpoint, binding, None);
            }
            _ => return Err(NOT_SUPPORTED)
        }
        Ok(())
    }

    /// Both normal exit and shell recovery use this owner's one teardown walk.
    /// All required capabilities are resolved before admission is stopped.
    fn stop_process(&mut self, file: ProcessInfo, ledger: crate::toolbox::handles::OwnerRef,
                    checkpoint: crate::toolbox::handles::cleanup::Checkpoint,
                    binding: crate::toolbox::processes::ResourceBinding, replacement: Option<u64>)
    {
        let caller = file.id;
        self.enter(caller);
        ledger.stop_admission();
        crate::machine::scheduler::retire_owner(caller);
        serial!("PROCESS_STOPPED app={caller} thread={}", file.thread);
        let before = ledger.summary();
        let walk = crate::kernel::with_process_resources(&binding, || {
            checkpoint.run(&ledger, || self.walk_owner(&ledger, false))
        });
        match walk
        {
            Ok(Ok(walk)) =>
            {
                let remaining = ledger.summary();
                serial!("OWNER_WALK app={} invalidated={} released={} quarantined={} live={} \
                         pending={} deferred={} completed={}",
                        caller,
                        walk.invalidated,
                        walk.released,
                        walk.quarantined,
                        remaining.live,
                        remaining.pending,
                        walk.deferred,
                        remaining.deaths.saturating_sub(before.deaths));
            }
            Ok(Err(error)) => serial!("OWNER_CLEANUP_RETAINED app={caller} {error}; {checkpoint}"),
            Err(status) =>
            {
                serial!("OWNER_CLEANUP_RETAINED app={caller} status={status}; subject resource \
                         context unavailable")
            }
        }
        self.route_events();
        let previous = replacement.unwrap_or_else(|| self.processes.previous(caller));
        self.enter(previous);
        self.notify_finder_file_state(file.volume, file.cnid, false);
        if self.foreground == caller
        {
            if let Err(status) = self.activate_process(previous)
            {
                serial!("PROCESS_ACTIVATION_RETAINED app={previous} status={status}; saved \
                         context remains admitted");
            }
        }
        else
        {
            self.refresh_app_menu();
        }
        // Saved state and the original image remain until all real barriers close.
        crate::toolbox::processes::retirement::resources_released();
    }

    pub(crate) fn evict_image_origin(&mut self, origin: u64)
    {
        self.definitions.evict_origin(origin);
    }

    pub(crate) fn reap_processes(&mut self)
    {
        crate::kernel::finish_image_origins(self);
        let mut previous = 0;
        while let Some(owner) = self.processes.stopping_after(previous)
        {
            previous = owner;
            if crate::machine::scheduler::has_threads(owner)
            {
                continue;
            }
            let Ok(ledger) = self.processes.ledger(owner)
            else
            {
                continue;
            };
            // One bounded retry per genuine reclamation checkpoint. Failed
            // tickets and saved managers survive; input/timer ticks do not
            // repeatedly retry a permanently refusing definition.
            let Ok(checkpoint) = self.processes.cleanup_checkpoint(owner)
            else
            {
                continue;
            };
            let remaining = ledger.summary();
            if checkpoint.ensure_settled().is_err()
               || remaining.quarantined != 0
               || remaining.live != 0
               || remaining.completion_faults != 0
            {
                let binding = match self.processes.resource_binding(owner)
                {
                    Ok(binding) => binding,
                    Err(status) =>
                    {
                        serial!("PROCESS_RETIRE_RETAINED app={owner} status={status}; cleanup \
                                 resource binding unavailable, saved manager state retained");
                        continue;
                    }
                };
                let retry = crate::kernel::with_process_resources(&binding, || {
                    if remaining.live != 0
                    {
                        // A refused initial invocation may leave both live
                        // objects and older quarantined tickets. Drain the
                        // live part, then retry quarantine once in this same
                        // genuine checkpoint; neither half needs input to run.
                        checkpoint.run(&ledger, || self.walk_owner(&ledger, false))?;
                    }
                    checkpoint.run(&ledger, || self.walk_owner(&ledger, true))
                });
                match retry
                {
                    Ok(Ok(_)) => (),
                    Ok(Err(error)) =>
                    {
                        serial!("PROCESS_RETIRE_RETAINED app={owner} {error}; {checkpoint}");
                        continue;
                    }
                    Err(status) =>
                    {
                        serial!("PROCESS_RETIRE_RETAINED app={owner} status={status}; cleanup \
                                 resource context unavailable, saved manager state retained");
                        continue;
                    }
                }
            }
            // The checkpoint can run on the idle/boot context. Provider pumps
            // may have selected the dying app's saved managers on the previous
            // trap; park them in their entry before removing that entry.
            if self.owner == owner
            {
                self.enter(self.processes.previous(owner));
                if self.owner == owner
                {
                    serial!("PROCESS_RETIRE_RETAINED app={owner}; cannot park active saved \
                             manager state in a surviving process context");
                    continue;
                }
            }
            // A partial asynchronous ACK is progress, not a failed retirement.
            // Retained uses and the common walk own their wakeups; do not ask
            // the directory to reject the same pending barrier or enumerate
            // every pool for a diagnostic on each child completion.
            let remaining = ledger.summary();
            if remaining.live != 0 || remaining.pending != 0 || remaining.completion_faults != 0
            {
                continue;
            }
            if let Err(status) = self.processes.remove(owner)
            {
                let remaining = ledger.summary();
                serial!("PROCESS_RETIRE_RETAINED app={owner} status={status} live={} pending={} \
                         quarantined={}; saved manager state retained for cleanup retry",
                        remaining.live,
                        remaining.pending,
                        remaining.quarantined);
                #[cfg(feature = "event-trace")]
                for at in 0..remaining.kinds
                {
                    let Ok(pool) = ledger.pool_at(remaining.revision, at)
                    else
                    {
                        break;
                    };
                    if pool.live != 0 || pool.pending != 0
                    {
                        serial!("PROCESS_RETIRE_POOL app={owner} kind={} live={} pending={} \
                                 scoped={}",
                                pool.kind,
                                pool.live,
                                pool.pending,
                                pool.scoped_children);
                    }
                }
                continue;
            }
            // enter(previous) in EXIT saved the dying owner's actual managers
            // in this entry. No pending payload/provider pin is abandoned.
            self.locals.retain(|state| state.id != owner);
            serial!("PROCESS_EXIT app={owner}");
        }
    }
}

impl crate::toolbox::files::watch::lifetime::WatchHost for Desktop
{
    fn release_file_watch(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                          storage: bool)
                          -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        match crate::kernel::with_files_owned(ticket, |ticket, files| {
                  crate::toolbox::files::watch::lifetime::WatchHost::release_file_watch(files,
                                                                                        ticket,
                                                                                        storage)
              })
        {
            Ok(result) => result,
            Err((error, ticket)) =>
            {
                Err(crate::toolbox::handles::cleanup::Failure::files_unavailable(ticket, error))
            }
        }
    }
}

impl crate::toolbox::files::forks::lifetime::ForkHost for Desktop
{
    fn release_file_fork(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                         storage: bool)
                         -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        match crate::kernel::with_files_owned(ticket, |ticket, files| {
                  crate::toolbox::files::forks::lifetime::ForkHost::release_file_fork(files,
                                                                                      ticket,
                                                                                      storage)
              })
        {
            Ok(result) => result,
            Err((error, ticket)) =>
            {
                Err(crate::toolbox::handles::cleanup::Failure::files_unavailable(ticket, error))
            }
        }
    }
}

impl crate::toolbox::files::write::lifetime::WriteHost for Desktop
{
    fn release_file_write(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                          storage: bool)
                          -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        match crate::kernel::with_files_owned(ticket, |ticket, files| {
                  crate::toolbox::files::write::lifetime::WriteHost::release_file_write(files,
                                                                                        ticket,
                                                                                        storage)
              })
        {
            Ok(result) => result,
            Err((error, ticket)) =>
            {
                Err(crate::toolbox::handles::cleanup::Failure::files_unavailable(ticket, error))
            }
        }
    }
}

impl crate::toolbox::menus::lifetime::MenuHost for Desktop
{
    fn release_menu(&mut self, ticket: mac_object_table::domain::ReleasedResource)
                    -> Result<(), mac_object_table::domain::CompletionError>
    {
        let owner = ticket.key().owner().incarnation();
        let manager = if self.owner == owner
        {
            Some(&mut self.menus)
        }
        else
        {
            self.locals.iter_mut().find(|state| state.id == owner).map(|state| &mut state.menus)
        };
        match manager { Some(manager) => manager.release(ticket), None => Err(<mac_object_table::domain::CompletionError as mac_object_table::registry::CleanupFailure>::registration_mismatch(ticket)) }
    }

    fn release_menu_storage(&mut self, ticket: mac_object_table::domain::ReleasedResource)
                            -> Result<(), mac_object_table::domain::CompletionError>
    {
        let owner = ticket.key().owner().incarnation();
        let manager = if self.owner == owner
        {
            Some(&mut self.menus)
        }
        else
        {
            self.locals.iter_mut().find(|state| state.id == owner).map(|state| &mut state.menus)
        };
        match manager { Some(manager) => manager.release_storage(ticket), None => Err(<mac_object_table::domain::CompletionError as mac_object_table::registry::CleanupFailure>::registration_mismatch(ticket)) }
    }
}

impl crate::toolbox::memory::lifetime::MemoryHost for Desktop
{
    fn release_memory(&mut self, ticket: mac_object_table::domain::ReleasedResource, storage: bool)
                      -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        match crate::kernel::with_memory_owned(ticket, |ticket, manager| {
                  manager.release(ticket, storage)
              })
        {
            Ok(result) => result.map_err(Into::into),
            Err((error, ticket)) =>
            {
                Err(crate::toolbox::handles::cleanup::Failure::memory_unavailable(ticket, error))
            }
        }
    }
}

impl crate::toolbox::textedit::lifetime::EditorHost for Desktop
{
    fn release_editor(&mut self, ticket: mac_object_table::domain::ReleasedResource, storage: bool)
                      -> Result<(), mac_object_table::domain::CompletionError>
    {
        let owner = ticket.key().owner().incarnation();
        let manager = if owner == self.owner
        {
            Some(&mut self.textedit)
        }
        else
        {
            self.locals.iter_mut().find(|local| local.id == owner).map(|local| &mut local.textedit)
        };
        let result = match manager {
            Some(manager) => manager.release(ticket, storage),
            None => Err(<mac_object_table::domain::CompletionError as mac_object_table::registry::CleanupFailure>::registration_mismatch(ticket))
        };
        if result.is_ok() && !storage
        {
            self.cursor_dirty = true;
        }
        result
    }
}

impl crate::toolbox::files::streams::lifetime::StreamHost for Desktop
{
    fn release_file_stream(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                           storage: bool)
                           -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        match crate::kernel::with_files_owned(ticket, |ticket, files| {
                  crate::toolbox::files::streams::lifetime::StreamHost::release_file_stream(files,
                                                                                            ticket,
                                                                                            storage)
              })
        {
            Ok(result) => result,
            Err((error, ticket)) =>
            {
                Err(crate::toolbox::handles::cleanup::Failure::files_unavailable(ticket, error))
            }
        }
    }
}

impl crate::toolbox::files::groups::lifetime::GroupHost for Desktop
{
    fn release_file_group(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                          storage: bool)
                          -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        match crate::kernel::with_files_owned(ticket, |ticket, files| {
                  crate::toolbox::files::groups::lifetime::GroupHost::release_file_group(files,
                                                                                         ticket,
                                                                                         storage)
              })
        {
            Ok(result) => result,
            Err((error, ticket)) =>
            {
                Err(crate::toolbox::handles::cleanup::Failure::files_unavailable(ticket, error))
            }
        }
    }
}

impl crate::toolbox::files::transfer_lifetime::TransferHost for Desktop
{
    fn release_file_transfer(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                             storage: bool)
                             -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        match crate::kernel::with_files_owned(ticket, |ticket, files| {
                  crate::toolbox::files::transfer_lifetime::TransferHost::release_file_transfer(files,
                                                                                         ticket,
                                                                                         storage)
              })
        {
            Ok(result) => result,
            Err((error, ticket)) =>
            {
                Err(crate::toolbox::handles::cleanup::Failure::files_unavailable(ticket, error))
            }
        }
    }
}

impl crate::toolbox::files::pins::PinHost for Desktop
{
    fn release_file_pin(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                        kind: crate::toolbox::files::pins::Registration)
                        -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        match crate::kernel::with_files_owned(ticket, |ticket, files| {
                  crate::toolbox::files::pins::PinHost::release_file_pin(files, ticket, kind)
              })
        {
            Ok(result) => result,
            Err((error, ticket)) =>
            {
                Err(crate::toolbox::handles::cleanup::Failure::files_unavailable(ticket, error))
            }
        }
    }
}

fn window_request(id: WindowId) -> WindowRequest
{
    WindowRequest { id:             id.0,
                    bounds:         Rect::ZERO,
                    title:          [0; WINDOW_TITLE_BYTES],
                    title_length:   0,
                    procedure:      0,
                    minimum_width:  0,
                    minimum_height: 0,
                    reserved:       0 }
}
