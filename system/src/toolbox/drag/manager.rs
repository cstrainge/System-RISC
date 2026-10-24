//! Resident drag lifetime, negotiation and lazy flavor storage. No app pointers.
#![forbid(unsafe_code)]
#[path = "classic.rs"]
mod classic;
use alloc::vec::Vec;

use mac_abi::{drag::*, *};

use super::super::handles::{Kind, Table};

struct Flavor
{
    kind:           [u8; 4],
    state:          u32, // 0=inline/fulfilled, 1=offered promise, 2=requested, 3=failed
    claimed:        bool,
    taken:          bool,
    native_claimed: bool,
    flags:          u32,
    declared:       usize,
    error:          Status,
    bytes:          Vec<u8>
}
struct Item
{
    id:      u32,
    flavors: Vec<Flavor>,
    bounds:  Rect
}
struct Session
{
    owner:               u64,
    source:              u64,
    thread:              u64,
    detached:            bool,
    action:              u32,
    destination:         u32,
    allowed_actions:     u32,
    file_volume:         Option<u32>,
    mixed_volumes:       bool,
    trash:               bool,
    source_read_only:    bool,
    file_folder:         Option<(u32, u32)>,
    icon_layout:         bool,
    application:         bool,
    eject:               bool,
    volume_root:         Option<u32>,
    file_types:          Vec<[u8; 4]>,
    file_types_complete: bool,
    folders:             bool,
    anchor:              Point,
    point:               Point,
    modifiers:           u16,
    down_modifiers:      u16,
    up_modifiers:        u16,
    phase:               u32,
    outcome:             u32,
    error:               Status,
    receiver:            Option<u32>,
    accepted:            bool,
    responded:           bool,
    released:            bool,
    cancel_requested:    bool,
    items:               Vec<Item>,
    common:              u32,
    started:             bool,
    bytes:               usize,
    bounds:              Option<Rect>,
    return_start:        Point,
    return_tick:         u32,
    return_step:         u32
}
impl Session
{
    fn resolved_action(&self) -> u32
    {
        if self.application
        {
            COPY
        }
        else if self.eject
                  || self.icon_layout
                  || self.trash
                  || self.modifiers & mac_abi::events::CMD_KEY != 0
        {
            MOVE
        }
        else if self.modifiers & mac_abi::events::OPTION_KEY != 0
                  || self.source_read_only
                  || self.mixed_volumes
                  || self.file_volume.is_some_and(|volume| volume != self.destination)
        {
            COPY
        }
        else
        {
            MOVE
        }
    }

    fn action_error(&self) -> Option<Status>
    {
        let action = self.resolved_action();
        if action == MOVE && self.source_read_only && !self.icon_layout && !self.eject
        {
            Some(READ_ONLY)
        }
        else if action == MOVE
                  && self.allowed_actions & SAME_VOLUME_MOVE != 0
                  && (self.mixed_volumes || self.file_volume != Some(self.destination))
        {
            Some(NOT_SUPPORTED)
        }
        else if self.allowed_actions & if action == COPY { ACCEPT_COPY } else { ACCEPT_MOVE } == 0
        {
            Some(NOT_SUPPORTED)
        }
        else
        {
            None
        }
    }
}
struct Receiver
{
    owner:   u64,
    window:  u64,
    thread:  u64,
    closing: bool
}
#[derive(Clone, Copy)]
pub struct Notice
{
    pub owner: u64,
    pub event: Event
}
pub struct Manager
{
    sessions:         Table<Session>,
    receivers:        Table<Receiver>,
    active:           Option<u32>,
    notices:          [Option<Notice>; 4],
    highlight:        Option<(u64, Rect)>,
    highlight_damage: [Option<Rect>; 2]
}
impl Default for Manager
{
    fn default() -> Self
    {
        Self { sessions:         Table::new(Kind::Drags),
               receivers:        Table::new(Kind::DragReceivers),
               active:           None,
               notices:          [None; 4],
               highlight:        None,
               highlight_damage: [None; 2] }
    }
}
fn copied(bytes: &[u8]) -> Result<Vec<u8>, Status>
{
    let mut result = Vec::new();
    result.try_reserve_exact(bytes.len()).map_err(|_| NO_MEMORY)?;
    result.extend_from_slice(bytes);
    Ok(result)
}
impl Manager
{
    fn notice(&mut self, owner: u64, prefix: u32, window: u64, id: u32, when: u32)
    {
        let session = self.sessions.get(id).expect("live drag notification");
        let notice = Notice { owner,
                              event: Event { kind: EventKind::OsEvent,
                                             message: prefix
                                                      | if prefix == FINISHED_EVENT
                                                      {
                                                          session.outcome
                                                      }
                                                      else if prefix == DROP_EVENT
                                                      {
                                                          session.action
                                                      }
                                                      else
                                                      {
                                                          session.items.len() as u32
                                                          | (session.common << 12)
                                                      },
                                             when,
                                             where_: session.point,
                                             modifiers: session.modifiers,
                                             payload: u64::from(id),
                                             window } };
        // Each input/request is drained before another manager operation.
        *self.notices.iter_mut().find(|n| n.is_none()).expect("bounded drag effects") =
            Some(notice);
    }

    pub fn next_notice(&mut self) -> Option<Notice>
    {
        for notice in &mut self.notices
        {
            if notice.is_some()
            {
                return notice.take();
            }
        }
        None
    }

    fn owner(&self, id: u32, owner: u64) -> Result<&Session, Status>
    {
        let session = self.sessions.get(id).filter(|s| !s.detached).ok_or(NOT_FOUND)?;
        if session.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        Ok(session)
    }

    fn readable(&self, id: u32, owner: u64) -> Result<&Session, Status>
    {
        let session = self.sessions.get(id).ok_or(NOT_FOUND)?;
        if (session.owner != owner || session.detached)
           && !session.receiver
                      .and_then(|id| self.receivers.get(id))
                      .is_some_and(|r| r.owner == owner)
        {
            return Err(ACCESS_DENIED);
        }
        Ok(session)
    }

    pub(crate) fn offer_live(&self, owner: u64, drag: u32) -> Result<(), Status>
    {
        if self.readable(drag, owner)?.phase != TRACKING { Err(ACCESS_DENIED) } else { Ok(()) }
    }

    pub(crate) fn accepts_documents(&self, owner: u64, drag: u32, types: &[[u8; 4]])
                                    -> Result<bool, Status>
    {
        let session = self.readable(drag, owner)?;
        if session.phase != TRACKING
        {
            return Err(ACCESS_DENIED);
        }
        Ok(session.file_types_complete
           && !session.folders
           && !session.file_types.is_empty()
           && session.file_types.iter().all(|kind| types.binary_search(kind).is_ok()))
    }

    fn receiver(&self, id: u32, receiver: u32, owner: u64) -> Result<(), Status>
    {
        let session = self.sessions.get(id).ok_or(NOT_FOUND)?;
        let registration = self.receivers.get(receiver).ok_or(NOT_FOUND)?;
        if registration.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        if session.receiver != Some(receiver)
        {
            return Err(NOT_FOUND);
        }
        Ok(())
    }

    pub fn receiver_at(&self, window: u64) -> Option<u32>
    {
        self.receivers.iter().find_map(|(id, r)| (r.window == window).then_some(id))
    }

    pub fn feedback(&self) -> Option<Rect>
    {
        let session = self.sessions.get(self.active?)?;
        let bounds = session.bounds?;
        if !matches!(session.phase, TRACKING | RETURNING)
        {
            return None;
        }
        let dh = i32::from(session.point.h) - i32::from(session.anchor.h);
        let dv = i32::from(session.point.v) - i32::from(session.anchor.v);
        let add =
            |n: i16, d: i32| (i32::from(n) + d).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        Some(Rect::new(add(bounds.left, dh),
                       add(bounds.top, dv),
                       add(bounds.right, dh),
                       add(bounds.bottom, dv)))
    }

    pub fn is_tracking(&self) -> bool
    {
        // A receiver may have a progress window after mouse-up. Deliver its
        // Stop clicks and ordinary window events through the normal UI path.
        self.active
            .and_then(|id| self.sessions.get(id))
            .is_some_and(|s| matches!(s.phase, ARMED | TRACKING | RETURNING))
    }

    pub fn ensure_receiver(&mut self, owner: u64, thread: u64, window: u64) -> Result<(), Status>
    {
        if let Some(id) = self.receiver_at(window)
        {
            let r = self.receivers.get(id).unwrap();
            return if r.owner == owner && r.thread == thread
            {
                Ok(())
            }
            else
            {
                Err(ACCESS_DENIED)
            };
        }
        self.receivers
            .insert(Receiver { owner,
                               thread,
                               window,
                               closing: false })
            .map_err(|_| NO_MEMORY)?;
        Ok(())
    }

    pub fn highlight(&self) -> Option<(u64, Rect)>
    {
        self.highlight
    }

    pub fn highlight_damage(&mut self) -> [Option<Rect>; 2]
    {
        core::mem::take(&mut self.highlight_damage)
    }

    fn set_highlight(&mut self, next: Option<(u64, Rect)>)
    {
        if next == self.highlight
        {
            return;
        }
        self.highlight_damage = [self.highlight.map(|(_, r)| r), next.map(|(_, r)| r)];
        self.highlight = next;
    }

    pub fn source_read_only(&mut self, id: u32)
    {
        if let Some(s) = self.sessions.get_mut(id)
        {
            s.source_read_only = true;
        }
    }

    fn leave(&mut self, id: u32, when: u32)
    {
        self.set_highlight(None);
        let old = self.sessions.get_mut(id).unwrap().receiver.take();
        if let Some((receiver_id, owner, window, closing)) =
            old.and_then(|id| self.receivers.get(id).map(|r| (id, r.owner, r.window, r.closing)))
        {
            // A pending take must wake when its receiver capability is revoked,
            // including source exit/cancellation before a promise is fulfilled.
            if self.sessions.get(id).unwrap().phase == DROPPING
               && self.sessions
                      .get(id)
                      .unwrap()
                      .items
                      .iter()
                      .flat_map(|item| &item.flavors)
                      .any(|flavor| flavor.state == 2)
            {
                self.notice(owner, DATA_READY_EVENT, window, id, when);
            }
            self.notice(owner, LEFT_EVENT, window, id, when);
            if closing
            {
                self.receivers.remove(receiver_id);
            }
        }
        let session = self.sessions.get_mut(id).unwrap();
        session.accepted = false;
        session.responded = false;
    }

    fn finish(&mut self, id: u32, when: u32)
    {
        let session = self.sessions.get_mut(id).unwrap();
        session.phase = FINISHED;
        let (owner, source) = (session.owner, session.source);
        self.active = None;
        let detached = session.detached;
        self.notice(owner, FINISHED_EVENT, source, id, when);
        if detached
        {
            self.sessions.remove(id);
        }
    }

    fn return_to_source(&mut self, id: u32, outcome: u32, error: Status, when: u32)
    {
        self.leave(id, when);
        let session = self.sessions.get_mut(id).unwrap();
        session.outcome = outcome;
        session.error = error;
        if session.bounds.is_some() && session.point != session.anchor
        {
            session.return_start = session.point;
            session.return_tick = when;
            session.return_step = 0;
            session.phase = RETURNING;
        }
        else
        {
            self.finish(id, when);
        }
    }

    fn drop_at_receiver(&mut self, id: u32, when: u32)
    {
        let session = self.sessions.get(id).unwrap();
        if session.accepted
           && let Some(r) = session.receiver.and_then(|r| self.receivers.get(r))
        {
            let (owner, window) = (r.owner, r.window);
            let session = self.sessions.get_mut(id).unwrap();
            session.action = session.resolved_action();
            if let Some(error) = session.action_error()
            {
                self.return_to_source(id, REJECTED, error, when);
                return;
            }
            session.phase = DROPPING;
            self.set_highlight(None);
            self.notice(owner, DROP_EVENT, window, id, when);
        }
        else
        {
            self.return_to_source(id, REJECTED, session.error, when);
        }
    }

    /// System supplies the hot content window from its shared Window Manager.
    /// No client can supply physical release/modifier state or steal the gesture.
    pub fn motion(&mut self, point: Point, modifiers: u16, hot: Option<u32>, when: u32)
    {
        let Some(id) = self.active
        else
        {
            return;
        };
        let session = self.sessions.get_mut(id).unwrap();
        if session.released || !matches!(session.phase, ARMED | TRACKING)
        {
            return;
        }
        let changed = session.point != point || session.modifiers != modifiers;
        if changed && session.phase == TRACKING
        {
            session.accepted = false;
            session.responded = false;
        }
        session.point = point;
        session.modifiers = modifiers;
        if session.phase == ARMED
        {
            if (i32::from(point.h) - i32::from(session.anchor.h)).abs() <= 4
               && (i32::from(point.v) - i32::from(session.anchor.v)).abs() <= 4
            {
                return;
            }
            session.started = true;
            session.phase = TRACKING;
        }
        if session.phase != TRACKING
        {
            return;
        }
        // Modifier changes withdraw an impossible highlight immediately, before
        // the app services the new offer. This is constant-time cached policy.
        let clear_highlight = changed && session.action_error().is_some();
        let receiver_changed = session.receiver != hot;
        if clear_highlight
        {
            self.set_highlight(None);
        }
        if receiver_changed
        {
            self.leave(id, when);
            self.sessions.get_mut(id).unwrap().receiver = hot;
            if let Some(receiver) = hot.and_then(|r| self.receivers.get(r))
            {
                self.notice(receiver.owner, ENTERED_EVENT, receiver.window, id, when);
            }
        }
        else if changed && let Some(receiver) = hot.and_then(|r| self.receivers.get(r))
        {
            self.notice(receiver.owner, WITHIN_EVENT, receiver.window, id, when);
        }
    }

    pub fn release(&mut self, point: Point, modifiers: u16, hot: Option<u32>, when: u32)
    {
        self.motion(point, modifiers, hot, when);
        let Some(id) = self.active
        else
        {
            return;
        };
        let session = self.sessions.get_mut(id).unwrap();
        session.released = true;
        session.up_modifiers = modifiers;
        match session.phase
        {
            ARMED =>
            {
                session.outcome = CLICKED;
                self.finish(id, when);
            }
            TRACKING if session.responded || session.receiver.is_none() =>
            {
                self.drop_at_receiver(id, when)
            }
            _ =>
            {}
        }
    }

    pub fn escape(&mut self, when: u32) -> bool
    {
        let Some(id) = self.active
        else
        {
            return false;
        };
        let session = self.sessions.get_mut(id).unwrap();
        if session.phase == DROPPING
        {
            if !session.cancel_requested
            {
                session.cancel_requested = true;
                if let Some(receiver) = session.receiver.and_then(|r| self.receivers.get(r))
                {
                    self.notice(receiver.owner, CANCEL_REQUESTED_EVENT, receiver.window, id, when);
                }
            }
        }
        else if session.phase != RETURNING
        {
            self.return_to_source(id, CANCELLED, OK, when);
        }
        true
    }

    pub fn tick(&mut self, when: u32)
    {
        let Some(id) = self.active
        else
        {
            return;
        };
        let session = self.sessions.get_mut(id).unwrap();
        if session.phase != RETURNING || when.wrapping_sub(session.return_tick) == 0
        {
            return;
        }
        session.return_tick = when;
        session.return_step += 1;
        let step = session.return_step.min(6) as i32;
        let lerp =
            |a: i16, b: i16| (i32::from(a) + (i32::from(b) - i32::from(a)) * step / 6) as i16;
        session.point = Point { h: lerp(session.return_start.h, session.anchor.h),
                                v: lerp(session.return_start.v, session.anchor.v) };
        if step == 6
        {
            self.finish(id, when);
        }
    }

    pub fn forget_window(&mut self, window: u64, when: u32)
    {
        if let Some(active) = self.active
           && self.sessions.get(active).is_some_and(|s| s.source == window && s.phase != DROPPING)
        {
            self.return_to_source(active, CANCELLED, OK, when);
        }
        if let Some(id) = self.receiver_at(window)
        {
            if let Some(active) = self.active
               && self.sessions.get(active).unwrap().receiver == Some(id)
            {
                if self.sessions.get(active).unwrap().phase == DROPPING
                {
                    // Closing a receiver's window cannot invalidate its live
                    // cleanup capability. The app drains even after the window
                    // disappears; leave() retires the closing registration.
                    self.receivers.get_mut(id).unwrap().closing = true;
                    self.escape(when);
                    return;
                }
                self.return_to_source(active, REJECTED, NOT_FOUND, when);
            }
            self.receivers.remove(id);
        }
    }

    pub fn close_owner(&mut self, owner: u64, when: u32)
    {
        // A dead receiver cannot acknowledge. Its File Manager owners have
        // independent exit cleanup; retire drag feedback/registration now.
        if let Some(active) = self.active
           && self.sessions
                  .get(active)
                  .and_then(|s| s.receiver)
                  .and_then(|r| self.receivers.get(r))
                  .is_some_and(|r| r.owner == owner)
        {
            self.return_to_source(active, REJECTED, OPERATION_CANCELLED, when);
        }
        loop
        {
            let id = self.sessions.iter().find_map(|(id, s)| (s.owner == owner).then_some(id));
            let Some(id) = id
            else
            {
                break;
            };
            if self.active == Some(id)
            {
                self.leave(id, when);
                self.active = None;
            }
            self.sessions.remove(id);
        }
        loop
        {
            let window =
                self.receivers.iter().find_map(|(_, r)| (r.owner == owner).then_some(r.window));
            let Some(window) = window
            else
            {
                break;
            };
            self.forget_window(window, when);
        }
    }

    /// Registration's window ownership is validated at the window service boundary.
    pub fn request(&mut self, owner: u64, thread: u64, call: &mut Call, input: &[u8],
                   output: &mut [u8], when: u32)
                   -> Result<(), Status>
    {
        call.transferred = 0;
        if !matches!(call.operation, CREATE | DISPOSE | ABANDON)
        {
            let session = self.readable(call.id, owner)?;
            let expected = if session.owner == owner
            {
                session.thread
            }
            else
            {
                session.receiver.and_then(|r| self.receivers.get(r)).ok_or(NOT_FOUND)?.thread
            };
            if expected != thread
            {
                return Err(WRONG_THREAD);
            }
        }
        match call.operation
        {
            CREATE =>
            {
                call.id = self.sessions
                              .insert(Session { owner,
                                                source: call.window,
                                                thread,
                                                detached: false,
                                                action: MOVE,
                                                destination: 0,
                                                allowed_actions: 0,
                                                file_volume: None,
                                                mixed_volumes: false,
                                                trash: false,
                                                source_read_only: false,
                                                file_folder: None,
                                                icon_layout: false,
                                                application: false,
                                                eject: false,
                                                volume_root: None,
                                                file_types: Vec::new(),
                                                file_types_complete: false,
                                                folders: false,
                                                anchor: call.point,
                                                point: call.point,
                                                modifiers: call.flags as u16,
                                                down_modifiers: call.flags as u16,
                                                up_modifiers: 0,
                                                phase: DRAFT,
                                                outcome: 0,
                                                error: OK,
                                                receiver: None,
                                                accepted: false,
                                                responded: false,
                                                released: false,
                                                cancel_requested: false,
                                                items: Vec::new(),
                                                common: 0,
                                                started: false,
                                                bytes: 0,
                                                bounds: None,
                                                return_start: call.point,
                                                return_tick: 0,
                                                return_step: 0 })
                              .map_err(|_| NO_MEMORY)?;
            }
            BEGIN =>
            {
                if self.active.is_some()
                {
                    return Err(SERVICE_BUSY);
                }
                let session = self.sessions.get_mut(call.id).unwrap();
                if session.phase != DRAFT || session.items.is_empty() || call.bounds.is_empty()
                {
                    return Err(BAD_ARGUMENT);
                }
                session.anchor = call.point;
                session.point = call.point;
                session.modifiers = call.flags as u16;
                session.down_modifiers = call.flags as u16;
                session.bounds = Some(call.bounds);
                session.phase = ARMED;
                // File identity metadata is frozen once. Never inspect the
                // item/flavor list again on a pointer/modifier path.
                for flavor in session.items.iter().flat_map(|item| &item.flavors)
                {
                    if flavor.kind == *b"fref" && flavor.bytes.len() >= 4
                    {
                        let volume = u32::from_le_bytes(flavor.bytes[..4].try_into().unwrap());
                        session.mixed_volumes |= session.file_volume.is_some_and(|v| v != volume);
                        session.file_volume = Some(volume);
                    }
                }
                session.file_types.clear();
                session.volume_root =
                    if let [item] = session.items.as_slice()
                       && let Some(flavor) = item.flavors.iter().find(|f| {
                                                                    f.kind == *b"fref"
                                                                    && f.state == 0
                                                                    && f.bytes.len()
                                                                       == FILE_ITEM_BYTES
                                                                })
                       && u32::from_le_bytes(flavor.bytes[4..8].try_into().unwrap())
                          == mac_abi::files::ROOT_ID
                       && u32::from_le_bytes(flavor.bytes[28..32].try_into().unwrap()) == 1
                    {
                        Some(u32::from_le_bytes(flavor.bytes[..4].try_into().unwrap()))
                    }
                    else
                    {
                        None
                    };
                session.folders = false;
                session.file_types_complete = true;
                session.file_types.try_reserve(session.items.len()).map_err(|_| NO_MEMORY)?;
                for item in &session.items
                {
                    let flavor = item.flavors.iter().find(|flavor| {
                                                        flavor.kind == *b"fref"
                                                        && flavor.state == 0
                                                        && flavor.bytes.len() == FILE_ITEM_BYTES
                                                    });
                    if let Some(flavor) = flavor
                    {
                        let flags = u32::from_le_bytes(flavor.bytes[28..32].try_into().unwrap());
                        if flags > 1
                        {
                            return Err(BAD_ARGUMENT);
                        }
                        session.folders |= flags != 0;
                        let kind = flavor.bytes[24..28].try_into().unwrap();
                        if let Err(at) = session.file_types.binary_search(&kind)
                        {
                            session.file_types.insert(at, kind);
                        }
                    }
                    else
                    {
                        session.file_types_complete = false;
                    }
                }
                session.file_folder = session.items
                                             .iter()
                                             .try_fold(None, |previous, item| {
                                                 let flavor = item.flavors
                                                                  .iter()
                                                                  .find(|f| {
                                                                      f.kind == *b"fref"
                                                                      && f.state == 0
                                                                      && f.bytes.len()
                                                                         == FILE_ITEM_BYTES
                                                                  })
                                                                  .ok_or(())?;
                                                 let folder =
                                   (u32::from_le_bytes(flavor.bytes[..4].try_into().unwrap()),
                                    u32::from_le_bytes(flavor.bytes[8..12].try_into().unwrap()));
                                                 if previous.is_some_and(|old| old != folder)
                                                 {
                                                     Err(())
                                                 }
                                                 else
                                                 {
                                                     Ok(Some(folder))
                                                 }
                                             })
                                             .ok()
                                             .flatten();
                session.common =
                    u32::from(session.items
                                     .iter()
                                     .all(|i| i.flavors.iter().any(|f| f.kind == *b"fref")))
                    | (u32::from(session.items.iter().all(|i| {
                                                         i.flavors
                                                          .iter()
                                                          .any(|f| f.kind == *b"TEXT")
                                                     }))
                       << 1)
                    | (u32::from(session.volume_root.is_some()) << 2);
                self.active = Some(call.id);
            }
            DISPOSE =>
            {
                self.owner(call.id, owner)?;
                if self.active == Some(call.id)
                {
                    self.sessions.get_mut(call.id).unwrap().detached = true;
                    self.escape(when);
                }
                else
                {
                    self.sessions.remove(call.id);
                }
            }
            INFO =>
            {
                let session = self.readable(call.id, owner)?;
                call.index = u32::from(session.started);
                call.point = session.anchor;
                call.flags =
                    u32::from(session.down_modifiers) | (u32::from(session.up_modifiers) << 16);
                call.info = Info { phase:     session.phase,
                                   outcome:   if session.cancel_requested
                                                 && session.phase == DROPPING
                                   {
                                       CANCELLED
                                   }
                                   else
                                   {
                                       session.outcome
                                   },
                                   items:     session.items.len() as u32,
                                   receiver:  session.receiver.unwrap_or(0),
                                   window:    session.receiver
                                                     .and_then(|r| self.receivers.get(r))
                                                     .map_or(0, |r| r.window),
                                   point:     session.point,
                                   modifiers: session.modifiers,
                                   action:    session.action as u16,
                                   error:     session.error };
            }
            CANCEL =>
            {
                self.owner(call.id, owner)?;
                if self.active == Some(call.id)
                {
                    self.escape(when);
                }
            }
            CANCEL_RECEIVE =>
            {
                self.receiver(call.id, call.receiver, owner)?;
                if self.sessions.get(call.id).unwrap().phase != DROPPING
                {
                    return Err(ACCESS_DENIED);
                }
                if self.active == Some(call.id)
                {
                    self.escape(when);
                }
            }
            FILE_ITEMS =>
            {
                self.owner(call.id, owner)?;
                let session = self.sessions.get_mut(call.id).unwrap();
                if session.phase != DRAFT || input.len() % FILE_ITEM_BYTES != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                let count = input.len() / FILE_ITEM_BYTES;
                if count == 0
                   || count > MAX_ITEMS - session.items.len()
                   || input.len() > MAX_BYTES - session.bytes
                {
                    return Err(BAD_ARGUMENT);
                }
                // Each private draft append is atomic; no receiver can observe
                // this group until its owner successfully calls Begin.
                let mut items = Vec::new();
                items.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
                let first = session.items.len();
                session.items.try_reserve(count).map_err(|_| NO_MEMORY)?;
                for (index, bytes) in input.chunks_exact(FILE_ITEM_BYTES).enumerate()
                {
                    let bounds = Rect::from_be_bytes(bytes[16..24].try_into().unwrap());
                    let mut flavors = Vec::new();
                    flavors.try_reserve_exact(1).map_err(|_| NO_MEMORY)?;
                    flavors.push(Flavor { kind:           *b"fref",
                                          state:          0,
                                          claimed:        false,
                                          taken:          false,
                                          native_claimed: false,
                                          flags:          0,
                                          declared:       FILE_ITEM_BYTES,
                                          error:          OK,
                                          bytes:          copied(bytes)? });
                    items.push(Item { id: (first + index) as u32 + 1,
                                      flavors,
                                      bounds });
                }
                session.items.extend(items);
                session.bytes += input.len();
            }
            ADD_ITEM | OFFER =>
            {
                if call.operation == OFFER
                   && call.kind == *b"TEXT"
                   && core::str::from_utf8(input).is_err()
                {
                    return Err(BAD_ARGUMENT);
                }
                let session = self.owner(call.id, owner)?;
                if session.phase != DRAFT
                {
                    return Err(ACCESS_DENIED);
                }
                let session = self.sessions.get_mut(call.id).unwrap();
                if call.operation == ADD_ITEM
                {
                    if session.items.len() == MAX_ITEMS
                    {
                        return Err(NO_MEMORY);
                    }
                    if session.items.iter().any(|i| i.id == call.item)
                    {
                        return Err(ALREADY_EXISTS);
                    }
                    session.items.try_reserve(1).map_err(|_| NO_MEMORY)?;
                    session.items.push(Item { id:      call.item,
                                              flavors: Vec::new(),
                                              bounds:  call.bounds });
                }
                else if call.operation == OFFER
                {
                    if call.flags & !PROMISED != 0 || call.flags == PROMISED && !input.is_empty()
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    if session.bytes.checked_add(input.len()).is_none_or(|n| n > MAX_BYTES)
                    {
                        return Err(NO_MEMORY);
                    }
                    let item =
                        session.items.iter_mut().find(|i| i.id == call.item).ok_or(NOT_FOUND)?;
                    if item.flavors.iter().any(|f| f.kind == call.kind)
                    {
                        return Err(ALREADY_EXISTS);
                    }
                    if item.flavors.len() == MAX_FLAVORS
                    {
                        return Err(NO_MEMORY);
                    }
                    let bytes = copied(input)?;
                    item.flavors.try_reserve(1).map_err(|_| NO_MEMORY)?;
                    item.flavors.push(Flavor { kind: call.kind,
                                               state: call.flags,
                                               claimed: false,
                                               taken: false,
                                               native_claimed: false,
                                               flags: 0,
                                               declared: input.len(),
                                               error: OK,
                                               bytes });
                    session.bytes += input.len();
                }
            }
            ACCEPT =>
            {
                self.receiver(call.id, call.receiver, owner)?;
                if call.flags
                   & !(1
                       | TARGET_TRASH
                       | TARGET_ICON_LAYOUT
                       | TARGET_APPLICATION
                       | TARGET_EJECT
                       | ACCEPT_COPY
                       | ACCEPT_MOVE
                       | SAME_VOLUME_MOVE)
                   != 0
                   || call.flags & 1 != 0 && call.flags & (ACCEPT_COPY | ACCEPT_MOVE) == 0
                   || call.flags & 1 == 0 && call.flags != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                if self.sessions.get(call.id).unwrap().phase != TRACKING
                {
                    return Err(ACCESS_DENIED);
                }
                let session = self.sessions.get_mut(call.id).unwrap();
                // A receiver may answer an older coalesced motion after release.
                // Only the response for the actual drop position may commit it.
                if call.point != session.point
                {
                    return Ok(());
                }
                let layout = call.flags & TARGET_ICON_LAYOUT != 0;
                if layout
                   && (call.flags & (TARGET_TRASH | ACCEPT_COPY) != 0
                       || call.flags & ACCEPT_MOVE == 0
                       || session.file_folder != Some((call.volume, call.parent)))
                {
                    return Err(BAD_ARGUMENT);
                }
                let application = call.flags & TARGET_APPLICATION != 0;
                if application
                   && (call.parent <= mac_abi::files::ROOT_ID
                       || call.volume == 0
                       || call.flags
                          & (TARGET_TRASH | TARGET_ICON_LAYOUT | TARGET_EJECT | ACCEPT_MOVE)
                          != 0
                       || call.flags & ACCEPT_COPY == 0)
                {
                    return Err(BAD_ARGUMENT);
                }
                session.application = application;
                let eject = call.flags & TARGET_EJECT != 0;
                if eject
                   && (session.volume_root != Some(call.volume)
                       || call.parent != mac_abi::files::ROOT_ID
                       || call.flags
                          & (TARGET_TRASH | TARGET_ICON_LAYOUT | TARGET_APPLICATION | ACCEPT_COPY)
                          != 0
                       || call.flags & ACCEPT_MOVE == 0)
                {
                    return Err(BAD_ARGUMENT);
                }
                session.eject = eject;
                session.icon_layout = layout;
                session.allowed_actions =
                    call.flags & (ACCEPT_COPY | ACCEPT_MOVE | SAME_VOLUME_MOVE);
                session.destination = call.volume;
                session.trash = call.flags & TARGET_TRASH != 0;
                session.error = if call.flags & 1 != 0
                {
                    session.action_error().unwrap_or(OK)
                }
                else
                {
                    call.error
                };
                session.accepted = call.flags & 1 != 0 && session.error == OK;
                session.responded = true;
                let released = session.released;
                let highlight = session.accepted.then_some((call.window, call.bounds));
                self.set_highlight(highlight.filter(|(_, r)| !r.is_empty()));
                if released
                {
                    self.drop_at_receiver(call.id, when);
                }
            }
            FILE_ITEM_METADATA =>
            {
                let session = self.readable(call.id, owner)?;
                if session.phase != TRACKING
                {
                    return Err(ACCESS_DENIED);
                }
                let item = session.items.get(call.index as usize).ok_or(NOT_FOUND)?;
                let flavor = item.flavors
                                 .iter()
                                 .find(|flavor| flavor.kind == *b"fref")
                                 .ok_or(FLAVOR_UNAVAILABLE)?;
                if flavor.state != 0
                {
                    return Err(FLAVOR_UNAVAILABLE);
                }
                if flavor.bytes.len() != FILE_ITEM_BYTES
                {
                    return Err(BAD_ABI);
                }
                if output.len() < FILE_ITEM_BYTES
                {
                    return Err(NO_MEMORY);
                }
                output[..FILE_ITEM_BYTES].copy_from_slice(&flavor.bytes);
                call.transferred = FILE_ITEM_BYTES;
            }
            ITEM_AT | FLAVOR_AT =>
            {
                let session = self.readable(call.id, owner)?;
                if call.operation == ITEM_AT
                {
                    let item = session.items.get(call.index as usize).ok_or(-1851)?;
                    call.item = item.id;
                    call.flags = item.flavors
                                     .iter()
                                     .filter(|f| session.owner == owner || f.flags & 1 == 0)
                                     .count() as u32;
                }
                else
                {
                    let item = session.items.iter().find(|i| i.id == call.item).ok_or(NOT_FOUND)?;
                    let flavor = item.flavors
                                     .iter()
                                     .filter(|f| session.owner == owner || f.flags & 1 == 0)
                                     .nth(call.index as usize)
                                     .ok_or(-1852)?;
                    call.kind = flavor.kind;
                    call.flags = u32::from(flavor.state != 0);
                    call.transferred = flavor.declared.max(flavor.bytes.len());
                }
            }
            READ =>
            {
                self.receiver(call.id, call.receiver, owner)?;
                let session = self.sessions.get_mut(call.id).unwrap();
                if session.phase != DROPPING
                {
                    return Err(ACCESS_DENIED);
                }
                if session.cancel_requested
                {
                    return Err(OPERATION_CANCELLED);
                }
                let sender = session.owner == owner;
                let item = session.items.iter_mut().find(|i| i.id == call.item).ok_or(NOT_FOUND)?;
                let flavor = item.flavors
                                 .iter_mut()
                                 .find(|f| f.kind == call.kind && (sender || f.flags & 1 == 0))
                                 .ok_or(FLAVOR_UNAVAILABLE)?;
                if call.flags > 1
                   || flavor.taken
                   || (call.flags == 0 && flavor.native_claimed)
                   || (call.flags == 1 && !flavor.native_claimed)
                {
                    return Err(FLAVOR_UNAVAILABLE);
                }
                flavor.native_claimed = true;
                match flavor.state
                {
                    0 =>
                    {
                        if flavor.taken
                        {
                            return Err(FLAVOR_UNAVAILABLE);
                        }
                        call.transferred = flavor.bytes.len();
                        if output.len() < flavor.bytes.len()
                        {
                            return Err(NO_MEMORY);
                        }
                        output[..flavor.bytes.len()].copy_from_slice(&flavor.bytes);
                        flavor.taken = true;
                    }
                    1 =>
                    {
                        flavor.state = 2;
                        let (owner, source) = (session.owner, session.source);
                        self.notice(owner, PROMISE_EVENT, source, call.id, when);
                        return Err(SERVICE_BUSY);
                    }
                    2 => return Err(SERVICE_BUSY),
                    _ =>
                    {
                        flavor.taken = true;
                        call.error = flavor.error;
                        return Err(PROMISE_FAILED);
                    }
                }
            }
            NEXT_PROMISE =>
            {
                self.owner(call.id, owner)?;
                let session = self.sessions.get_mut(call.id).unwrap();
                if session.phase != DROPPING
                {
                    return Err(ACCESS_DENIED);
                }
                if session.cancel_requested
                {
                    return Err(NOT_FOUND);
                }
                let (item, flavor) = session.items
                                            .iter_mut()
                                            .find_map(|i| {
                                                i.flavors
                                                 .iter_mut()
                                                 .find(|f| f.state == 2 && !f.claimed)
                                                 .map(|f| (i.id, f))
                                            })
                                            .ok_or(NOT_FOUND)?;
                flavor.claimed = true;
                call.item = item;
                call.kind = flavor.kind;
            }
            FULFILL =>
            {
                self.owner(call.id, owner)?;
                let session = self.sessions.get_mut(call.id).unwrap();
                if session.phase != DROPPING
                {
                    return Err(ACCESS_DENIED);
                }
                if call.error == OK && call.kind == *b"TEXT" && core::str::from_utf8(input).is_err()
                {
                    return Err(BAD_ARGUMENT);
                }
                if session.bytes.checked_add(input.len()).is_none_or(|n| n > MAX_BYTES)
                {
                    return Err(NO_MEMORY);
                }
                let item = session.items.iter_mut().find(|i| i.id == call.item).ok_or(NOT_FOUND)?;
                let flavor =
                    item.flavors.iter_mut().find(|f| f.kind == call.kind).ok_or(NOT_FOUND)?;
                if flavor.state != 2 || !flavor.claimed
                {
                    return Err(ACCESS_DENIED);
                }
                let bytes = copied(input)?;
                flavor.state = if call.error == OK { 0 } else { 3 };
                flavor.error = call.error;
                flavor.bytes = bytes;
                session.bytes += input.len();
                if let Some(receiver) = session.receiver.and_then(|r| self.receivers.get(r))
                {
                    self.notice(receiver.owner, DATA_READY_EVENT, receiver.window, call.id, when);
                }
            }
            ABANDON =>
            {
                // Cleanup carries no data-access or acceptance capability. It is
                // safe when a Send future is destroyed on another app thread.
                self.receiver(call.id, call.receiver, owner)?;
                let session = self.sessions.get(call.id).unwrap();
                if call.index == DROPPING && session.phase == DROPPING
                {
                    self.return_to_source(call.id, REJECTED, IO_ERROR, when);
                }
                else if call.index == TRACKING
                          && session.phase == TRACKING
                          && call.point == session.point
                {
                    let released = session.released;
                    let session = self.sessions.get_mut(call.id).unwrap();
                    session.accepted = false;
                    session.responded = true;
                    self.set_highlight(None);
                    if released
                    {
                        self.drop_at_receiver(call.id, when);
                    }
                }
            }
            ACKNOWLEDGE =>
            {
                self.receiver(call.id, call.receiver, owner)?;
                if self.sessions.get(call.id).unwrap().phase != DROPPING
                {
                    return Err(ACCESS_DENIED);
                }
                if call.flags == 0
                {
                    let cancelled = self.sessions.get(call.id).unwrap().cancel_requested;
                    self.return_to_source(call.id,
                                          if cancelled { CANCELLED } else { REJECTED },
                                          call.error,
                                          when);
                }
                else
                {
                    self.leave(call.id, when);
                    let session = self.sessions.get_mut(call.id).unwrap();
                    session.outcome = if session.action == COPY { COPIED } else { MOVED };
                    self.finish(call.id, when);
                }
            }
            CLASSIC_ADD | CLASSIC_SET | CLASSIC_FLAVOR | CLASSIC_READ | ITEM_BOUNDS
            | CLASSIC_COUNT =>
            {
                self.classic_data(owner, call, input, output)?;
            }
            _ => return Err(NOT_SUPPORTED)
        }
        Ok(())
    }
}
