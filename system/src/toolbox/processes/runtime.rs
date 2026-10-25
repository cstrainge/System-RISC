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
impl Desktop
{
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

    pub(crate) fn spawn_process(&self, id: u64, image: usize, entry: usize) -> Result<u64, Status>
    {
        crate::machine::scheduler::spawn(self.processes.ledger(id)?, image, entry)
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

    pub(crate) fn notify_document_batch(&mut self, owner: u64, handle: u32) -> Result<(), Status>
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

    pub(crate) fn notify_file_transfer(&mut self, owner: u64, handle: u32) -> Result<(), Status>
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

    fn application_icon(index: usize) -> mac_abi::menus::MenuIcon
    {
        let scope = mac_abi::resources::LOADED_IMAGE_BASE + index as i16;
        if let Some(image) = crate::kernel::image(index)
        {
            if let Ok(id) = crate::toolbox::quickdraw::icons_service::file_id(&image.resources,
                                                                              image.creator,
                                                                              image.file_type)
            {
                return mac_abi::menus::MenuIcon::new(scope, id);
            }
            if let Some(manager) = crate::kernel::resources()
            {
                let mut call = mac_abi::quickdraw::IconCall { operation:
                                                                  mac_abi::quickdraw::ICON_FILE,
                                                              scope: mac_abi::resources::SYSTEM,
                                                              creator: image.creator,
                                                              file_type: image.file_type,
                                                              ..Default::default() };
                if crate::toolbox::quickdraw::icons_service::query(manager, &mut call).is_ok()
                {
                    return mac_abi::menus::MenuIcon::new(call.origin, call.id);
                }
            }
        }
        mac_abi::menus::MenuIcon::new(0, -3996)
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
    fn notify_inspectors(&mut self)
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
        let crate::toolbox::files::application::Loaded { catalog,
                                                         resources } = loaded;
        let image = crate::kernel::cache_image(call.volume, call.cnid, catalog.creator,
                                               catalog.file_type, resources)?;
        let entry = crate::kernel::reload_image(image)?;
        let id = self.processes.add(catalog.name(), call.volume, call.cnid, image)?;
        let local = match Saved::try_new(self.processes.ledger(id)?, self.mdef)
        {
            Ok(local) => local,
            Err(error) =>
            {
                self.processes.remove(id)?;
                return Err(error);
            }
        };
        // RAM-only pin admission closes launch-vs-transfer races. No storage
        // operation or provider callback runs under this desktop guard.
        let pinned = crate::kernel::with_files(|files| {
                         files.pin_executable(id, call.volume, call.cnid)
                     }).map_err(crate::state::AccessError::status)
                       .and_then(|r| r);
        if let Err(error) = pinned
        {
            self.processes.remove(id)?;
            return Err(error);
        }
        let thread = match self.spawn_process(id, image, entry)
        {
            Ok(t) => t,
            Err(e) =>
            {
                self.processes.remove(id)?;
                let _ = crate::kernel::with_files(|files| files.close_all(id));
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
                self.enter(caller);
                let ledger = self.processes.ledger(caller)?;
                // Never resume the dying app, even if a cleanup callback or UI
                // transition below refuses. Its stack is reclaimed elsewhere.
                ledger.stop_admission();
                crate::machine::scheduler::retire_current();
                serial!("PROCESS_STOPPED app={caller} thread={}", file.thread);
                let before_walk = ledger.summary();
                let checkpoint = self.processes.cleanup_checkpoint(caller)?;
                let walk = checkpoint.run(&ledger, || mac_object_table::walk::drain(self, &ledger,
                    |_, key| crate::toolbox::handles::registry(|registry| registry.dispatch(key.kind()))
                        .and_then(|result| result.map_err(crate::toolbox::handles::KindAccessError::Registry)),
                    |key, error| serial!("OWNER_CLEANUP_QUARANTINED: owner={} kind={} slot={} generation={} {error}",
                        key.owner(), key.kind(), key.slot(), key.generation())));
                match walk
                {
                    Ok(walk) =>
                    {
                        let remaining = ledger.summary();
                        serial!("OWNER_WALK app={} invalidated={} released={} quarantined={} \
                                 live={} pending={} deferred={} completed={}",
                                caller,
                                walk.invalidated,
                                walk.released,
                                walk.quarantined,
                                remaining.live,
                                remaining.pending,
                                walk.deferred,
                                remaining.deaths.saturating_sub(before_walk.deaths));
                    }
                    Err(error) =>
                    {
                        serial!("OWNER_CLEANUP_RETAINED app={caller} {error}; {checkpoint}")
                    }
                }
                self.route_events();
                let previous = self.processes.previous(caller);
                self.enter(previous);
                self.notify_finder_file_state(file.volume, file.cnid, false);
                super::super::processes::documents::close_owner(&mut self.processes, caller);
                let _ = crate::kernel::with_files(|files| files.close_all(caller));
                if self.foreground == caller
                {
                    self.activate_process(previous)?;
                }
                else
                {
                    self.refresh_app_menu();
                }
                // Saved managers remain alive until the thread checkpoint
                // and the ledger's final retirement both succeed.
            }
            _ => return Err(NOT_SUPPORTED)
        }
        Ok(())
    }

    pub(crate) fn reap_processes(&mut self)
    {
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
            if !checkpoint.settled() || ledger.summary().quarantined != 0
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
                    checkpoint.run(&ledger, || mac_object_table::walk::retry(
                                                  self,
                                                  &ledger,
                                                  |_, key| {
                                                      crate::toolbox::handles::registry(|registry| registry.dispatch(key.kind()))
                        .and_then(|result| result.map_err(crate::toolbox::handles::KindAccessError::Registry))
                                                  },
                                                  |key, error| {
                                                      serial!("OWNER_CLEANUP_QUARANTINED: \
                                                               owner={} kind={} slot={} \
                                                               generation={} {error}",
                                                              key.owner(),
                                                              key.kind(),
                                                              key.slot(),
                                                              key.generation())
                                                  }
                    ))
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
            if let Err(status) = self.processes.remove(owner)
            {
                let remaining = ledger.summary();
                serial!("PROCESS_RETIRE_RETAINED app={owner} status={status} live={} pending={} \
                         quarantined={}; saved manager state retained for cleanup retry",
                        remaining.live,
                        remaining.pending,
                        remaining.quarantined);
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
