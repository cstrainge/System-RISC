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
    dialogs:      crate::toolbox::dialogs::DialogState,
    menus:        crate::toolbox::menus::Manager,
    cursors:      crate::toolbox::quickdraw::cursors::Cursors,
    worlds:       crate::toolbox::quickdraw::worlds::Worlds,
    classic_font: mac_abi::quickdraw::FontSelection,
    events:       Box<EventManager>,
    textedit:     crate::toolbox::textedit::Manager
}
impl Saved
{
    pub fn new(id: u64, mdef: DefinitionEntry) -> Self
    {
        Self { id,
               crash: Default::default(),
               dialogs: Default::default(),
               menus: crate::toolbox::menus::Manager::new(mdef, crate::machine::traps::gateway),
               cursors: Default::default(),
               worlds: Default::default(),
               classic_font: Default::default(),
               events: Box::new(EventManager::new()),
               textedit: Default::default() }
    }
}
impl Desktop
{
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
    pub fn wake_waiters(&mut self, waiting: &[(u64, u16); 8]) -> ([bool; 8], u64)
    {
        let pointer_activity = self.poll_inputs();
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
        (ready, self.foreground)
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
        swap!(worlds);
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
        self.window_owners.iter().find(|(window, _)| *window == id).map(|(_, app)| *app)
    }

    pub fn owns(&self, id: WindowId) -> bool
    {
        self.window_owner(id) == Some(self.owner)
    }

    pub fn front_owned(&self) -> Option<WindowId>
    {
        self.windows.back_to_front().filter(|w| self.owns(w.id)).last().map(|w| w.id)
    }

    pub fn route_events(&mut self)
    {
        while let Some((event, _)) = self.events.next_system()
        {
            let id = match event.kind
            {
                EventKind::OsEvent if event.message == mac_abi::events::WINDOW_GEOMETRY_CHANGED =>
                {
                    Some(WindowId(event.payload as u32))
                }
                EventKind::Activate | EventKind::Update | EventKind::MouseDown
                    if event.message != 0 =>
                {
                    Some(WindowId(event.message))
                }
                EventKind::OsEvent if event.message & 0xff000000 != 0 =>
                {
                    Some(WindowId(event.message & 0xffffff))
                }
                _ => None
            };
            let target = id.and_then(|id| self.window_owner(id)).unwrap_or(self.owner);
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
        for (window, owner) in &self.window_owners
        {
            if *owner == id
            {
                self.windows.set_hidden(*window, false);
            }
        }
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
        for (window, owner) in &self.window_owners
        {
            if *owner == id
            {
                self.windows.set_hidden(*window, true);
            }
        }
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
                for (window, _) in &self.window_owners
                {
                    self.windows.set_hidden(*window, false);
                }
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
        let thread = match crate::machine::scheduler::spawn(id, image, entry)
        {
            Ok(t) => t,
            Err(e) =>
            {
                self.processes.remove(id)?;
                let _ = crate::kernel::with_files(|files| files.close_all(id));
                return Err(e);
            }
        };
        self.locals.push(Saved::new(id, self.mdef));
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
                let walk = mac_object_table::walk::drain(self, &ledger,
                    |_, key| crate::toolbox::handles::registry(|registry| registry.dispatch(key.kind()))
                        .and_then(|result| result.map_err(crate::toolbox::handles::KindAccessError::Registry)),
                    |key, error| serial!("OWNER_CLEANUP_QUARANTINED: owner={} kind={} slot={} generation={} {error}",
                        key.owner(), key.kind(), key.slot(), key.generation()))
                    .unwrap_or_else(|error| panic!("owner cleanup invariant failed: {error}"));
                let remaining = ledger.summary();
                serial!("OWNER_WALK app={} invalidated={} released={} quarantined={} live={} \
                         pending={} deferred={}",
                        caller,
                        walk.invalidated,
                        walk.released,
                        walk.quarantined,
                        remaining.live,
                        remaining.pending,
                        walk.deferred);
                let ids: Vec<_> = self.window_owners
                                      .iter()
                                      .filter(|(_, id)| *id == caller)
                                      .map(|(w, _)| *w)
                                      .collect();
                for window in ids
                {
                    self.windows.dispose_without_activation(window, &mut self.events, Self::when());
                    self.content.retain(|c| c.id != window);
                    self.pending_content.retain(|c| c.id != window);
                }
                self.route_events();
                self.window_owners.retain(|(_, id)| *id != caller);
                let previous = self.processes.previous(caller);
                self.enter(previous);
                let feedback = self.drag.feedback();
                self.drag.close_owner(caller, Self::when());
                self.drag_effects(feedback);
                self.locals.retain(|s| s.id != caller);
                let retirement = self.processes.remove(caller);
                self.notify_finder_file_state(file.volume, file.cnid, false);
                if self.foreground == caller
                {
                    self.activate_process(previous)?;
                }
                else
                {
                    self.refresh_app_menu();
                }
                super::super::processes::documents::close_owner(&mut self.processes, caller);
                let _ = crate::kernel::with_files(|files| files.close_all(caller));
                crate::kernel::release_memory();
                crate::machine::scheduler::retire_current();
                retirement?;
                serial!("PROCESS_EXIT app={}", caller);
            }
            _ => return Err(NOT_SUPPORTED)
        }
        Ok(())
    }
}
