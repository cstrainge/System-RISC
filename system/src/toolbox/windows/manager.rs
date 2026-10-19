//! Rectangular document windows with stable IDs, z order, damage and tracking.
//! Rust APIs implement a documented subset; no WindowRecord guest ABI is implied.
#![cfg_attr(not(test), forbid(unsafe_code))]
use alloc::vec;

use mac_abi::{DefinitionCall, DefinitionEntry, Header, Surface};

use super::definition::{self, WindowDefinitionState};
use crate::toolbox::{events::*,
                     quickdraw::{Canvas, Point, Rect, SurfaceExt,
                                 geometry::{sect_rect, union_rect}}};

pub const MAX_WINDOWS: usize = 32;
pub use mac_abi::WindowId;
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
    pub bounds:          Rect,
    pub refcon:          u32,
    pub min_width:       i16,
    pub min_height:      i16,
    state:               WindowDefinitionState,
    procedure:           i16,
    entry:               DefinitionEntry,
    gate:                mac_abi::TrapGate,
    user_bounds:         Option<Rect>,
    grow_icon_clip:      Option<Rect>,
    damage:              Option<Rect>,
    hidden:              bool,
    pub(crate) updating: bool
}
impl Window
{
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
            crate::kernel::with_resource_owner(u64::from(self.refcon), || {
                crate::toolbox::resources::definitions::invoke(self.entry, &mut call)
            })
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

/// Original initial indexed-device presentation. Unknown RGB32 colors use native
/// direct-color inversion; the named reference profile has all 256 CLUT entries.
fn indexed_tracking_xor(rgb: u32) -> u32
{
    const TABLE: &[u8] = include_bytes!("../../../assets/windows/system7-indexed-xor.bin");
    let entries = TABLE.as_chunks::<8>().0;
    entries.binary_search_by_key(&rgb, |entry| u32::from_be_bytes(entry[..4].try_into().unwrap()))
           .map_or(rgb ^ 0xffffff, |index| {
               u32::from_be_bytes(entries[index][4..].try_into().unwrap())
           })
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
    Close(WindowId)
}

pub struct WindowManager
{
    windows:  [Option<Window>; MAX_WINDOWS],
    order:    [WindowId; MAX_WINDOWS],
    len:      usize,
    next_id:  u32,
    active:   Option<WindowId>,
    gate:     mac_abi::TrapGate,
    tracking: Option<Tracking>
}
impl Default for WindowManager
{
    fn default() -> Self
    {
        Self::new()
    }
}
impl WindowManager
{
    pub fn with_gate(gate: mac_abi::TrapGate) -> Self
    {
        Self { gate,
               ..Self::new() }
    }

    pub fn new() -> Self
    {
        Self { windows:  [None; MAX_WINDOWS],
               order:    [WindowId(0); MAX_WINDOWS],
               len:      0,
               next_id:  1,
               active:   None,
               gate:     crate::toolbox::quickdraw::surface_gate,
               tracking: None }
    }

    pub fn count(&self) -> usize
    {
        self.len
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
        self.windows.iter().flatten().find(|w| w.id == id)
    }

    fn window_mut(&mut self, id: WindowId) -> Option<&mut Window>
    {
        self.windows.iter_mut().flatten().find(|w| w.id == id)
    }

    pub fn back_to_front(&self) -> impl Iterator<Item = &Window>
    {
        self.order[..self.len].iter().filter_map(|id| self.window(*id)).filter(|w| !w.hidden)
    }

    #[cfg(test)]
    pub fn new_window(&mut self, bounds: Rect, title: &str, refcon: u32, minimum: (i16, i16),
                      events: &mut EventManager, when: u32)
                      -> Option<WindowId>
    {
        self.new_window_with_definition(bounds,
                                        title,
                                        refcon,
                                        minimum,
                                        0,
                                        crate::test_wdef::standard_window_definition,
                                        events,
                                        when)
    }

    /// The entry must satisfy the native WDEF ABI for this window's lifetime.
    #[allow(clippy::too_many_arguments)]
    pub fn new_window_with_definition(&mut self, bounds: Rect, title: &str, refcon: u32,
                                      minimum: (i16, i16), procedure: i16, entry: DefinitionEntry,
                                      events: &mut EventManager, when: u32)
                                      -> Option<WindowId>
    {
        self.new_window_with_activation(bounds, title, refcon, minimum, procedure, entry, events,
                                        when, true)
    }

    /// Background application windows enter the z order without transient focus.
    #[allow(clippy::too_many_arguments)]
    pub fn new_window_with_activation(&mut self, bounds: Rect, title: &str, refcon: u32,
                                      minimum: (i16, i16), procedure: i16, entry: DefinitionEntry,
                                      events: &mut EventManager, when: u32, activate: bool)
                                      -> Option<WindowId>
    {
        let (min_width, min_height) = minimum;
        if min_width < 1 || min_height < 1
        {
            return None;
        }
        if bounds.is_empty()
           || bounds.left < 1
           || bounds.top < 25
           || bounds.right > 638
           || bounds.bottom > 478
           || bounds.right - bounds.left < min_width
           || bounds.bottom - bounds.top < min_height
        {
            return None;
        }
        // Onscreen windows cannot collide with the reserved desktop or worlds.
        if self.next_id >= mac_abi::quickdraw::DESKTOP_TARGET
        {
            return None;
        }
        let slot = self.windows.iter().position(Option::is_none)?;
        let previous = self.active;
        let id = WindowId(self.next_id);
        self.next_id = self.next_id.checked_add(1)?;
        let mut window = Window { id,
                                  bounds,
                                  refcon,
                                  min_width,
                                  min_height,
                                  state: WindowDefinitionState::new(bounds, title),
                                  procedure,
                                  entry,
                                  gate: self.gate,
                                  user_bounds: None,
                                  grow_icon_clip: None,
                                  damage: Some(Rect::new(0,
                                                         0,
                                                         bounds.right - bounds.left,
                                                         bounds.bottom - bounds.top)),
                                  hidden: false,
                                  updating: false };
        if window.call(definition::NEW, 0, core::ptr::null_mut()) < 0 || !window.calculate()
        {
            return None;
        }
        let structure = window.structure();
        if structure.is_empty()
           || structure.left < 0
           || structure.top < 25
           || structure.right > 640
           || structure.bottom > 480
        {
            window.call(definition::DISPOSE, 0, core::ptr::null_mut());
            return None;
        }
        self.windows[slot] = Some(window);
        self.order[self.len] = id;
        self.len += 1;
        if activate
        {
            self.active = Some(id);
            self.refresh_active();
            Self::activation(previous, false, events, when);
            Self::activation(Some(id), true, events, when);
        }
        self.invalidate_all();
        Some(id)
    }

    fn refresh_active(&mut self)
    {
        let active = self.active;
        for window in self.windows.iter_mut().flatten()
        {
            window.state.active = u32::from(active == Some(window.id));
        }
    }

    fn activation(id: Option<WindowId>, active: bool, events: &mut EventManager, when: u32)
    {
        if let Some(id) = id
        {
            events.post_system(Event { kind: EventKind::Activate,
                                       message: id.0,
                                       when,
                                       where_: events.mouse,
                                       modifiers: (events.modifiers & !ACTIVE_FLAG)
                                                  | if active { ACTIVE_FLAG } else { 0 } });
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
        self.order.copy_within(index + 1..self.len, index);
        self.order[self.len - 1] = id;
        self.active = Some(id);
        self.refresh_active();
        Self::activation(previous, false, events, when);
        Self::activation(Some(id), true, events, when);
        self.invalidate_all();
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

    fn order_group(&mut self, ids: &[WindowId], events: &mut EventManager, when: u32,
                   focus: GroupFocus)
    {
        let mut order = [WindowId(0); MAX_WINDOWS];
        let mut count = 0;
        for selected in [false, true]
        {
            for id in self.order[..self.len].iter().copied()
            {
                if ids.contains(&id) == selected
                {
                    order[count] = id;
                    count += 1;
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
            return;
        }
        self.order = order;
        self.active = active;
        self.refresh_active();
        if previous != active
        {
            Self::activation(previous, false, events, when);
            Self::activation(active, true, events, when);
        }
        self.invalidate_all();
    }

    pub fn deactivate_all(&mut self, events: &mut EventManager, when: u32)
    {
        let Some(previous) = self.active.take()
        else
        {
            return;
        };
        self.refresh_active();
        self.tracking = None;
        Self::activation(Some(previous), false, events, when);
        self.invalidate_all();
    }

    pub fn dispose_window(&mut self, id: WindowId, events: &mut EventManager, when: u32) -> bool
    {
        self.dispose_with_focus(id, events, when, true)
    }

    /// Process teardown chooses its surviving application's final focus after
    /// all departing windows are gone, rather than activating each intermediate.
    #[cfg(target_arch = "riscv64")]
    pub(crate) fn dispose_without_activation(&mut self, id: WindowId, events: &mut EventManager,
                                             when: u32)
                                             -> bool
    {
        self.dispose_with_focus(id, events, when, false)
    }

    fn dispose_with_focus(&mut self, id: WindowId, events: &mut EventManager, when: u32,
                          select_next: bool)
                          -> bool
    {
        let Some(index) = self.order[..self.len].iter().position(|other| *other == id)
        else
        {
            return false;
        };
        let active = self.active == Some(id);
        if let Some(window) = self.window_mut(id)
        {
            window.call(definition::DISPOSE, 0, core::ptr::null_mut());
        }
        self.windows.iter_mut().find(|w| w.as_ref().is_some_and(|w| w.id == id)).unwrap().take();
        self.order.copy_within(index + 1..self.len, index);
        self.len -= 1;
        if active
        {
            self.active = if select_next { self.front_window() } else { None };
        }
        self.refresh_active();
        if active && select_next
        {
            Self::activation(self.front_window(), true, events, when);
        }
        self.tracking = None;
        self.invalidate_all();
        true
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
        window.hidden = hidden;
        self.invalidate_all();
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

    pub fn invalidate_all(&mut self)
    {
        for window in self.windows.iter_mut().flatten()
        {
            window.damage = Some(window.local_bounds());
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
                     message: id.0,
                     when,
                     where_: events.mouse,
                     modifiers: events.modifiers })
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
                     message: id.0,
                     when,
                     where_: events.mouse,
                     modifiers: events.modifiers })
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

    pub fn end_update(&mut self, id: WindowId)
    {
        if let Some(window) = self.window_mut(id)
        {
            window.updating = false;
        }
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
            let window = self.window(*id).unwrap();
            if window.hidden
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
        let tracking = self.tracking?;
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
                        w.bounds = Rect::new(w.bounds.left - structure.left,
                                             25 + w.bounds.top - structure.top,
                                             640 - (structure.right - w.bounds.right),
                                             459 - (structure.bottom - w.bounds.bottom));
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
                let preview = Rect::new(0, 21, 640, 480).contains(point).then(|| {
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
                                                       i32::from(640
                                                                 - (structure.right - w.bounds.right)
                                                                 - bounds.left).max(i32::from(w.min_width)))
                            as i16;
                let height = (i32::from(bounds.bottom - bounds.top) + i32::from(point.v)
                              - i32::from(start.v)).clamp(i32::from(w.min_height),
                                                         i32::from(480
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
                    self.dispose_window(window, events, when);
                    action = Some(Action::Close(window));
                }
            }
        }
        if released
        {
            self.tracking = None;
        }
        if action.is_some()
        {
            self.invalidate_all();
        }
        action
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
        self.draw_tracking(c);
    }

    /// Tracking overlays never mutate window geometry, retained content or damage.
    /// Grow geometry belongs to the loaded definition, not to the manager.
    fn draw_tracking(&self, canvas: &mut Canvas)
    {
        let Some(tracking @ (Tracking::Grow { .. }
                 | Tracking::Drag { preview: Some(_),
                                   .. })) = self.tracking
        else
        {
            return;
        };
        let mut mask = vec![0xffffff; canvas.width * canvas.height];
        match tracking
        {
            Tracking::Grow { window,
                             preview,
                             .. } =>
            {
                let Some(mut window) = self.window(window).copied()
                else
                {
                    return;
                };
                let mut target = Canvas::new(&mut mask, canvas.width, canvas.height);
                let mut surface = Surface::from_canvas(&mut target);
                if window.call(definition::GROW, (&preview as *const Rect) as i64, &mut surface) < 0
                {
                    return;
                }
            }
            Tracking::Drag { window,
                             preview: Some(preview),
                             bounds,
                             .. } =>
            {
                let Some(window) = self.window(window)
                else
                {
                    return;
                };
                let dx = preview.left - bounds.left;
                let dy = preview.top - bounds.top;
                let contains = |x: i16, y: i16| {
                    window.structure_pieces().iter().any(|r| {
                                                        r.contains(Point { h: x - dx,
                                                                           v: y - dy })
                                                    })
                };
                for y in 21..canvas.height as i16
                {
                    for x in 0..canvas.width as i16
                    {
                        if contains(x, y)
                           && (!contains(x - 1, y)
                               || !contains(x + 1, y)
                               || !contains(x, y - 1)
                               || !contains(x, y + 1))
                        {
                            mask[y as usize * canvas.width + x as usize] = 0;
                        }
                    }
                }
            }
            _ => return
        }
        // Classic gray pen pattern, XORed over the composed desktop. Rebuilding
        // the retained scene erases old outlines without damaging covered ports.
        for y in 21..canvas.height
        {
            for x in 0..canvas.width
            {
                let index = y * canvas.width + x;
                if mask[index] != 0xffffff && (x + y) & 1 == 1
                {
                    canvas.pixels[index] = indexed_tracking_xor(canvas.pixels[index]);
                }
            }
        }
    }

    /// Enumerate non-overlapping global content rectangles after subtracting
    /// higher windows' structure regions. Rectangular regions are the first cut.
    pub fn for_each_visible_rect(&self, id: WindowId, mut visit: impl FnMut(Rect))
    {
        const CAPACITY: usize = MAX_WINDOWS * MAX_WINDOWS * 4;
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
        let mut pieces = [Rect::ZERO; CAPACITY];
        pieces[0] = window.bounds;
        let mut count = 1;
        for front in &self.order[index + 1..self.len]
        {
            if self.window(*front).is_none_or(|window| window.hidden)
            {
                continue;
            }
            for cover in self.window(*front).unwrap().structure_pieces()
            {
                let before = count;
                for i in (0..before).rev()
                {
                    let old = pieces[i];
                    let mut cut = Rect::ZERO;
                    if !sect_rect(old, *cover, &mut cut)
                    {
                        continue;
                    }
                    count -= 1;
                    pieces[i] = pieces[count];
                    for fragment in [Rect::new(old.left, old.top, old.right, cut.top),
                                     Rect::new(old.left, cut.bottom, old.right, old.bottom),
                                     Rect::new(old.left, cut.top, cut.left, cut.bottom),
                                     Rect::new(cut.right, cut.top, old.right, cut.bottom)]
                    {
                        if !fragment.is_empty()
                        {
                            pieces[count] = fragment;
                            count += 1;
                        }
                    }
                }
            }
        }

        for rect in &pieces[..count]
        {
            visit(*rect);
        }
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
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
        assert_eq!(event.message, b.0);
        assert_eq!(event.modifiers & ACTIVE_FLAG, 0);
        assert!(events.next_matching(u16::MAX).is_none());
        manager.deactivate_all(&mut events, 2);
        assert!(events.next_matching(u16::MAX).is_none());
        manager.select_window(b, &mut events, 3);
        assert_eq!(manager.active_window(), Some(b));
        let event = events.next_matching(u16::MAX).unwrap();
        assert_eq!(event.message, b.0);
        assert_ne!(event.modifiers & ACTIVE_FLAG, 0);
        assert!(events.next_matching(u16::MAX).is_none());
        manager.deactivate_all(&mut events, 4);
        assert!(manager.dispose_window(b, &mut events, 5));
        assert_eq!(manager.front_window(), Some(a));
        assert_eq!(manager.active_window(), None);
        assert_eq!(manager.window(a).unwrap().state.active, 0);
        manager.select_window(a, &mut events, 6);
        assert_eq!(manager.active_window(), Some(a));
    }

    #[test]
    fn window_ids_cannot_enter_desktop_or_offscreen_namespaces()
    {
        let mut manager = WindowManager::new();
        let mut events = EventManager::new();
        manager.next_id = mac_abi::quickdraw::DESKTOP_TARGET;
        assert!(manager.new_window(Rect::new(80, 80, 280, 200),
                                   "Reserved",
                                   0,
                                   (80, 60),
                                   &mut events,
                                   0)
                       .is_none());
        assert_eq!(manager.count(), 0);
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
        wm.dispose_window(a, &mut events, 2);
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
        assert!(wm.next_update(&events, 3).is_some());
    }

    #[test]
    fn begin_end_update_preserves_new_damage()
    {
        let (mut wm, events, _, b) = make();
        assert_eq!(wm.next_update(&events, 0).unwrap().message, b.0);
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
        wm.dispose_window(b, &mut events, 1);
        assert_eq!(wm.next_update(&events, 1).unwrap().message, a.0);
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
        wm.start_tracking(wm.find_window(p), p);
        assert_eq!(wm.track(p, true, &mut events, 2), Some(Action::Resize(id)));
        let zoomed = wm.window(id).unwrap().bounds;
        assert_ne!(zoomed, original);
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
