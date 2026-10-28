//! Rectangular document windows with stable IDs, z order, damage and tracking.
//! Rust APIs implement a documented subset; no WindowRecord guest ABI is implied.
#![cfg_attr(not(test), forbid(unsafe_code))]
#[cfg(test)]
use alloc::vec;
use alloc::{collections::VecDeque, vec::Vec};
use core::cell::{Cell, RefCell};

pub use mac_abi::WindowId;
use mac_abi::{BAD_ARGUMENT, DefinitionCall, DefinitionEntry, Header, NOT_FOUND, Status, Surface};
use mac_object_table::shared::InvocationGate as _;

use super::definition::{self, WindowDefinitionState};
use crate::toolbox::{events::*,
                     handles::{InvocationGate, KindLease, OwnerRef, Store, cleanup::Failure},
                     quickdraw::{Canvas, Point, Rect, SurfaceExt,
                                 geometry::{sect_rect, union_rect},
                                 presentation::{Outline, indexed_tracking_xor},
                                 region::{BooleanOp, Region, RegionError}}};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i16)]
pub enum WindowPart
{
    Desk    = 0,
    MenuBar = 1,
    Content = 3,
    Drag    = 4,
    Grow    = 5,
    GoAway  = 6,
    ZoomIn  = 7,
    ZoomOut = 8
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit
{
    pub part:   WindowPart,
    pub window: Option<WindowId>
}

/// Focus requested while moving a whole application window plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupFocus
{
    Front,
    Window(WindowId),
    Desktop,
    Preserve
}

#[derive(Clone, Copy)]
pub struct Window
{
    pub id:              WindowId,
    initialized:         bool,
    pub bounds:          Rect,
    pub owner:           u64,
    pub min_width:       i16,
    pub min_height:      i16,
    state:               WindowDefinitionState,
    procedure:           i16,
    entry:               DefinitionEntry,
    gate:                mac_abi::TrapGate,
    user_bounds:         Option<Rect>,
    standard_bounds:     Option<Rect>,
    grow_icon_clip:      Option<Rect>,
    content_outline:     Option<Rect>,
    close_deferred:      bool,
    damage:              Option<Rect>,
    hidden:              bool,
    pub(crate) updating: bool
}
impl Window
{
    pub fn zoomed(&self) -> bool
    {
        self.state.zoomed != 0
    }

    pub fn title(&self) -> &str
    {
        core::str::from_utf8(&self.state.title[..self.state.title_length as usize]).unwrap_or("WINDOW")
    }

    pub fn width(&self) -> i16
    {
        self.bounds.right - self.bounds.left
    }

    pub fn height(&self) -> i16
    {
        self.bounds.bottom - self.bounds.top
    }

    pub fn local_bounds(&self) -> Rect
    {
        Rect::new(0, 0, self.width(), self.height())
    }

    pub(crate) fn outline_rect(&self) -> Option<Rect>
    {
        self.content_outline
    }

    pub fn procedure(&self) -> i16
    {
        self.procedure
    }

    pub fn structure(&self) -> Rect
    {
        self.state.structure
    }

    pub fn structure_pieces(&self) -> &[Rect]
    {
        if self.state.structure_piece_count == 0
        {
            core::slice::from_ref(&self.state.structure)
        }
        else
        {
            &self.state.structure_pieces[..(self.state.structure_piece_count as usize).min(2)]
        }
    }

    pub fn close_box(&self) -> Rect
    {
        self.state.close_box
    }

    pub fn grow_box(&self) -> Rect
    {
        self.state.grow_box
    }

    /// Definition-owned content adornments are above retained application
    /// drawing. Preserve the compositor's damage clip, including grow-strip
    /// restrictions supplied by applications that do not use both scrollbars.
    #[cfg_attr(not(target_arch = "riscv64"), allow(dead_code))]
    pub(crate) fn paint_content_overlay(&self, canvas: &mut Canvas<'_>)
    {
        let origin = canvas.origin();
        let previous = canvas.clip();
        let mut clip = previous;
        if let Some(mut grow) = self.grow_icon_clip
        {
            grow.offset(self.bounds.left, self.bounds.top);
            if !sect_rect(grow, previous, &mut clip)
            {
                return;
            }
        }
        canvas.set_origin(0, 0);
        canvas.set_clip(clip);
        let mut copy = *self;
        let mut surface = Surface::from_canvas(canvas);
        copy.call(definition::DRAW_GROW_ICON, 0, &mut surface);
        canvas.set_clip(previous);
        canvas.set_origin(origin.h, origin.v);
    }

    fn call(&mut self, message: i16, param: i64, surface: *mut Surface) -> i64
    {
        let mut call = DefinitionCall { header: Header::for_type::<DefinitionCall>(),
                                        kind: definition::WDEF,
                                        id: self.procedure >> 4,
                                        variation: self.procedure & 15,
                                        message,
                                        reserved: 0,
                                        param,
                                        object:
                                            (&mut self.state as *mut WindowDefinitionState).cast(),
                                        surface,
                                        gate: self.gate };
        // The manager owns state, and a supplied entry must implement this native ABI.
        #[cfg(target_arch = "riscv64")]
        {
            crate::kernel::with_resource_owner(self.owner, || {
                crate::toolbox::resources::definitions::invoke(self.entry, &mut call)
            }).unwrap_or_else(i64::from)
        }
        #[cfg(not(target_arch = "riscv64"))]
        {
            crate::toolbox::resources::definitions::invoke(self.entry, &mut call)
        }
    }

    fn calculate(&mut self) -> bool
    {
        self.state.content = self.bounds;
        self.call(definition::CALCULATE, 0, core::ptr::null_mut()) >= 0
    }

    pub fn global_to_local(&self, point: Point) -> Point
    {
        Point { h: point.h - self.bounds.left,
                v: point.v - self.bounds.top }
    }
}

#[derive(Clone, Copy)]
enum Tracking
{
    Zoom
    {
        window: WindowId, armed: bool
    },
    Drag
    {
        window:   WindowId,
        start:    Point,
        bounds:   Rect,
        preview:  Option<Rect>,
        activate: bool
    },
    Grow
    {
        window: WindowId, start: Point, bounds: Rect, preview: Rect
    },
    Close
    {
        window: WindowId, armed: bool
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action
{
    Drag(WindowId),
    Resize(WindowId),
    Close(WindowId, u64),
    CloseRequest(WindowId)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CreationError
{
    InvalidRequest,
    NoMemory,
    Facility(mac_object_table::domain::PoolError),
    Stopping,
    Definition
    {
        procedure: i16,
        operation: i16,
        reply:     i64
    },
    Geometry
    {
        procedure: i16
    }
}
impl core::fmt::Display for CreationError
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::InvalidRequest => f.write_str("window bounds or minimum dimensions are invalid"),
            Self::NoMemory =>
            {
                f.write_str("not enough memory to prepare window metadata or visibility storage")
            }
            Self::Facility(error) => error.fmt(f),
            Self::Stopping =>
            {
                f.write_str("the window owner is stopping; no window was published to its UI")
            }
            Self::Definition { procedure,
                               operation,
                               reply } =>
            {
                write!(f,
                       "WDEF {} variation {} refused initialization message {operation} with \
                        reply {reply}",
                       procedure >> 4,
                       procedure & 15)
            }
            Self::Geometry { procedure } =>
            {
                write!(f,
                       "WDEF {} variation {} produced empty or out-of-screen structure geometry",
                       procedure >> 4,
                       procedure & 15)
            }
        }
    }
}
struct InitializingWindow<'a>
{
    manager:   &'a mut WindowManager,
    id:        WindowId,
    committed: bool
}
impl InitializingWindow<'_>
{
    fn value(&mut self) -> Result<&mut Window, CreationError>
    {
        self.manager.windows.get_mut(self.id.0).ok_or(CreationError::Stopping)
    }

    fn commit(mut self) -> WindowId
    {
        self.committed = true;
        self.id
    }
}
impl Drop for InitializingWindow<'_>
{
    fn drop(&mut self)
    {
        use mac_object_table::registry::CleanupFailure;
        if self.committed
        {
            return;
        }
        // A concurrent owner walk may already own the invalidated ticket. It
        // cannot remove this payload until the exclusive manager borrow ends.
        let Some(ticket) = self.manager.windows.begin_release(self.id.0)
        else
        {
            return;
        };
        if let Err(failure) = self.manager.release(ticket)
        {
            let (ticket, cause) = failure.into_parts();
            self.manager.quarantine_or_retain(ticket);
            #[cfg(target_arch = "riscv64")]
            serial!("WINDOW_INITIALIZATION_ROLLBACK_REFUSED: {cause}");
            #[cfg(not(target_arch = "riscv64"))]
            let _ = cause;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseError
{
    NotFound(WindowId),
    Cleanup(crate::toolbox::handles::cleanup::Cause)
}
impl core::fmt::Display for CloseError
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::NotFound(id) => write!(f, "window {id} is no longer live"),
            Self::Cleanup(cause) => cause.fmt(f)
        }
    }
}

pub struct WindowManager
{
    // Desktop admission already excludes reentrant/mutating WM requests while
    // drawing. Reborrow cached sweep storage without adding an interrupt lock
    // around definition drawing callbacks. The manager remains Send behind the
    // resident service lock; callers never share it without that admission.
    visibility:        RefCell<super::visibility::Scratch>,
    // A single bounded memo, never an authority or a payload pin. Successful
    // window-list changes invalidate it; cached IDs still validate on lookup.
    front_cache:       Cell<Option<(u64, Option<WindowId>)>>,
    windows:           Store<Window>,
    retained_failures: VecDeque<mac_object_table::domain::CompletionError>,
    order:             Vec<WindowId>,
    len:               usize,
    active:            Option<WindowId>,
    order_scratch:     Vec<WindowId>,
    geometry_order:    Vec<WindowId>,
    #[cfg(test)]
    test_registry:     Option<alloc::boxed::Box<crate::toolbox::handles::Registry<WindowManager>>>,
    #[cfg(test)]
    test_owners:       Vec<(u64, OwnerRef)>,
    gate:              mac_abi::TrapGate,
    tracking:          Option<Tracking>
}
#[cfg(test)]
impl Default for WindowManager
{
    fn default() -> Self
    {
        Self::new()
    }
}
impl WindowManager
{
    pub(crate) fn with_kinds(kinds: (KindLease, KindLease), gate: mac_abi::TrapGate)
                             -> Result<Self, Status>
    {
        Ok(Self { visibility: RefCell::new(Default::default()),
                  front_cache: Cell::new(None),
                  windows: Store::owned(kinds.0, kinds.1).map_err(|_| BAD_ARGUMENT)?,
                  retained_failures: VecDeque::new(),
                  order: Vec::new(),
                  len: 0,
                  active: None,
                  order_scratch: Vec::new(),
                  geometry_order: Vec::new(),
                  gate,
                  tracking: None,
                  #[cfg(test)]
                  test_registry: None,
                  #[cfg(test)]
                  test_owners: Vec::new() })
    }

    #[cfg(test)]
    pub fn new() -> Self
    {
        use mac_object_table::{domain::{DomainKind, OwnerIndex},
                               registry::Registry};
        let registrar = OwnerRef::try_new(OwnerIndex::issue(DomainKind::Module).unwrap()).unwrap();
        let mut registry = Registry::new();
        let objects = registry.register(&registrar,
                                        super::lifetime::specification(false),
                                        super::lifetime::ReleaseWindows)
                              .unwrap();
        let storage = registry.register(&registrar,
                                        super::lifetime::specification(true),
                                        super::lifetime::ReleaseStorage)
                              .unwrap();
        let mut manager =
            Self::with_kinds((objects, storage), crate::toolbox::quickdraw::surface_gate).unwrap();
        manager.test_registry = Some(alloc::boxed::Box::new(registry));
        manager
    }

    #[cfg(test)]
    fn test_owner(&mut self, alias: u64) -> OwnerRef
    {
        if let Some((_, owner)) = self.test_owners.iter().find(|(id, _)| *id == alias)
        {
            return owner.clone();
        }
        let owner = OwnerRef::try_new(mac_object_table::domain::OwnerIndex::issue(mac_object_table::domain::DomainKind::Process).unwrap()).unwrap();
        self.test_owners.push((alias, owner.clone()));
        owner
    }

    pub(crate) fn owner(&self, id: WindowId) -> Option<u64>
    {
        self.windows.owner(id.0).map(|owner| owner.incarnation())
    }

    pub(crate) fn releasing_id(&self, ticket: &mac_object_table::domain::ReleasedResource)
                               -> Option<WindowId>
    {
        self.windows.releasing(ticket).map(|window| window.id)
    }

    fn quarantine_or_retain(&mut self, ticket: mac_object_table::domain::ReleasedResource)
    {
        if let Err(error) = self.windows.quarantine(ticket)
        {
            #[cfg(target_arch = "riscv64")]
            serial!("WINDOW_QUARANTINE_RETAINED: {error}; payload/backing remain pending");
            // Capacity for a refusal per admitted window is pinned before
            // publication. Cleanup neither allocates nor drops this capability.
            self.retained_failures.push_back(error);
        }
    }

    fn retry_retained_quarantines(&mut self)
    {
        // A real create/cleanup checkpoint, never a timer/input scan. A failed
        // entry is tried once and put back; it cannot starve another owner.
        for _ in 0..self.retained_failures.len()
        {
            if let Some(error) = self.retained_failures.pop_front()
               && let Err(error) = self.windows.quarantine(error.into_ticket())
            {
                self.retained_failures.push_back(error);
            }
        }
    }

    pub(crate) fn release_storage(&mut self, ticket: mac_object_table::domain::ReleasedResource)
                                  -> Result<(), Failure>
    {
        self.retry_retained_quarantines();
        self.windows.release_storage(ticket).map_err(Failure::from)
    }

    pub(crate) fn release(&mut self, ticket: mac_object_table::domain::ReleasedResource)
                          -> Result<WindowId, Failure>
    {
        use mac_object_table::registry::CleanupFailure;
        let Some(window) = self.windows.releasing(&ticket).copied()
        else
        {
            return Err(Failure::registration_mismatch(ticket));
        };
        let id = window.id;
        if let Some(index) = self.order.iter().position(|other| *other == id)
        {
            self.front_cache.set(None);
            self.order.remove(index);
            self.len -= 1;
            if !window.hidden
            {
                self.invalidate_uncovered(&window, index);
            }
        }
        if self.active == Some(id)
        {
            self.active = None;
        }
        if matches!(self.tracking, Some(Tracking::Drag {window,..} | Tracking::Grow {window,..} | Tracking::Close {window,..} | Tracking::Zoom {window,..}) if window == id)
        {
            self.tracking = None;
        }
        let Some(reply) = self.windows.with_releasing(&ticket, |window| {
                                          window.call(definition::DISPOSE, 0, core::ptr::null_mut())
                                      })
        else
        {
            return Err(Failure::registration_mismatch(ticket));
        };
        if reply < 0
        {
            return Err(Failure::window_definition(ticket, id.0, window.procedure, reply));
        }
        drop(self.windows.release(ticket).map_err(Failure::from)?);
        Ok(id)
    }

    pub fn count(&self) -> usize
    {
        self.len
    }

    pub(crate) fn window_for_owner(&self, id: WindowId, owner: u64) -> Option<&Window>
    {
        let (bound, window) = self.windows.get_with_owner(id.0)?;
        (window.initialized && bound.incarnation() == owner).then_some(window)
    }

    pub(crate) fn front_record_for_owner(&self, owner: u64) -> Option<&Window>
    {
        if let Some((cached_owner, cached)) = self.front_cache.get()
           && cached_owner == owner
        {
            match cached
            {
                None => return None,
                Some(id) =>
                {
                    if let Some(window) = self.window_for_owner(id, owner).filter(|w| !w.hidden)
                    {
                        return Some(window);
                    }
                }
            }
        }
        let front = self.order[..self.len].iter().rev().find_map(|id| {
                                                           self.window_for_owner(*id, owner)
                                                               .filter(|window| !window.hidden)
                                                       });
        self.front_cache.set(Some((owner, front.map(|window| window.id))));
        front
    }

    pub(crate) fn front_for_owner(&self, owner: u64) -> Option<WindowId>
    {
        self.front_record_for_owner(owner).map(|window| window.id)
    }

    pub fn front_window(&self) -> Option<WindowId>
    {
        self.order[..self.len].iter()
                              .rev()
                              .find(|id| self.window(**id).is_some_and(|w| !w.hidden))
                              .copied()
    }

    /// The desktop may own focus while windows remain in the same z order.
    pub fn active_window(&self) -> Option<WindowId>
    {
        self.active
    }

    pub fn window(&self, id: WindowId) -> Option<&Window>
    {
        self.windows.get(id.0).filter(|window| window.initialized)
    }

    fn window_mut(&mut self, id: WindowId) -> Option<&mut Window>
    {
        self.windows.get_mut(id.0)
    }

    pub fn back_to_front(&self) -> impl Iterator<Item = &Window>
    {
        self.order[..self.len].iter().filter_map(|id| self.window(*id)).filter(|w| !w.hidden)
    }

    #[cfg(test)]
    pub fn new_window(&mut self, bounds: Rect, title: &str, owner: u64, minimum: (i16, i16),
                      events: &mut EventManager, when: u32)
                      -> Option<WindowId>
    {
        self.new_window_with_definition(bounds,
                                        title,
                                        owner,
                                        minimum,
                                        0,
                                        crate::test_wdef::standard_window_definition,
                                        events,
                                        when)
    }

    /// The entry must satisfy the native WDEF ABI for this window's lifetime.
    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    pub fn new_window_with_definition(&mut self, bounds: Rect, title: &str, owner: u64,
                                      minimum: (i16, i16), procedure: i16, entry: DefinitionEntry,
                                      events: &mut EventManager, when: u32)
                                      -> Option<WindowId>
    {
        let owner = self.test_owner(owner);
        self.new_window_with_activation(bounds, title, &owner, minimum, procedure, entry, events,
                                        when, true)
            .ok()
    }

    /// Background application windows enter the z order without transient focus.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_window_with_activation(&mut self, bounds: Rect, title: &str,
                                             owner: &OwnerRef, minimum: (i16, i16),
                                             procedure: i16, entry: DefinitionEntry,
                                             events: &mut EventManager, when: u32, activate: bool)
                                             -> Result<WindowId, CreationError>
    {
        let (min_width, min_height) = minimum;
        if min_width < 1 || min_height < 1
        {
            return Err(CreationError::InvalidRequest);
        }
        if bounds.is_empty()
           || bounds.left < 1
           || bounds.top < 25
           || bounds.right > crate::display::WIDTH as i16 - 2
           || bounds.bottom > crate::display::HEIGHT as i16 - 2
           || bounds.right - bounds.left < min_width
           || bounds.bottom - bounds.top < min_height
        {
            return Err(CreationError::InvalidRequest);
        }
        // These arrays index all owners. Only their capacity is System-owned;
        // the subsequent Window and owner Store backing retain caller charges.
        InvocationGate::with_system_allocation(|| {
            self.visibility
                .get_mut()
                .prepare(self.len.checked_add(1).ok_or(CreationError::NoMemory)?)
                .map_err(|_| CreationError::NoMemory)?;
            self.order.try_reserve(1).map_err(|_| CreationError::NoMemory)?;
            self.order_scratch
                .try_reserve((self.len + 1).saturating_sub(self.order_scratch.len()))
                .map_err(|_| CreationError::NoMemory)?;
            self.geometry_order
                .try_reserve((self.len + 1).saturating_sub(self.geometry_order.len()))
                .map_err(|_| CreationError::NoMemory)
        })?;
        let previous = self.active;
        let mut window = Window { id: WindowId(0),
                                  initialized: false,
                                  bounds,
                                  owner: owner.owner().incarnation(),
                                  min_width,
                                  min_height,
                                  state: WindowDefinitionState::new(bounds, title),
                                  procedure,
                                  entry,
                                  gate: self.gate,
                                  user_bounds: None,
                                  standard_bounds: None,
                                  grow_icon_clip: None,
                                  content_outline: None,
                                  close_deferred: false,
                                  damage: Some(Rect::new(0,
                                                         0,
                                                         bounds.right - bounds.left,
                                                         bounds.bottom - bounds.top)),
                                  hidden: false,
                                  updating: false };
        self.retry_retained_quarantines();
        InvocationGate::with_system_allocation(|| self.retained_failures.try_reserve(self.len + 1))
            .map_err(|_| CreationError::NoMemory)?;
        let id = WindowId(self.windows
                              .insert_with(owner, |id| {
                                  window.id = WindowId(id);
                                  window
                              })
                              .map_err(CreationError::Facility)?);
        let mut guard = InitializingWindow { manager: self,
                                             id,
                                             committed: false };
        for operation in [definition::NEW, definition::CALCULATE]
        {
            let reply = guard.value()?.call(operation, 0, core::ptr::null_mut());
            if reply < 0
            {
                return Err(CreationError::Definition { procedure,
                                                       operation,
                                                       reply });
            }
        }
        let structure = guard.value()?.structure();
        if structure.is_empty()
           || structure.left < 0
           || structure.top < 25
           || structure.right > crate::display::WIDTH as i16
           || structure.bottom > crate::display::HEIGHT as i16
        {
            return Err(CreationError::Geometry { procedure });
        }
        guard.value()?.initialized = true;
        let id = guard.commit();
        self.order.push(id);
        self.len += 1;
        self.front_cache.set(None);
        if activate
        {
            self.active = Some(id);
            self.refresh_active(previous);
            Self::activation(previous, false, events, when);
            Self::activation(Some(id), true, events, when);
        }
        // The new window already owns full initial damage. Covering another
        // window exposes no content there; activation changes only chrome.
        Ok(id)
    }

    fn refresh_active(&mut self, previous: Option<WindowId>)
    {
        if previous == self.active
        {
            return;
        }
        if let Some(window) = previous.and_then(|id| self.window_mut(id))
        {
            window.state.active = 0;
        }
        if let Some(window) = self.active.and_then(|id| self.window_mut(id))
        {
            window.state.active = 1;
        }
    }

    fn activation(id: Option<WindowId>, active: bool, events: &mut EventManager, when: u32)
    {
        if let Some(id) = id
        {
            events.post_system(Event { kind: EventKind::Activate,
                                       message: 0,
                                       when,
                                       where_: events.mouse,
                                       payload: 0,
                                       modifiers: (events.modifiers & !ACTIVE_FLAG)
                                                  | if active { ACTIVE_FLAG } else { 0 },
                                       window: id.0 });
        }
    }

    pub fn select_window(&mut self, id: WindowId, events: &mut EventManager, when: u32)
    {
        if self.front_window() == Some(id) && self.active == Some(id)
        {
            return;
        }
        let Some(index) = self.order[..self.len].iter().position(|other| *other == id)
        else
        {
            return;
        };
        let previous = self.active;
        let mut old_order = core::mem::take(&mut self.order_scratch);
        old_order.clear();
        old_order.extend_from_slice(&self.order);
        self.front_cache.set(None);
        self.order.copy_within(index + 1..self.len, index);
        self.order[self.len - 1] = id;
        self.active = Some(id);
        self.refresh_active(previous);
        if previous != Some(id)
        {
            Self::activation(previous, false, events, when);
            Self::activation(Some(id), true, events, when);
        }
        self.invalidate_order_exposure(&old_order);
        self.order_scratch = old_order;
    }

    /// Raise an application's complete window plane in one transition. The
    /// relative order of both groups stays intact; only the final focus changes.
    pub fn select_group(&mut self, ids: &[WindowId], events: &mut EventManager, when: u32)
    {
        self.order_group(ids, events, when, GroupFocus::Front);
    }

    /// Selecting a background application's nonfront window raises its plane
    /// and chosen window together, without activating the old front in between.
    pub fn select_group_focus(&mut self, ids: &[WindowId], focus: GroupFocus,
                              events: &mut EventManager, when: u32)
    {
        self.order_group(ids, events, when, focus);
    }

    /// Restore a foreground plane without changing its current window or
    /// desktop focus after another application creates a background window.
    pub fn restore_group(&mut self, ids: &[WindowId], events: &mut EventManager, when: u32)
    {
        self.order_group(ids, events, when, GroupFocus::Preserve);
    }

    /// Raise a process plane using ID storage admitted with its windows. This
    /// also serves cleanup focus changes when the allocator refuses new work.
    pub(crate) fn select_owner_group_focus(&mut self, owner: u64, focus: GroupFocus,
                                           events: &mut EventManager, when: u32)
    {
        let mut ids = core::mem::take(&mut self.geometry_order);
        ids.clear();
        ids.extend(self.back_to_front()
                       .filter(|window| window.owner == owner)
                       .map(|window| window.id));
        self.select_group_focus(&ids, focus, events, when);
        self.geometry_order = ids;
    }

    /// Restore a foreground plane without allocating on background creation.
    pub(crate) fn restore_owner_group(&mut self, owner: u64, events: &mut EventManager, when: u32)
    {
        self.select_owner_group_focus(owner, GroupFocus::Preserve, events, when);
    }

    fn order_group(&mut self, ids: &[WindowId], events: &mut EventManager, when: u32,
                   focus: GroupFocus)
    {
        let mut order = core::mem::take(&mut self.order_scratch);
        order.clear();
        for selected in [false, true]
        {
            for id in self.order[..self.len].iter().copied()
            {
                if ids.contains(&id) == selected
                {
                    order.push(id);
                }
            }
        }
        if let GroupFocus::Window(front) = focus
           && ids.contains(&front)
           && let Some(index) = order[..self.len].iter().position(|id| *id == front)
        {
            order.copy_within(index + 1..self.len, index);
            order[self.len - 1] = front;
        }
        let active = match focus
        {
            GroupFocus::Front | GroupFocus::Window(_) =>
            {
                order[..self.len].iter().rev().copied().find(|id| {
                                                           ids.contains(id)
                                                               && self.window(*id)
                                                                      .is_some_and(|window| {
                                                                          !window.hidden
                                                                      })
                                                       })
            }
            GroupFocus::Desktop => None,
            GroupFocus::Preserve => self.active
        };
        let reordered = self.order[..self.len] != order[..self.len];
        let previous = self.active;
        if !reordered && previous == active
        {
            self.order_scratch = order;
            return;
        }
        self.front_cache.set(None);
        core::mem::swap(&mut self.order, &mut order);
        self.invalidate_order_exposure(&order);
        self.order_scratch = order;
        self.active = active;
        self.refresh_active(previous);
        if previous != active
        {
            Self::activation(previous, false, events, when);
            Self::activation(active, true, events, when);
        }
    }

    pub fn deactivate_all(&mut self, events: &mut EventManager, when: u32)
    {
        let Some(previous) = self.active.take()
        else
        {
            return;
        };
        self.refresh_active(Some(previous));
        self.tracking = None;
        Self::activation(Some(previous), false, events, when);
    }

    pub(crate) fn dispose_window(&mut self, id: WindowId, events: &mut EventManager, when: u32)
                                 -> Result<(), CloseError>
    {
        use mac_object_table::registry::CleanupFailure;
        self.retry_retained_quarantines();
        let active = self.active == Some(id);
        let ticket = self.windows.begin_release(id.0).ok_or(CloseError::NotFound(id))?;
        let result = match self.release(ticket)
        {
            Ok(_) => Ok(()),
            Err(failure) =>
            {
                let (ticket, cause) = failure.into_parts();
                self.quarantine_or_retain(ticket);
                Err(CloseError::Cleanup(cause))
            }
        };
        if active
        {
            self.active = self.front_window();
            self.refresh_active(None);
            Self::activation(self.active, true, events, when);
        }
        result
    }

    pub(crate) fn set_owner_hidden(&mut self, owner: u64, hidden: bool)
    {
        for index in 0..self.len
        {
            let id = self.order[index];
            if self.window(id).is_some_and(|window| window.owner == owner)
            {
                self.set_hidden(id, hidden);
            }
        }
    }

    /// Batch Hide Others without rescanning every plane for each process or
    /// damaging windows that are themselves about to disappear. Order/focus and
    /// activation events stay unchanged. Noneligible lifetime domains retain
    /// their windows and contribute surviving occlusion. The caller validates
    /// a genuinely running visible foreground process.
    pub(crate) fn hide_other_owners(&mut self, owner: u64, mut eligible: impl FnMut(u64) -> bool)
                                    -> bool
    {
        let mut hidden = core::mem::take(&mut self.geometry_order);
        hidden.clear();
        let mut first_survivor = self.len;
        let mut survivors = 0;
        let mut last_hidden = 0;
        for index in 0..self.len
        {
            let id = self.order[index];
            let Some(window) = self.window_mut(id)
            else
            {
                continue;
            };
            if window.hidden
            {
                continue;
            }
            if window.owner == owner || !eligible(window.owner)
            {
                first_survivor = first_survivor.min(index);
                survivors += 1;
            }
            else
            {
                window.hidden = true;
                hidden.push(id);
                last_hidden = index;
            }
        }
        if hidden.is_empty()
        {
            self.geometry_order = hidden;
            return false;
        }
        self.front_cache.set(None);
        // The normal foreground plane is already entirely in front. Removing
        // its background planes exposes no application content at all.
        if last_hidden < first_survivor
        {
            self.geometry_order = hidden;
            return true;
        }
        let mut removed = Region::new();
        let mut surviving_cover = Region::new();
        let mut changed = hidden.iter().rev().peekable();
        for index in (0..self.len).rev()
        {
            let id = self.order[index];
            let Some(window) = self.window(id).copied()
            else
            {
                continue;
            };
            if changed.peek().is_some_and(|&&next| next == id)
            {
                changed.next();
                for &piece in window.structure_pieces()
                {
                    let piece = Region::from_rect(piece);
                    // Keeping the whole piece after scratch refusal is a
                    // conservative exposure, never lost damage.
                    let visible =
                        piece.combine(&surviving_cover, BooleanOp::Difference).unwrap_or(piece);
                    removed =
                        removed.combine(&visible, BooleanOp::Union)
                               .unwrap_or_else(|_| {
                                   Region::from_rect(union_rect(removed.bounds(), visible.bounds()))
                               });
                }
            }
            else if !window.hidden
            {
                let mut global = Rect::ZERO;
                if sect_rect(removed.bounds(), window.bounds, &mut global)
                {
                    let fallback = Rect::new(global.left - window.bounds.left,
                                             global.top - window.bounds.top,
                                             global.right - window.bounds.left,
                                             global.bottom - window.bounds.top);
                    let exposed = removed.combine(&Region::from_rect(window.bounds),
                                                  BooleanOp::Intersection)
                                         .and_then(|region| {
                                             region.offset(window.bounds
                                                       .left
                                                       .checked_neg()
                                                       .ok_or(RegionError::CoordinateOverflow)?,
                                                 window.bounds
                                                       .top
                                                       .checked_neg()
                                                       .ok_or(RegionError::CoordinateOverflow)?)
                                         });
                    self.invalidate_exposure(id, exposed, fallback);
                }
                survivors -= 1;
                if survivors == 0
                {
                    // Lower removed planes can expose only desktop pixels;
                    // retained desktop composition already owns that work.
                    break;
                }
                for &piece in window.structure_pieces()
                {
                    let piece = Region::from_rect(piece);
                    // Refused subtraction may over-report a bounded update;
                    // refused cover growth keeps an exact subset. Never use a
                    // bounding box as cover: that could erase real exposure.
                    if let Ok(remaining) = removed.combine(&piece, BooleanOp::Difference)
                    {
                        removed = remaining;
                    }
                    if let Ok(cover) = surviving_cover.combine(&piece, BooleanOp::Union)
                    {
                        surviving_cover = cover;
                    }
                }
            }
        }
        self.geometry_order = hidden;
        true
    }

    pub(crate) fn set_all_hidden(&mut self, hidden: bool)
    {
        for index in 0..self.len
        {
            self.set_hidden(self.order[index], hidden);
        }
    }

    pub fn set_hidden(&mut self, id: WindowId, hidden: bool)
    {
        let Some(window) = self.window_mut(id)
        else
        {
            return;
        };
        if window.hidden == hidden
        {
            return;
        }
        let previous = *window;
        window.hidden = hidden;
        self.front_cache.set(None);
        if hidden
        {
            if let Some(index) = self.order.iter().position(|other| *other == id)
            {
                self.invalidate_uncovered(&previous, index);
            }
        }
        else
        {
            // A previously hidden window has no visible pixels. Damage only
            // its own newly visible content, never the windows it now covers.
            let visible = self.local_visibility(id, &self.order, None);
            let bounds = self.window(id).map_or(Rect::ZERO, |window| window.local_bounds());
            self.invalidate_exposure(id, visible, bounds);
        }
    }

    pub fn next_update_matching(&self, events: &EventManager, when: u32,
                                owns: impl Fn(WindowId) -> bool)
                                -> Option<Event>
    {
        self.order[..self.len].iter().rev().filter(|id| owns(**id)).find_map(|id| {
                                                                       self.next_update_for(*id,
                                                                                            events,
                                                                                            when)
                                                                   })
    }

    pub fn invalidate(&mut self, id: WindowId, rect: Rect)
    {
        if let Some(window) = self.window_mut(id)
        {
            let mut clipped = Rect::ZERO;
            if sect_rect(rect, window.local_bounds(), &mut clipped)
            {
                window.damage = Some(window.damage.map_or(clipped, |old| union_rect(old, clipped)));
            }
        }
    }

    pub fn next_update(&self, events: &EventManager, when: u32) -> Option<Event>
    {
        let id = self.order[..self.len].iter().rev().find(|id| {
                                                         self.window(**id).is_some_and(|w| {
                                                                              !w.hidden
                                                                              && w.damage.is_some()
                                                                              && !w.updating
                                                                          })
                                                     })?;
        Some(Event { kind: EventKind::Update,
                     message: 0,
                     when,
                     where_: events.mouse,
                     payload: 0,
                     modifiers: events.modifiers,
                     window: id.0 })
    }

    /// During modal alerts, leave background damage queued for its application.
    pub fn next_update_for(&self, id: WindowId, events: &EventManager, when: u32) -> Option<Event>
    {
        let window = self.window(id)?;
        if window.hidden || window.damage.is_none() || window.updating
        {
            return None;
        }
        Some(Event { kind: EventKind::Update,
                     message: 0,
                     when,
                     where_: events.mouse,
                     payload: 0,
                     modifiers: events.modifiers,
                     window: id.0 })
    }

    pub fn begin_update(&mut self, id: WindowId) -> Option<Rect>
    {
        let window = self.window_mut(id)?;
        if window.updating
        {
            return None;
        }
        let damage = window.damage.take()?;
        window.updating = true;
        Some(damage)
    }

    /// Retained content already covers exposure. A client replacing its model
    /// asynchronously may acknowledge that exposure without repainting it.
    pub fn validate(&mut self, id: WindowId) -> Result<(), mac_abi::Status>
    {
        let window = self.window_mut(id).ok_or(mac_abi::NOT_FOUND)?;
        if window.updating
        {
            return Err(mac_abi::SERVICE_BUSY);
        }
        window.damage = None;
        Ok(())
    }

    pub fn end_update(&mut self, id: WindowId)
    {
        if let Some(window) = self.window_mut(id)
        {
            window.updating = false;
        }
    }

    /// A scene patch must not consume or expand an unrelated exposure region.
    pub fn begin_partial_update(&mut self, id: WindowId) -> bool
    {
        let Some(window) = self.window_mut(id)
        else
        {
            return false;
        };
        if window.updating
        {
            return false;
        }
        window.updating = true;
        true
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn content_outline(&mut self, id: WindowId, next: Option<Rect>)
                                  -> Result<bool, mac_abi::Status>
    {
        let window = self.window_mut(id).ok_or(mac_abi::NOT_FOUND)?;
        if window.content_outline == next
        {
            return Ok(false);
        }
        window.content_outline = next;
        Ok(true)
    }

    pub fn find_window(&self, point: Point) -> Hit
    {
        if point.v >= 0 && point.v < 25
        {
            return Hit { part:   WindowPart::MenuBar,
                         window: None };
        }
        for id in self.order[..self.len].iter().rev()
        {
            let Some(window) = self.window(*id)
            else
            {
                continue;
            };
            if window.hidden || !window.structure_pieces().iter().any(|rect| rect.contains(point))
            {
                continue;
            }
            let mut probe = *window;
            let param = ((point.v as u16 as u32) << 16) | point.h as u16 as u32;
            let part = match probe.call(definition::HIT, param as i64, core::ptr::null_mut())
            {
                1 => WindowPart::Content,
                2 => WindowPart::Drag,
                3 => WindowPart::Grow,
                4 => WindowPart::GoAway,
                5 => WindowPart::ZoomIn,
                6 => WindowPart::ZoomOut,
                _ => continue
            };
            return Hit { part,
                         window: Some(*id) };
        }
        Hit { part:   WindowPart::Desk,
              window: None }
    }

    pub fn start_tracking(&mut self, hit: Hit, point: Point)
    {
        self.start_tracking_with_modifiers(hit, point, 0);
    }

    pub fn is_tracking(&self) -> bool
    {
        self.tracking.is_some()
    }

    /// Only grow/pressed chrome still belongs to the scene compositor.
    pub fn tracking_repaints_scene(&self) -> bool
    {
        matches!(self.tracking,
                 Some(Tracking::Grow { .. } | Tracking::Close { .. } | Tracking::Zoom { .. }))
    }

    pub(crate) fn drag_outline(&self) -> Option<Outline>
    {
        let Tracking::Drag { window,
                             bounds,
                             preview: Some(preview),
                             .. } = self.tracking?
        else
        {
            return None;
        };
        Outline::new(self.window(window)?.structure_pieces(),
                     i32::from(preview.left) - i32::from(bounds.left),
                     i32::from(preview.top) - i32::from(bounds.top))
    }

    /// Native desktop tracking policy. Command-drag leaves activation and z order
    /// unchanged; an ordinary background drag activates only after acceptance.
    pub fn start_tracking_with_modifiers(&mut self, hit: Hit, point: Point, modifiers: u16)
    {
        let Some(window) = hit.window.and_then(|id| self.window(id)).copied()
        else
        {
            return;
        };
        self.tracking = match hit.part
        {
            WindowPart::Drag =>
            {
                Some(Tracking::Drag { window:   window.id,
                                      start:    point,
                                      bounds:   window.bounds,
                                      preview:  Some(window.bounds),
                                      activate: modifiers & CMD_KEY == 0 })
            }
            WindowPart::Grow =>
            {
                Some(Tracking::Grow { window:  window.id,
                                      start:   point,
                                      bounds:  window.bounds,
                                      preview: window.bounds })
            }
            WindowPart::ZoomIn | WindowPart::ZoomOut =>
            {
                Some(Tracking::Zoom { window: window.id,
                                      armed:  true })
            }
            WindowPart::GoAway =>
            {
                Some(Tracking::Close { window: window.id,
                                       armed:  true })
            }
            _ => None
        };
    }

    pub fn track(&mut self, point: Point, released: bool, events: &mut EventManager, when: u32)
                 -> Option<Action>
    {
        let mut order = core::mem::take(&mut self.geometry_order);
        order.clear();
        if released
           && matches!(self.tracking,
                       Some(Tracking::Drag { .. } | Tracking::Grow { .. } | Tracking::Zoom { .. }))
        {
            order.extend_from_slice(&self.order);
        }
        let result = self.track_prepared(point, released, events, when, &order);
        self.geometry_order = order;
        result
    }

    fn track_prepared(&mut self, point: Point, released: bool, events: &mut EventManager,
                      when: u32, old_order: &[WindowId])
                      -> Option<Action>
    {
        let tracking = self.tracking?;
        // Snapshot geometry only on release. Mouse motion keeps its existing
        // outline-only path and never builds content visibility regions.
        let previous = if released
        {
            match tracking
            {
                Tracking::Drag { window,
                                 .. }
                | Tracking::Grow { window,
                                 .. }
                | Tracking::Zoom { window,
                                 .. } => self.window(window).copied().map(|w| (w, old_order)),
                _ => None
            }
        }
        else
        {
            None
        };
        let mut action = None;
        match tracking
        {
            Tracking::Zoom { window,
                             .. } =>
            {
                let armed = self.window(window).is_some_and(|w| w.state.zoom_box.contains(point));
                self.tracking = Some(Tracking::Zoom { window,
                                                      armed });
                if released
                   && armed
                   && let Some(w) = self.window_mut(window)
                {
                    if let Some(user) = w.user_bounds.take()
                    {
                        w.bounds = user;
                        w.state.zoomed = 0;
                    }
                    else
                    {
                        w.user_bounds = Some(w.bounds);
                        let structure = w.structure();
                        w.bounds =
                            w.standard_bounds.unwrap_or_else(|| {
                                                 Rect::new(w.bounds.left - structure.left,
                                                           25 + w.bounds.top - structure.top,
                                                           crate::display::WIDTH as i16
                                                           - (structure.right - w.bounds.right),
                                                           (crate::display::HEIGHT as i16 - 21)
                                                           - (structure.bottom - w.bounds.bottom))
                                             });
                        w.state.zoomed = 1;
                    }
                    w.calculate();
                    action = Some(Action::Resize(window));
                }
            }
            Tracking::Drag { window,
                             start,
                             bounds,
                             activate,
                             .. } =>
            {
                // DragWindow constrains the cursor, not the entire window. A
                // release outside the desktop cancels, including activation.
                let preview = Rect::new(0, 21, crate::display::WIDTH as i16, crate::display::HEIGHT as i16).contains(point).then(|| {
                    let dx = i32::from(point.h) - i32::from(start.h);
                    let dy = i32::from(point.v) - i32::from(start.v);
                    Rect::new((i32::from(bounds.left) + dx) as i16,
                              (i32::from(bounds.top) + dy) as i16,
                              (i32::from(bounds.right) + dx) as i16,
                              (i32::from(bounds.bottom) + dy) as i16)
                });
                self.tracking = Some(Tracking::Drag { window,
                                                      start,
                                                      bounds,
                                                      preview,
                                                      activate });
                if released && let Some(next) = preview
                {
                    if let Some(w) = self.window_mut(window)
                       && next != w.bounds
                    {
                        w.bounds = next;
                        w.user_bounds = None;
                        w.state.zoomed = 0;
                        w.calculate();
                        action = Some(Action::Drag(window));
                    }
                    if activate
                    {
                        self.select_window(window, events, when);
                    }
                }
            }
            Tracking::Grow { window,
                             start,
                             bounds,
                             .. } =>
            {
                let w = self.window(window)?;
                let structure = w.structure();
                let width = (i32::from(bounds.right - bounds.left) + i32::from(point.h)
                             - i32::from(start.h)).clamp(i32::from(w.min_width),
                                                       i32::from(crate::display::WIDTH as i16
                                                                 - (structure.right - w.bounds.right)
                                                                 - bounds.left).max(i32::from(w.min_width)))
                            as i16;
                let height = (i32::from(bounds.bottom - bounds.top) + i32::from(point.v)
                              - i32::from(start.v)).clamp(i32::from(w.min_height),
                                                         i32::from(crate::display::HEIGHT as i16
                                                                   - (structure.bottom - w.bounds.bottom)
                                                                   - bounds.top).max(i32::from(w.min_height)))
                             as i16;
                let next =
                    Rect::new(bounds.left, bounds.top, bounds.left + width, bounds.top + height);
                self.tracking = Some(Tracking::Grow { window,
                                                      start,
                                                      bounds,
                                                      preview: next });
                if released
                   && let Some(w) = self.window_mut(window)
                   && next != w.bounds
                {
                    w.bounds = next;
                    w.user_bounds = None;
                    w.state.zoomed = 0;
                    w.calculate();
                    action = Some(Action::Resize(window));
                }
            }
            Tracking::Close { window,
                              .. } =>
            {
                let armed = self.window(window).is_some_and(|w| w.close_box().contains(point));
                self.tracking = Some(Tracking::Close { window,
                                                       armed });
                if released && armed
                {
                    if self.window(window).is_some_and(|w| w.close_deferred)
                    {
                        action = Some(Action::CloseRequest(window));
                    }
                    else
                    {
                        let owner = self.window(window)?.owner;
                        if let Err(error) = self.dispose_window(window, events, when)
                        {
                            #[cfg(target_arch = "riscv64")]
                            serial!("WINDOW_CLOSE_REFUSED: owner={owner} {error}");
                            #[cfg(not(target_arch = "riscv64"))]
                            let _ = error;
                        }
                        action = Some(Action::Close(window, owner));
                    }
                }
            }
        }
        if released
        {
            self.tracking = None;
        }
        if let (Some(action @ (Action::Drag(_) | Action::Resize(_))), Some((window, order))) =
            (action, previous)
        {
            self.invalidate_geometry_exposure(&window, &order, action);
        }
        action
    }

    fn local_visibility(&self, id: WindowId, order: &[WindowId], previous: Option<&Window>)
                        -> Result<Region, RegionError>
    {
        let window_at = |id| previous.filter(|w| w.id == id).or_else(|| self.window(id));
        let Some(window) = window_at(id).filter(|w| !w.hidden)
        else
        {
            return Ok(Region::new());
        };
        let Some(index) = order.iter().position(|other| *other == id)
        else
        {
            return Ok(Region::new());
        };
        let mut visible =
            Region::from_rect(window.bounds).combine(&Region::from_rect(Rect::new(0, 20, crate::display::WIDTH as i16,
                                                                                  crate::display::HEIGHT as i16)),
                                                     BooleanOp::Intersection)?;
        for front in &order[index + 1..]
        {
            if let Some(front) = window_at(*front).filter(|w| !w.hidden)
            {
                for piece in front.structure_pieces()
                {
                    visible = visible.combine(&Region::from_rect(*piece), BooleanOp::Difference)?;
                    if visible.is_empty()
                    {
                        return Ok(visible);
                    }
                }
            }
        }
        // Visible screen coordinates and bounded window positions guarantee
        // these local coordinates fit the existing i16 QuickDraw contract.
        visible.offset(window.bounds.left.checked_neg().ok_or(RegionError::CoordinateOverflow)?,
                       window.bounds.top.checked_neg().ok_or(RegionError::CoordinateOverflow)?)
    }

    fn invalidate_region(&mut self, id: WindowId, region: &Region)
    {
        for band in region.bands()
        {
            for span in &band.spans
            {
                self.invalidate(id, Rect::new(span.left, band.top, span.right, band.bottom));
            }
        }
    }

    /// Geometry scratch refusal must retain damage after a committed plane/close
    /// change. Bound the recovery update to this affected object's content; the
    /// compositor still applies ordinary occlusion. Healthy paths remain exact.
    fn invalidate_exposure(&mut self, id: WindowId, region: Result<Region, RegionError>,
                           fallback: Rect)
    {
        match region
        {
            Ok(region) => self.invalidate_region(id, &region),
            Err(_) => self.invalidate(id, fallback)
        }
    }

    /// A plane change damages only pixels that became visible. Focus chrome
    /// is composited by System and is not application content damage.
    fn invalidate_order_exposure(&mut self, previous: &[WindowId])
    {
        for index in 0..self.len
        {
            let id = self.order[index];
            if previous.iter().position(|old| *old == id) == Some(index)
               && previous[index + 1..] == self.order[index + 1..]
            {
                continue;
            }
            let before = self.local_visibility(id, previous, None);
            let after = self.local_visibility(id, &self.order, None);
            let exposed =
                after.and_then(|after| {
                         before.and_then(|before| after.combine(&before, BooleanOp::Difference))
                     });
            let bounds = self.window(id).map_or(Rect::ZERO, |window| window.local_bounds());
            self.invalidate_exposure(id, exposed, bounds);
        }
    }

    /// Removing or hiding one structure can expose only pixels underneath it.
    /// This also serves invalidate-first owner cleanup, whose ID no longer
    /// resolves but whose unique cleanup ticket still retains the geometry.
    fn invalidate_uncovered(&mut self, previous: &Window, behind: usize)
    {
        for index in 0..behind
        {
            let id = self.order[index];
            let Some(window) = self.window(id).filter(|w| !w.hidden).copied()
            else
            {
                continue;
            };
            let mut overlap = Rect::ZERO;
            if !sect_rect(window.bounds, previous.structure(), &mut overlap)
            {
                continue;
            }
            let (left, top) = (window.bounds.left, window.bounds.top);
            let fallback = Rect::new(overlap.left - left,
                                     overlap.top - top,
                                     overlap.right - left,
                                     overlap.bottom - top);
            let visible = self.local_visibility(id, &self.order, None);
            match visible
            {
                Err(_) => self.invalidate(id, fallback),
                Ok(visible) =>
                {
                    for piece in previous.structure_pieces()
                    {
                        let mut overlap = Rect::ZERO;
                        if !sect_rect(window.bounds, *piece, &mut overlap)
                        {
                            continue;
                        }
                        let region=Region::from_rect(overlap).offset(-left,-top)
                        .and_then(|piece|visible.combine(&piece,BooleanOp::Intersection));
                        self.invalidate_exposure(id, region, fallback);
                    }
                }
            }
        }
    }

    fn invalidate_geometry_exposure(&mut self, previous: &Window, old_order: &[WindowId],
                                    action: Action)
    {
        let affected =
            self.window(previous.id)
                .map_or(previous.structure(), |w| union_rect(previous.structure(), w.structure()));
        for index in 0..self.len
        {
            let id = self.order[index];
            if id != previous.id
               && self.window(id).is_none_or(|w| {
                                     let mut overlap = Rect::ZERO;
                                     !sect_rect(w.bounds, affected, &mut overlap)
                                 })
            {
                continue;
            }
            if matches!(action,Action::Resize(target) if target==id)
            {
                // A size change requires the owner to lay out its content even
                // when shrinking exposes no new local pixels.
                if let Some(w) = self.window(id)
                {
                    self.invalidate(id, w.local_bounds());
                }
                continue;
            }
            let before = self.local_visibility(id, &old_order[..self.len], Some(previous));
            let after = self.local_visibility(id, &self.order[..self.len], None);
            let exposed =
                after.and_then(|after| {
                         before.and_then(|before| after.combine(&before, BooleanOp::Difference))
                     });
            let fallback = self.window(id).map_or(Rect::ZERO, |window| {
                                              if id == previous.id
                                              {
                                                  return window.local_bounds();
                                              }
                                              let mut clipped = Rect::ZERO;
                                              if !sect_rect(window.bounds, affected, &mut clipped)
                                              {
                                                  return Rect::ZERO;
                                              }
                                              Rect::new(clipped.left - window.bounds.left,
                                                        clipped.top - window.bounds.top,
                                                        clipped.right - window.bounds.left,
                                                        clipped.bottom - window.bounds.top)
                                          });
            self.invalidate_exposure(id, exposed, fallback);
        }
    }

    pub fn set_title(&mut self, id: WindowId, title: &str) -> bool
    {
        let Some(window) = self.window_mut(id)
        else
        {
            return false;
        };
        if title.len() > window.state.title.len()
        {
            return false;
        }
        window.state.title.fill(0);
        window.state.title[..title.len()].copy_from_slice(title.as_bytes());
        window.state.title_length = title.len() as u32;
        true
    }

    pub fn set_close_deferred(&mut self, id: WindowId, deferred: bool) -> bool
    {
        let Some(window) = self.window_mut(id)
        else
        {
            return false;
        };
        window.close_deferred = deferred;
        true
    }

    pub fn set_standard_bounds(&mut self, id: WindowId, bounds: Option<Rect>)
                               -> Result<(), Status>
    {
        let window = self.window_mut(id).ok_or(NOT_FOUND)?;
        if window.standard_bounds == bounds
        {
            return Ok(());
        }
        if let Some(bounds) = bounds
        {
            if i32::from(bounds.right) - i32::from(bounds.left) < i32::from(window.min_width)
               || i32::from(bounds.bottom) - i32::from(bounds.top) < i32::from(window.min_height)
            {
                return Err(BAD_ARGUMENT);
            }
            let mut proposed = *window;
            proposed.bounds = bounds;
            if !proposed.calculate()
            {
                return Err(BAD_ARGUMENT);
            }
            let structure = proposed.structure();
            if structure.left < 0
               || structure.top < 25
               || structure.right > crate::display::WIDTH as i16
               || structure.bottom > (crate::display::HEIGHT as i16 - 21)
            {
                return Err(BAD_ARGUMENT);
            }
        }
        window.standard_bounds = bounds;
        Ok(())
    }

    pub fn set_grow_icon_clip(&mut self, id: WindowId, clip: Option<Rect>) -> bool
    {
        let Some(window) = self.window_mut(id)
        else
        {
            return false;
        };
        window.grow_icon_clip = clip;
        true
    }

    /// Content is clipped to visible fragments; the callback may be called more
    /// than once per window. Retained application content makes these paints cheap.
    pub fn draw(&self, c: &mut Canvas, mut paint: impl FnMut(&Window, &mut Canvas))
    {
        self.draw_with_scratch(c, None, &mut paint);
    }

    /// The resident compositor supplies a reusable mask. Other renderers can
    /// omit scratch and retain the ordinary independently owned drawing path.
    pub fn draw_with_scratch(&self, c: &mut Canvas, scratch: Option<&mut [u32]>,
                             mut paint: impl FnMut(&Window, &mut Canvas))
    {
        for window in self.back_to_front()
        {
            c.reset_port();
            let mut copy = *window;
            copy.state.close_pressed = u32::from(matches!(self.tracking,
                Some(Tracking::Close {window: id, armed: true}) if id == window.id));
            copy.state.zoom_pressed = u32::from(matches!(self.tracking,
                Some(Tracking::Zoom {window: id, armed: true}) if id == window.id));
            let mut surface = Surface::from_canvas(c);
            copy.call(definition::DRAW, 0, &mut surface);
            self.for_each_visible_rect(window.id, |rect| {
                    c.reset_port();
                    c.set_origin(window.bounds.left, window.bounds.top);
                    let mut local = rect;
                    local.offset(-window.bounds.left, -window.bounds.top);
                    c.set_clip(local);
                    paint(window, c);
                });
            c.reset_port();
            if let Some(mut clip) = window.grow_icon_clip
            {
                clip.offset(window.bounds.left, window.bounds.top);
                c.set_clip(clip);
            }
            let mut surface = Surface::from_canvas(c);
            copy.call(definition::DRAW_GROW_ICON, 0, &mut surface);
        }
        c.reset_port();
        self.draw_tracking(c, scratch);
    }

    /// Tracking overlays never mutate window geometry, retained content or damage.
    /// Grow geometry belongs to the loaded definition, not to the manager.
    fn draw_tracking(&self, canvas: &mut Canvas, scratch: Option<&mut [u32]>)
    {
        self.draw_tracking_at(canvas, scratch, 21.min(canvas.height));
    }

    fn draw_tracking_at(&self, canvas: &mut Canvas, scratch: Option<&mut [u32]>, mask_top: usize)
    {
        let Some(Tracking::Grow { window,
                                  preview,
                                  .. }) = self.tracking
        else
        {
            return;
        };
        let mask_height = canvas.height - mask_top;
        if mask_height == 0
        {
            return;
        }
        let count = canvas.width * mask_height;
        let mut owned = alloc::vec::Vec::new();
        let mask = if let Some(scratch) = scratch.filter(|scratch| scratch.len() >= count)
        {
            &mut scratch[..count]
        }
        else
        {
            if InvocationGate::with_system_allocation(|| owned.try_reserve_exact(count)).is_err()
            {
                return;
            }
            owned.resize(count, 0);
            &mut owned
        };
        mask.fill(0xffffff);
        let Some(mut window) = self.window(window).copied()
        else
        {
            return;
        };
        let mut target = Canvas::new(mask, canvas.width, mask_height);
        target.set_origin(0, -(mask_top as i16));
        let mut surface = Surface::from_canvas(&mut target);
        if window.call(definition::GROW, (&preview as *const Rect) as i64, &mut surface) < 0
        {
            return;
        }
        // Classic gray pen pattern, XORed over the composed desktop. Rebuilding
        // the retained scene erases old outlines without damaging covered ports.
        for y in 21..canvas.height
        {
            for x in 0..canvas.width
            {
                let index = y * canvas.width + x;
                if mask[(y - mask_top) * canvas.width + x] != 0xffffff && (x + y) & 1 == 1
                {
                    canvas.pixels[index] = indexed_tracking_xor(canvas.pixels[index]);
                }
            }
        }
    }

    /// Enumerate non-overlapping global content rectangles after subtracting
    /// higher windows' structure regions. Rectangular regions are the first cut.
    pub fn for_each_visible_rect(&self, id: WindowId, visit: impl FnMut(Rect))
    {
        let Some(index) = self.order[..self.len].iter().position(|other| *other == id)
        else
        {
            return;
        };
        let Some(window) = self.window(id)
        else
        {
            return;
        };
        self.visible_fragments(window.bounds, &self.order[index + 1..self.len], visit);
    }

    /// Desktop damage is clipped below every visible window's structure.
    pub fn for_each_desktop_rect(&self, bounds: Rect, visit: impl FnMut(Rect))
    {
        self.visible_fragments(bounds, &self.order[..self.len], visit);
    }

    fn visible_fragments(&self, bounds: Rect, covers: &[WindowId], visit: impl FnMut(Rect))
    {
        let rectangles = covers.iter()
                               .filter_map(|id| self.window(*id))
                               .filter(|window| !window.hidden)
                               .flat_map(|window| window.structure_pieces().iter().copied());
        self.visibility.borrow_mut().visit(bounds, rectangles, visit);
    }
}

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn owner_front_queries_follow_hide_show_group_order_and_domain_changes()
    {
        let (mut manager, mut events, a, b) = make();
        let owner = manager.owner(a).unwrap();
        assert_eq!(manager.front_for_owner(owner), Some(b));
        manager.set_hidden(b, true);
        assert_eq!(manager.front_for_owner(owner), Some(a));
        manager.set_hidden(a, true);
        assert_eq!(manager.front_for_owner(owner), None);
        manager.set_hidden(b, false);
        assert_eq!(manager.front_for_owner(owner), Some(b));
        manager.set_hidden(a, false);
        manager.select_group_focus(&[a, b], GroupFocus::Window(a), &mut events, 1);
        assert_eq!(manager.front_for_owner(owner), Some(a));
        let other = manager.new_window(Rect::new(30, 60, 200, 180),
                                       "Other domain",
                                       99,
                                       (20, 20),
                                       &mut events,
                                       2)
                           .unwrap();
        let other_owner = manager.owner(other).unwrap();
        assert_eq!(manager.front_for_owner(other_owner), Some(other));
        assert_eq!(manager.front_for_owner(owner), Some(a));
        assert!(manager.window_for_owner(a, other_owner).is_none());
        assert!(manager.window_for_owner(other, owner).is_none());
        manager.dispose_window(a, &mut events, 3).unwrap();
        assert_eq!(manager.front_for_owner(owner), Some(b));
        manager.dispose_window(b, &mut events, 4).unwrap();
        assert_eq!(manager.front_for_owner(owner), None);
        assert_eq!(manager.front_for_owner(other_owner), Some(other));
    }

    #[test]
    fn window_admission_uses_the_configured_right_and_bottom_edges()
    {
        let (mut wm, mut events, _, _) = make();
        let right = crate::display::WIDTH as i16;
        let bottom = crate::display::HEIGHT as i16;
        let id = wm.new_window(Rect::new(right - 140, bottom - 100, right - 12, bottom - 12),
                               "Platform corner",
                               99,
                               (20, 20),
                               &mut events,
                               0)
                   .unwrap();
        let structure = wm.window(id).unwrap().structure();
        assert!(structure.right <= right && structure.bottom <= bottom);
        assert!(wm.new_window(Rect::new(right - 100, bottom - 100, right + 1, bottom - 10),
                              "Outside platform",
                              99,
                              (20, 20),
                              &mut events,
                              0)
                  .is_none());
    }

    #[test]
    fn desktop_fragments_exclude_all_visible_window_structures()
    {
        let (mut wm, _events, _a, b) = make();
        for hide in [false, true]
        {
            wm.set_hidden(b, hide);
            let bounds = Rect::new(0, 21, 640, 480);
            let mut visible = vec![0u8; 640 * 480];
            wm.for_each_desktop_rect(bounds, |rect| {
                  assert!(rect.left >= bounds.left
                          && rect.top >= bounds.top
                          && rect.right <= bounds.right
                          && rect.bottom <= bounds.bottom);
                  for y in rect.top..rect.bottom
                  {
                      for x in rect.left..rect.right
                      {
                          visible[y as usize * 640 + x as usize] += 1;
                      }
                  }
              });
            for y in 0..480
            {
                for x in 0..640
                {
                    let point = Point { h: x as i16,
                                        v: y as i16 };
                    let exposed = bounds.contains(point)
                                  && !wm.back_to_front().any(|window| {
                                                            window.structure_pieces()
                                                                  .iter()
                                                                  .any(|r| r.contains(point))
                                                        });
                    assert_eq!(visible[y * 640 + x], u8::from(exposed));
                }
            }
        }
    }
    #[test]
    fn visible_fragments_handle_eighty_windows_without_overlap()
    {
        let mut wm = WindowManager::new();
        let mut events = EventManager::new();
        for i in 0..80
        {
            let x = 16 + (i * 37 % 320) as i16;
            let y = 60 + (i * 23 % 180) as i16;
            let id = wm.new_window(Rect::new(x, y, x + 160, y + 110),
                                   "Cover",
                                   0,
                                   (100, 60),
                                   &mut events,
                                   0)
                       .unwrap();
            wm.set_hidden(id, i % 5 == 0);
        }
        let bounds = Rect::new(0, 21, 640, 480);
        let mut pixels = vec![0u8; 640 * 480];
        wm.for_each_desktop_rect(bounds, |rect| {
              assert!(!rect.is_empty());
              for y in rect.top..rect.bottom
              {
                  for x in rect.left..rect.right
                  {
                      pixels[y as usize * 640 + x as usize] += 1;
                  }
              }
          });
        for y in 0..480
        {
            for x in 0..640
            {
                let point = Point { h: x as i16,
                                    v: y as i16 };
                let exposed = bounds.contains(point)
                              && !wm.back_to_front().any(|window| {
                                                        window.structure_pieces()
                                                              .iter()
                                                              .any(|rect| rect.contains(point))
                                                    });
                assert_eq!(pixels[y * 640 + x], u8::from(exposed), "({x},{y})");
            }
        }
        let mut empty = 0;
        wm.for_each_desktop_rect(Rect::ZERO, |_| empty += 1);
        assert_eq!(empty, 0);
        let wm = WindowManager::new();
        let mut fragments = alloc::vec::Vec::new();
        wm.for_each_desktop_rect(bounds, |rect| fragments.push(rect));
        assert_eq!(fragments, [bounds], "uncovered content is visited exactly once");
    }

    #[test]
    fn perimeter_tracking_matches_full_screen_union_reference()
    {
        let shapes = [&[Rect::new(12, 25, 80, 70)][..],
                      &[Rect::new(12, 25, 70, 60), Rect::new(30, 40, 80, 70)][..],
                      &[Rect::new(12, 25, 40, 70), Rect::new(40, 25, 80, 70)][..],
                      &[Rect::new(12, 25, 13, 70), Rect::new(20, 30, 80, 31)][..],
                      &[Rect::new(12, 25, 13, 26), Rect::new(13, 26, 14, 27)][..],
                      &[Rect::new(12, 25, 80, 70), Rect::new(12, 25, 80, 70)][..],
                      &[Rect::new(12, 25, 80, 70), Rect::new(30, 35, 40, 45)][..],
                      &[Rect::new(12, 25, 13, 26), Rect::new(12, 25, 13, 26)][..],
                      &[Rect::ZERO, Rect::new(12, 25, 80, 70)][..],
                      &[Rect::new(12, 25, 80, 26), Rect::new(12, 26, 80, 27)][..]];
        for pieces in shapes
        {
            for (dx, dy) in
                [(-100, 0), (-40, -35), (-5, -8), (0, 0), (5, 3), (50, 40), (100, 0), (0, 100)]
            {
                let mut actual = vec![0xffffff; 96 * 80];
                let mut expected = actual.clone();
                let contains = |x: i16, y: i16| {
                    pieces.iter().any(|piece| {
                                     piece.contains(Point { h: x - dx,
                                                            v: y - dy })
                                 })
                };
                for y in 21..80
                {
                    for x in 0..96
                    {
                        if (x + y) & 1 == 1
                           && contains(x, y)
                           && (!contains(x - 1, y)
                               || !contains(x + 1, y)
                               || !contains(x, y - 1)
                               || !contains(x, y + 1))
                        {
                            expected[y as usize * 96 + x as usize] = 0;
                        }
                    }
                }
                let mut visits = 0;
                Outline::new(pieces, i32::from(dx), i32::from(dy)).unwrap()
                    .visit(Rect::new(0,0,96,80), |x,y| {
                        assert_eq!(actual[y * 96 + x], 0xffffff, "duplicate XOR visit");
                        actual[y * 96 + x] = 0;
                        visits += 1;
                    });
                assert_eq!(visits, expected.iter().filter(|&&p| p == 0).count());
                assert_eq!(actual, expected, "translated structure {pieces:?} by {dx},{dy}");
            }
        }
    }

    #[test]
    fn supplied_grow_tracking_scratch_matches_owned_mask()
    {
        let (mut wm, _, window, _) = make();
        let bounds = wm.window(window).unwrap().bounds;
        let mut preview = bounds;
        preview.offset(20, 10);
        let mut scratch = vec![0x123456; 640 * 480];
        for tracking in [Tracking::Grow { window,
                                          start: Point::default(),
                                          bounds,
                                          preview }]
        {
            wm.tracking = Some(tracking);
            let mut owned = vec![0x878787; 640 * 480];
            let mut supplied = owned.clone();
            let paint = |window: &Window, canvas: &mut Canvas<'_>| {
                canvas.paint_rect(window.local_bounds(), 0xffffff);
            };
            wm.draw(&mut Canvas::new(&mut owned, 640, 480), paint);
            wm.draw_with_scratch(&mut Canvas::new(&mut supplied, 640, 480),
                                 Some(&mut scratch),
                                 paint);
            assert_eq!(owned, supplied);
        }
    }

    #[test]
    fn cropped_tracking_scratch_is_pixel_identical_to_the_full_surface()
    {
        let (mut wm, _, window, _) = make();
        let bounds = wm.window(window).unwrap().bounds;
        let mut preview = bounds;
        preview.offset(-35, 11);
        for height in [21, 80, 480]
        {
            for tracking in [Tracking::Grow { window,
                                              start: Point::default(),
                                              bounds,
                                              preview }]
            {
                wm.tracking = Some(tracking);
                let initial: alloc::vec::Vec<_> =
                    (0..640 * height).map(|i| (i as u32 * 31) & 0xffffff).collect();
                let mut full = initial.clone();
                let mut cropped = initial.clone();
                wm.draw_tracking_at(&mut Canvas::new(&mut full, 640, height), None, 0);
                let mut scratch = vec![0x123456; 640 * (height - 21)];
                wm.draw_tracking(&mut Canvas::new(&mut cropped, 640, height), Some(&mut scratch));
                assert_eq!(cropped, full, "tracking changed at height {height}");
                assert_eq!(cropped[..640 * 21],
                           initial[..640 * 21],
                           "tracking painted the menu band");
            }
        }
    }

    #[test]
    fn definition_disposal_refusal_is_retained_and_retry_never_aliases_the_closed_window()
    {
        use core::sync::atomic::{AtomicBool, Ordering};
        static ALLOW_DISPOSE: AtomicBool = AtomicBool::new(false);
        unsafe extern "C" fn definition(call: *mut DefinitionCall) -> i64
        {
            // This fixture receives the manager's live, synchronous WDEF call;
            // the object is its retained WindowDefinitionState until completion.
            let call = unsafe { &mut *call };
            if call.message == definition::DISPOSE
            {
                let state = unsafe { &mut *call.object.cast::<WindowDefinitionState>() };
                if !ALLOW_DISPOSE.load(Ordering::SeqCst)
                {
                    state.close_pressed = 0xcafe;
                    return BAD_ARGUMENT as i64;
                }
                if state.close_pressed != 0xcafe
                {
                    return BAD_ARGUMENT as i64;
                }
            }
            unsafe { crate::test_wdef::standard_window_definition(call) }
        }
        ALLOW_DISPOSE.store(false, Ordering::SeqCst);
        let mut manager = WindowManager::new();
        let mut events = EventManager::new();
        let owner = manager.test_owner(7);
        let stale = manager.new_window_with_definition(Rect::new(80, 80, 320, 240),
                                                       "Refusing definition",
                                                       7,
                                                       (100, 60),
                                                       0,
                                                       definition,
                                                       &mut events,
                                                       0)
                           .unwrap();
        let error = manager.dispose_window(stale, &mut events, 1).unwrap_err();
        assert!(matches!(error, CloseError::Cleanup(crate::toolbox::handles::cleanup::Cause::WindowDefinitionDisposal { window, reply, .. }) if window == stale.0 && reply == BAD_ARGUMENT as i64));
        let detail = alloc::format!("{error}");
        assert!(detail.contains("WDEF 0")
                && detail.contains("retry")
                && detail.contains(&alloc::format!("{stale}")));
        assert!(manager.window(stale).is_none());
        assert_eq!(manager.count(), 0);
        assert_eq!((owner.summary().live, owner.summary().pending, owner.summary().quarantined),
                   (1, 1, 1));
        let fresh = manager.new_window(Rect::new(80, 80, 320, 240),
                                       "Healthy window",
                                       7,
                                       (100, 60),
                                       &mut events,
                                       2)
                           .unwrap();
        assert_ne!(fresh, stale);
        assert_eq!(manager.active_window(), Some(fresh));
        ALLOW_DISPOSE.store(true, Ordering::SeqCst);
        let registry = manager.test_registry.take().unwrap();
        let report = mac_object_table::walk::retry(&mut manager,
                                                   &owner,
                                                   |_, key| registry.dispatch(key.kind()),
                                                   |_, error| panic!("{error}")).unwrap();
        assert_eq!((report.released, report.quarantined), (1, 0));
        assert_eq!((owner.summary().pending, owner.summary().quarantined), (0, 0));
        assert!(manager.window(stale).is_none());
        assert_eq!(manager.active_window(), Some(fresh));
        manager.dispose_window(fresh, &mut events, 3).unwrap();
        let report = mac_object_table::walk::drain(&mut manager,
                                                   &owner,
                                                   |_, key| registry.dispatch(key.kind()),
                                                   |_, error| panic!("{error}")).unwrap();
        assert_eq!(report.released, 1, "backing remains registered until the generic walk");
        assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
    }

    #[test]
    fn initialization_refusal_rolls_back_without_publishing_geometry_or_activation()
    {
        use core::sync::atomic::{AtomicUsize, Ordering};
        static DISPOSES: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn definition(call: *mut DefinitionCall) -> i64
        {
            // The fixture receives the manager's synchronous initialized call.
            match unsafe { (*call).message }
            {
                definition::CALCULATE => BAD_ARGUMENT as i64,
                definition::DISPOSE =>
                {
                    DISPOSES.fetch_add(1, Ordering::SeqCst);
                    0
                }
                _ =>
                unsafe { crate::test_wdef::standard_window_definition(call) }
            }
        }
        DISPOSES.store(0, Ordering::SeqCst);
        let mut manager = WindowManager::new();
        let owner = manager.test_owner(9);
        let mut events = EventManager::new();
        let error = manager.new_window_with_activation(Rect::new(80, 80, 320, 240),
                                                       "Refused creation",
                                                       &owner,
                                                       (100, 60),
                                                       0,
                                                       definition,
                                                       &mut events,
                                                       0,
                                                       true)
                           .unwrap_err();
        assert!(matches!(error, CreationError::Definition { operation: definition::CALCULATE, reply, .. } if reply == BAD_ARGUMENT as i64));
        assert!(alloc::format!("{error}").contains("WDEF"));
        assert_eq!(DISPOSES.load(Ordering::SeqCst), 1);
        assert_eq!(manager.count(), 0);
        assert!(manager.active_window().is_none());
        assert!(events.next_matching(u16::MAX).is_none());
        assert_eq!((owner.summary().live, owner.summary().pending),
                   (1, 0),
                   "only the registered backing remains");
        let fresh = manager.new_window(Rect::new(80, 80, 320, 240),
                                       "Usable after refusal",
                                       9,
                                       (100, 60),
                                       &mut events,
                                       1)
                           .unwrap();
        assert_eq!(manager.active_window(), Some(fresh));
    }

    #[test]
    fn plane_changes_damage_only_newly_visible_content_and_focus_does_not_damage_content()
    {
        let (mut manager, mut events, a, b) = make();
        let unrelated = manager.new_window(Rect::new(530, 80, 630, 200),
                                           "Unrelated",
                                           0,
                                           (50, 50),
                                           &mut events,
                                           0)
                               .unwrap();
        for id in [a, b, unrelated]
        {
            manager.validate(id).unwrap();
        }
        manager.deactivate_all(&mut events, 1);
        assert!(manager.next_update(&events, 1).is_none(),
                "chrome-only focus changes do not ask applications to repaint");
        manager.select_window(unrelated, &mut events, 2);
        assert!(manager.next_update(&events, 2).is_none());
        manager.select_window(a, &mut events, 3);
        assert!(manager.window(a).unwrap().damage.is_some());
        assert!(manager.window(b).unwrap().damage.is_none());
        assert!(manager.window(unrelated).unwrap().damage.is_none());
        manager.validate(a).unwrap();
        manager.select_window(a, &mut events, 4);
        assert!(manager.next_update(&events, 4).is_none(),
                "selecting the selected window is a no-op");
        manager.set_hidden(a, true);
        assert!(manager.window(b).unwrap().damage.is_some());
        assert!(manager.window(unrelated).unwrap().damage.is_none());
        manager.validate(b).unwrap();
        manager.set_hidden(a, true);
        assert!(manager.next_update(&events, 5).is_none());
        manager.set_hidden(a, false);
        assert!(manager.window(a).unwrap().damage.is_some());
        assert!(manager.window(b).unwrap().damage.is_none());
        manager.validate(a).unwrap();
        manager.dispose_window(a, &mut events, 6).unwrap();
        assert!(manager.window(b).unwrap().damage.is_some());
        assert!(manager.window(unrelated).unwrap().damage.is_none());
    }

    fn make() -> (WindowManager, EventManager, WindowId, WindowId)
    {
        let mut wm = WindowManager::new();
        let mut events = EventManager::new();
        let a =
            wm.new_window(Rect::new(20, 80, 320, 300), "A", 0, (160, 100), &mut events, 0).unwrap();
        let b = wm.new_window(Rect::new(220, 150, 520, 370), "B", 0, (160, 100), &mut events, 0)
                  .unwrap();
        (wm, events, a, b)
    }
    #[test]
    fn desktop_focus_deactivates_without_reordering_and_front_can_reactivate()
    {
        let (mut manager, mut events, a, b) = make();
        while events.next_matching(u16::MAX).is_some()
        {}
        manager.deactivate_all(&mut events, 1);
        assert_eq!(manager.front_window(), Some(b));
        assert_eq!(manager.active_window(), None);
        assert_eq!(manager.window(a).unwrap().state.active, 0);
        assert_eq!(manager.window(b).unwrap().state.active, 0);
        let event = events.next_matching(u16::MAX).unwrap();
        assert_eq!(event.kind, EventKind::Activate);
        assert_eq!(event.window, b.0);
        assert_eq!(event.modifiers & ACTIVE_FLAG, 0);
        assert!(events.next_matching(u16::MAX).is_none());
        manager.deactivate_all(&mut events, 2);
        assert!(events.next_matching(u16::MAX).is_none());
        manager.select_window(b, &mut events, 3);
        assert_eq!(manager.active_window(), Some(b));
        let event = events.next_matching(u16::MAX).unwrap();
        assert_eq!(event.window, b.0);
        assert_ne!(event.modifiers & ACTIVE_FLAG, 0);
        assert!(events.next_matching(u16::MAX).is_none());
        manager.deactivate_all(&mut events, 4);
        assert!(manager.dispose_window(b, &mut events, 5).is_ok());
        assert_eq!(manager.front_window(), Some(a));
        assert_eq!(manager.active_window(), None);
        assert_eq!(manager.window(a).unwrap().state.active, 0);
        manager.select_window(a, &mut events, 6);
        assert_eq!(manager.active_window(), Some(a));
    }

    #[test]
    fn raising_the_already_active_window_does_not_repeat_activation()
    {
        let (mut manager, mut events, _, active) = make();
        let owner = manager.test_owner(0);
        manager.new_window_with_activation(Rect::new(80, 80, 320, 240),
                                           "No focus steal",
                                           &owner,
                                           (100, 60),
                                           0,
                                           crate::test_wdef::standard_window_definition,
                                           &mut events,
                                           0,
                                           false)
               .unwrap();
        for id in manager.order.clone()
        {
            manager.validate(id).unwrap();
        }
        while events.next_matching(u16::MAX).is_some()
        {}
        manager.select_window(active, &mut events, 1);
        assert!(events.next_matching(u16::MAX).is_none(), "plane motion did not change focus");
        assert!(manager.window(active).unwrap().damage.is_some(),
                "newly exposed content still needs update");
        assert_eq!(manager.window(active).unwrap().state.active, 1);
    }

    #[test]
    fn front_hit_testing_activation_and_stale_ids()
    {
        let (mut wm, mut events, a, b) = make();
        assert_eq!(wm.find_window(Point { h: 250,
                                          v: 180 })
                     .window,
                   Some(b));
        wm.select_window(a, &mut events, 1);
        assert_eq!(wm.find_window(Point { h: 250,
                                          v: 180 })
                     .window,
                   Some(a));
        wm.dispose_window(a, &mut events, 2).unwrap();
        assert_eq!(wm.front_window(), Some(b));
        let c =
            wm.new_window(Rect::new(20, 80, 320, 300), "C", 0, (160, 100), &mut events, 3).unwrap();
        assert_ne!(a, c);
        assert!(wm.window(a).is_none());
    }
    #[test]
    fn drag_preview_does_not_move_or_invalidate_until_release_and_can_cancel()
    {
        let (mut wm, mut events, a, b) = make();
        for id in [a, b]
        {
            wm.begin_update(id);
            wm.end_update(id);
        }
        let original = wm.window(b).unwrap().bounds;
        let start = Point { h: 300,
                            v: 135 };
        wm.start_tracking(wm.find_window(start), start);
        let moved = Point { h: 340,
                            v: 170 };
        assert_eq!(wm.track(moved, false, &mut events, 1), None);
        assert_eq!(wm.window(b).unwrap().bounds, original);
        assert!(wm.next_update(&events, 1).is_none());
        assert_eq!(wm.track(Point { h: 0,
                                    v: 0 },
                            true,
                            &mut events,
                            2),
                   None);
        assert_eq!(wm.window(b).unwrap().bounds, original);
        assert!(wm.next_update(&events, 2).is_none());
        wm.start_tracking(wm.find_window(start), start);
        assert_eq!(wm.track(moved, true, &mut events, 3), Some(Action::Drag(b)));
        let mut expected = original;
        expected.offset(40, 35);
        assert_eq!(wm.window(b).unwrap().bounds, expected);
        assert!(wm.window(b).unwrap().damage.is_none(),
                "moving a fully visible front window must preserve its local content");
        assert!(wm.next_update(&events, 3).is_some());
    }

    #[test]
    fn begin_end_update_preserves_new_damage()
    {
        let (mut wm, events, _, b) = make();
        assert_eq!(wm.next_update(&events, 0).unwrap().window, b.0);
        assert!(wm.begin_update(b).is_some());
        wm.invalidate(b, Rect::new(0, 0, 5, 5));
        wm.end_update(b);
        assert_eq!(wm.begin_update(b), Some(Rect::new(0, 0, 5, 5)));
        wm.end_update(b);
        assert_eq!(wm.begin_update(b), None);
    }
    #[test]
    fn resource_identity_and_custom_handler_control_geometry_and_hit()
    {
        unsafe extern "C" fn custom(pointer: *mut DefinitionCall) -> i64
        {
            let call = unsafe { &mut *pointer };
            assert_eq!(call.id, 3);
            assert_eq!(call.variation, 2);
            let state = unsafe { &mut *call.object.cast::<WindowDefinitionState>() };
            match call.message
            {
                definition::NEW | definition::CALCULATE =>
                {
                    state.structure = state.content;
                    state.structure.inset(-3, -3);
                    0
                }
                definition::HIT => 2,
                _ => 0
            }
        }
        let mut wm = WindowManager::new();
        let mut events = EventManager::new();
        let id = wm.new_window_with_definition(Rect::new(40, 80, 320, 300),
                                               "CUSTOM",
                                               0,
                                               (160, 100),
                                               3 * 16 + 2,
                                               custom,
                                               &mut events,
                                               0)
                   .unwrap();
        assert_eq!(wm.window(id).unwrap().structure(), Rect::new(37, 77, 323, 303));
        assert_eq!(wm.window(id).unwrap().procedure(), 50);
        assert_eq!(wm.find_window(Point { h: 100,
                                          v: 100 })
                     .part,
                   WindowPart::Drag);
    }

    #[test]
    fn visible_regions_exclude_front_structure_and_exposure_posts_update()
    {
        let (mut wm, mut events, a, b) = make();
        wm.begin_update(a);
        wm.end_update(a);
        wm.begin_update(b);
        wm.end_update(b);
        let front = wm.window(b).unwrap().structure();
        let bounds = wm.window(a).unwrap().bounds;
        let mut area = 0i32;
        wm.for_each_visible_rect(a, |rect| {
              let mut overlap = Rect::ZERO;
              assert!(!sect_rect(rect, front, &mut overlap));
              area += i32::from(rect.right - rect.left) * i32::from(rect.bottom - rect.top);
          });
        let mut overlap = Rect::ZERO;
        sect_rect(bounds, front, &mut overlap);
        assert_eq!(area,
                   i32::from(bounds.right - bounds.left) * i32::from(bounds.bottom - bounds.top)
                   - i32::from(overlap.right - overlap.left)
                     * i32::from(overlap.bottom - overlap.top));
        assert!(wm.next_update(&events, 0).is_none());
        wm.dispose_window(b, &mut events, 1).unwrap();
        assert_eq!(wm.next_update(&events, 1).unwrap().window, a.0);
    }

    #[test]
    fn zoom_click_can_cancel_restore_and_tracks_manual_resize_as_user_state()
    {
        let mut wm = WindowManager::new();
        let mut events = EventManager::new();
        let original = Rect::new(40, 80, 360, 300);
        let id = wm.new_window_with_definition(original,
                                               "ZOOM",
                                               0,
                                               (160, 100),
                                               8,
                                               crate::test_wdef::standard_window_definition,
                                               &mut events,
                                               0)
                   .unwrap();
        let p = Point { h: 345,
                        v: 70 };
        assert_eq!(wm.find_window(p).part, WindowPart::ZoomOut);
        wm.start_tracking(wm.find_window(p), p);
        wm.track(Point { h: 0,
                         v: 0 },
                 true,
                 &mut events,
                 1);
        assert_eq!(wm.window(id).unwrap().bounds, original);
        let standard = Rect::new(40, 80, 240, 210);
        assert_eq!(wm.set_standard_bounds(id, Some(Rect::ZERO)), Err(BAD_ARGUMENT));
        wm.set_standard_bounds(id, Some(standard)).unwrap();
        assert_eq!(wm.window(id).unwrap().bounds, original, "proposal must not resize or paint");
        wm.start_tracking(wm.find_window(p), p);
        assert_eq!(wm.track(p, true, &mut events, 2), Some(Action::Resize(id)));
        let zoomed = wm.window(id).unwrap().bounds;
        assert_eq!(zoomed, standard);
        let p = Point { h: zoomed.right - 14,
                        v: zoomed.top - 9 };
        assert_eq!(wm.find_window(p).part, WindowPart::ZoomIn);
        wm.start_tracking(wm.find_window(p), p);
        wm.track(p, true, &mut events, 3);
        assert_eq!(wm.window(id).unwrap().bounds, original);
        assert!(wm.window(id).unwrap().user_bounds.is_none());
    }
    #[test]
    fn clear_shadow_corners_show_back_content_and_hit_back_window()
    {
        let mut wm = WindowManager::new();
        let mut events = EventManager::new();
        let back =
            wm.new_window(Rect::new(50, 70, 500, 400), "BACK", 0, (100, 100), &mut events, 0)
              .unwrap();
        let front =
            wm.new_window(Rect::new(100, 150, 350, 300), "FRONT", 0, (100, 100), &mut events, 0)
              .unwrap();
        let rect = wm.window(front).unwrap().structure();
        let corners = [Point { h: rect.right - 1,
                               v: rect.top },
                       Point { h: rect.left,
                               v: rect.bottom - 1 }];
        for point in corners
        {
            assert_eq!(wm.find_window(point).window, Some(back));
            let mut visible = false;
            wm.for_each_visible_rect(back, |piece| visible |= piece.contains(point));
            assert!(visible);
        }
        let mut pixels = [0x808080; 640 * 480];
        let mut canvas = Canvas::new(&mut pixels, 640, 480);
        wm.draw(&mut canvas, |window, canvas| {
              canvas.paint_rect(window.local_bounds(),
                                if window.id == back { 0x123456 } else { 0xabcdef })
          });
        for point in corners
        {
            assert_eq!(pixels[point.v as usize * 640 + point.h as usize], 0x123456);
        }
    }
}

#[cfg(test)]
impl super::lifetime::WindowHost for WindowManager
{
    fn release_window(&mut self, ticket: mac_object_table::domain::ReleasedResource)
                      -> Result<(), Failure>
    {
        self.release(ticket).map(|_| ())
    }

    fn release_window_storage(&mut self, ticket: mac_object_table::domain::ReleasedResource)
                              -> Result<(), Failure>
    {
        self.release_storage(ticket)
    }
}
