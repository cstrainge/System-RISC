//! Rectangular document windows with stable IDs, z order, damage and tracking.
//! Rust APIs implement a documented subset; no WindowRecord guest ABI is implied.
#![cfg_attr(not(test), forbid(unsafe_code))]
use alloc::{collections::VecDeque, vec::Vec};
use core::cell::{Cell, RefCell};

pub use mac_abi::WindowId;
use mac_abi::{BAD_ARGUMENT, DefinitionCall, Header, NOT_FOUND, Status, Surface};
use mac_object_table::shared::InvocationGate as _;

use super::definition::{self, WindowDefinitionState};
use crate::toolbox::{events::*,
                     handles::{InvocationGate, KindLease, OwnerRef, Store, cleanup::Failure},
                     quickdraw::{Canvas, Point, Rect, SurfaceExt,
                                 geometry::{sect_rect, union_rect},
                                 presentation::Outline,
                                 region::{BooleanOp, Region, RegionError}},
                     resources::definitions::{BoundDefinition, DefinitionRef}};
#[cfg(systemrisc)]
#[path = "admission.rs"]
mod admission;
#[cfg(systemrisc)]
#[path = "operations.rs"]
pub(crate) mod operations;
#[cfg(systemrisc)]
#[path = "birth.rs"]
pub(crate) mod birth;
#[cfg(systemrisc)]
#[path = "paint.rs"]
pub(crate) mod paint;
#[cfg(systemrisc)]
#[path = "canvas.rs"]
pub(crate) mod canvas;
#[cfg(systemrisc)]
#[path = "trusted.rs"]
pub(crate) mod trusted;
#[cfg(systemrisc)]
#[path = "ack.rs"]
pub(crate) mod ack;
#[cfg(systemrisc)]
pub(crate) use admission::{PreparedWindow, WindowCommit, WindowPlan};
/// First content scan line below the menu bar.
const MENU_BAR_BOTTOM: i16 = 21;
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
    Toolbox,
    Preserve
}

/// Logical event ownership and retained resource source are different roles.
#[cfg(systemrisc)]
#[derive(Clone, Copy)]
enum WindowSource
{
    Application,
    System
    {
        resources: crate::kernel::SystemResources,
        recovery:  bool
    }
}

#[cfg(systemrisc)]
impl WindowSource
{
    fn with<R>(self, owner: u64, operation: impl FnOnce() -> R) -> Result<R, Status>
    {
        match self
        {
            Self::Application =>
            {
                crate::context::without_recovery(|| {
                    crate::kernel::with_resource_owner(owner, operation)
                })
            }
            Self::System { resources,
                           recovery: true } => resources.with(operation),
            Self::System { resources,
                           recovery: false } =>
            {
                crate::context::without_recovery(|| resources.with(operation))
            }
        }
    }
}

/// Only the manager's original-ticket lookup creates this authority. A wire
/// DISPOSE message, copied WindowId or stopped source cannot construct it.
pub(crate) struct DefinitionDisposal<'a>
{
    binding: &'a BoundDefinition,
    _ticket: &'a mac_object_table::domain::ReleasedResource
}
impl DefinitionDisposal<'_>
{
    pub(crate) fn entry(&self) -> DefinitionRef
    {
        self.binding.reference()
    }
}

/// The canonical WIND payload owns this dependency; temporary Copy views do
/// not. Failed Dispose keeps only its ABI-mutated state for the same ticket.
struct OwnedWindow
{
    window:            Window,
    disposal_state:    Cell<Option<WindowDefinitionState>>,
    disposal_complete: Cell<bool>,
    binding:           BoundDefinition,
    revision:          Option<u64>,
    content_revision:  Option<u64>,
    #[cfg(systemrisc)]
    cleanup_job:       Option<alloc::sync::Arc<operations::WindowJob>>,
    #[cfg(systemrisc)]
    paint:             Option<alloc::sync::Arc<paint::PaintOwner>>,
    #[cfg(systemrisc)]
    trusted_close:     Option<trusted::TrustedCloseHandle>
}
/// Same-exclusion outline publication; preparation changes no Window state.
/// The original full-key payload stays borrowed through retained CTRL commit.
#[cfg(systemrisc)]
pub(crate) struct ControlOutlinePermit<'a>
{
    outline:      &'a mut Option<Rect>,
    revision:     &'a mut Option<u64>,
    hit_revision: &'a mut Option<u64>,
    next:         Option<Rect>,
    revisions:    Option<(u64, u64)>
}
#[cfg(systemrisc)]
impl ControlOutlinePermit<'_>
{
    pub(crate) fn commit(self) -> bool
    {
        let Some((revision, hit_revision)) = self.revisions
        else
        {
            return false;
        };
        *self.outline = self.next;
        *self.revision = Some(revision);
        *self.hit_revision = Some(hit_revision);
        true
    }
}
impl OwnedWindow
{
    #[allow(clippy::too_many_arguments)]
    fn new(bounds: Rect, title: &str, owner: u64, minimum: (i16, i16), procedure: i16,
           gate: mac_abi::TrapGate, binding: BoundDefinition, stay_in_back: bool,
           #[cfg(systemrisc)] source: WindowSource)
           -> Self
    {
        Self { window: Window { id: WindowId(0),
                                initialized: false,
                                user_initialization: UserInitialization::None,
                                bounds,
                                owner,
                                #[cfg(systemrisc)]
                                source,
                                min_width: minimum.0,
                                min_height: minimum.1,
                                state: WindowDefinitionState::new(bounds, title),
                                procedure,
                                entry: binding.reference(),
                                gate,
                                user_bounds: None,
                                standard_bounds: None,
                                grow_icon_clip: None,
                                content_outline: None,
                                control_outline: None,
                                close_deferred: false,
                                damage: Some(Rect::new(0,
                                                       0,
                                                       bounds.right - bounds.left,
                                                       bounds.bottom - bounds.top)),
                                hidden: false,
                                stay_in_back,
                                updating: false },
               disposal_state: Cell::new(None),
               disposal_complete: Cell::new(false),
               binding,
               revision: Some(1),
               content_revision: Some(1),
               #[cfg(systemrisc)]
               cleanup_job: None,
               #[cfg(systemrisc)]
               paint: None,
               #[cfg(systemrisc)]
               trusted_close: None }
    }
}
impl core::ops::Deref for OwnedWindow
{
    type Target = Window;

    fn deref(&self) -> &Window
    {
        &self.window
    }
}
impl core::ops::DerefMut for OwnedWindow
{
    fn deref_mut(&mut self) -> &mut Window
    {
        &mut self.window
    }
}

#[derive(Clone, Copy)]
pub struct Window
{
    pub id:              WindowId,
    initialized:         bool,
    user_initialization: UserInitialization,
    pub bounds:          Rect,
    pub owner:           u64,
    #[cfg(systemrisc)]
    source:              WindowSource,
    pub min_width:       i16,
    pub min_height:      i16,
    state:               WindowDefinitionState,
    procedure:           i16,
    entry:               DefinitionRef,
    gate:                mac_abi::TrapGate,
    user_bounds:         Option<Rect>,
    standard_bounds:     Option<Rect>,
    grow_icon_clip:      Option<Rect>,
    content_outline:     Option<Rect>,
    control_outline:     Option<Rect>,
    close_deferred:      bool,
    damage:              Option<Rect>,
    hidden:              bool,
    stay_in_back:        bool,
    pub(crate) updating: bool
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum UserInitialization
{
    None,
    New,
    Ready
}
impl Window
{
    /// Everything a painted frame depends on. A plan is stale exactly when one of
    /// these differs from the live window; the manager's mutation counter also
    /// advances for accesses that change none of them, including the paint's own
    /// admission and the job admission that stamps its revision.
    pub(crate) fn draws_same_frame_as(&self, other: &Self) -> bool
    {
        let a = self.state;
        let b = other.state;
        // Identity and registration.
        self.id == other.id
        && self.owner == other.owner
        && self.initialized == other.initialized
        && self.user_initialization == other.user_initialization
        && self.entry == other.entry
        // Definition and frame geometry: the procedure decides how the frame is
        // drawn, and every rect here is an input to the crop the plan takes.
        && self.procedure == other.procedure
        && self.gate == other.gate
        && self.bounds == other.bounds
        && self.min_width == other.min_width
        && self.min_height == other.min_height
        && self.user_bounds == other.user_bounds
        && self.standard_bounds == other.standard_bounds
        && self.grow_icon_clip == other.grow_icon_clip
        && self.content_outline == other.content_outline
        && self.control_outline == other.control_outline
        // Definition-visible frame state.
        && a.active == b.active
        && a.close_pressed == b.close_pressed
        && a.zoomed == b.zoomed
        && a.zoom_pressed == b.zoom_pressed
        && a.content == b.content
        && a.structure == b.structure
        && a.close_box == b.close_box
        && a.grow_box == b.grow_box
        && a.zoom_box == b.zoom_box
        && a.structure_piece_count == b.structure_piece_count
        && a.structure_pieces == b.structure_pieces
        && a.title_length == b.title_length
        && a.title == b.title
        // Update and visibility state govern whether and what a frame may paint.
        && self.hidden == other.hidden
        && self.updating == other.updating
        && self.close_deferred == other.close_deferred
        && self.stay_in_back == other.stay_in_back
        && self.damage == other.damage
        // `source` names where the window's resources came from; a change means
        // the definition behind this frame was replaced.
        && self.same_source(other)
    }

    /// `source` names where the window's resources came from; a change means the
    /// definition behind this frame was replaced.
    fn same_source(&self, _other: &Self) -> bool
    {
        #[cfg(systemrisc)]
        {
            use WindowSource::{Application, System};
            match (&self.source, &_other.source)
            {
                (Application, Application) => true,
                (System { resources: a,
                          recovery: ra },
                 System { resources: b,
                          recovery: rb }) => a.same_source(*b) && ra == rb,
                _ => false
            }
        }
        #[cfg(not(systemrisc))]
        {
            true
        }
    }

    /// Classification only; this does not authorize any caller or object ID.
    #[cfg(systemrisc)]
    pub(crate) fn uses_system_resources(&self) -> bool
    {
        matches!(self.source, WindowSource::System { .. })
    }

    /// Scope the source captured from the genuine storage owner at creation.
    /// Ordinary replay cannot inherit a surrounding modal placement permit.
    #[cfg(systemrisc)]
    pub(crate) fn with_resources<R>(&self, operation: impl FnOnce() -> R) -> Result<R, Status>
    {
        self.source.with(self.owner, operation)
    }

    /// Hidden windows cannot be restored as a modal's active window.
    pub(crate) fn is_hidden(&self) -> bool
    {
        self.hidden
    }

    pub fn zoomed(&self) -> bool
    {
        self.state.zoomed != 0
    }

    /// Fixed at creation. Such a window stays below every ordinary window and
    /// is never promoted by activation or group reordering.
    pub fn stays_in_back(&self) -> bool
    {
        self.stay_in_back
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
        self.control_outline.or(self.content_outline)
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

    pub(crate) fn zoom_box(&self) -> Rect
    {
        self.state.zoom_box
    }

    pub(crate) fn frame_rects(&self) -> [Rect; 4]
    {
        let structure = self.structure();
        let content = self.bounds;
        [Rect::new(structure.left, structure.top, structure.right, content.top),
         Rect::new(structure.left, content.bottom, structure.right, structure.bottom),
         Rect::new(structure.left, content.top, content.left, content.bottom),
         Rect::new(content.right, content.top, structure.right, content.bottom)]
    }

    /// Definition-owned content adornments are above retained application
    /// drawing. Preserve the compositor's damage clip, including grow-strip
    /// restrictions supplied by applications that do not use both scrollbars.
    #[cfg_attr(not(systemrisc), allow(dead_code))]
    pub(crate) fn paint_content_overlay(&self, canvas: &mut Canvas<'_>) -> Result<(), Status>
    {
        let origin = canvas.origin();
        let previous = canvas.clip();
        let mut clip = previous;
        if let Some(mut grow) = self.grow_icon_clip
        {
            grow.offset(self.bounds.left, self.bounds.top);
            if !sect_rect(grow, previous, &mut clip)
            {
                return Ok(());
            }
        }
        canvas.set_origin(0, 0);
        canvas.set_clip(clip);
        let mut copy = *self;
        let mut surface = Surface::from_canvas(canvas);
        let reply = copy.call(definition::DRAW_GROW_ICON, 0, &mut surface, None);
        canvas.set_clip(previous);
        canvas.set_origin(origin.h, origin.v);
        if reply < 0 { Err(i32::try_from(reply).unwrap_or(mac_abi::BAD_ABI)) } else { Ok(()) }
    }

    fn call(&mut self, message: i16, param: i64, surface: *mut Surface,
            disposal: Option<DefinitionDisposal<'_>>)
            -> i64
    {
        let mut call = DefinitionCall { properties: core::ptr::null(),
                                        properties_length: 0,
                                        reflow: 0,
                                        header: Header::for_type::<DefinitionCall>(),
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
        let entry = self.entry;
        let invoke = || {
            match disposal
            {
                Some(permit) =>
                {
                    crate::toolbox::resources::definitions::invoke_disposal(permit, &mut call)
                }
                None => crate::toolbox::resources::definitions::invoke(entry, &mut call)
            }
        };
        // The manager owns state, and a supplied entry must implement this native ABI.
        #[cfg(systemrisc)]
        {
            // Copy source metadata before invoking the ABI: holding &Window
            // across a call which mutates its definition state would alias it.
            let source = self.source;
            let owner = self.owner;
            source.with(owner, invoke).unwrap_or_else(i64::from)
        }
        #[cfg(not(systemrisc))]
        {
            invoke()
        }
    }

    fn calculate(&mut self) -> bool
    {
        self.state.content = self.bounds;
        self.call(definition::CALCULATE, 0, core::ptr::null_mut(), None) >= 0
    }

    pub fn global_to_local(&self, point: Point) -> Point
    {
        Point { h: point.h - self.bounds.left,
                v: point.v - self.bounds.top }
    }
}

// Shared native geometry producers. User plans stage the same policy before
// author execution; trusted definitions keep their original synchronous tail.
fn drag_geometry(bounds: Rect, start: Point, point: Point, screen: Rect) -> Option<Rect>
{
    Rect::new(0, 21, screen.right, screen.bottom).contains(point).then(|| {
                    let dx = i32::from(point.h) - i32::from(start.h);
                    let dy = i32::from(point.v) - i32::from(start.v);
                    Rect::new((i32::from(bounds.left) + dx) as i16,
                              (i32::from(bounds.top) + dy) as i16,
                              (i32::from(bounds.right) + dx) as i16,
                              (i32::from(bounds.bottom) + dy) as i16)
                })
}
fn grow_geometry(w: &Window, bounds: Rect, start: Point, point: Point, screen: Rect) -> Rect
{
    let structure = w.structure();
    let width = (i32::from(bounds.right - bounds.left) + i32::from(point.h)
                             - i32::from(start.h)).clamp(i32::from(w.min_width),
                                                       i32::from(screen.right
                                                                 - (structure.right - w.bounds.right)
                                                                 - bounds.left).max(i32::from(w.min_width)))
                            as i16;
    let height = (i32::from(bounds.bottom - bounds.top) + i32::from(point.v)
                              - i32::from(start.v)).clamp(i32::from(w.min_height),
                                                         i32::from(screen.bottom
                                                                   - (structure.bottom - w.bounds.bottom)
                                                                   - bounds.top).max(i32::from(w.min_height)))
                             as i16;
    Rect::new(bounds.left, bounds.top, bounds.left + width, bounds.top + height)
}
fn zoom_geometry(w: &mut Window, screen: Rect)
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
        w.bounds = w.standard_bounds.unwrap_or_else(|| {
                                        Rect::new(w.bounds.left - structure.left,
                                                  25 + w.bounds.top - structure.top,
                                                  screen.right - (structure.right - w.bounds.right),
                                                  (screen.bottom - 21)
                                                  - (structure.bottom - w.bounds.bottom))
                                    });
        w.state.zoomed = 1;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
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
/// Read-only tracking pixels/dependencies. This is presentation metadata, not
/// a Window mutation capability or a new input identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TrackingVisual
{
    pub(crate) tracking: bool,
    pub(crate) outline:  Option<Outline>,
    pub(crate) chrome:   Option<TrackingChrome>
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrackingChrome
{
    Grow
    {
        window: WindowId, structure: Rect, preview: Rect
    },
    Close
    {
        window: WindowId, structure: Rect
    },
    Zoom
    {
        window: WindowId, structure: Rect
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
    RecoveryBusy,
    RecoveryAuthority,
    CapacityChanged,
    Publication(mac_object_table::store::PreparedError),
    Facility(mac_object_table::domain::PoolError),
    Binding(Status),
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
            Self::RecoveryBusy =>
            {
                f.write_str("the recovery dialog already retains its main window and auxiliary \
                             sheet; finish their cleanup before opening another")
            }
            Self::RecoveryAuthority =>
            {
                f.write_str("System modal window creation requires its genuine storage owner, \
                             recipient and admitted I/O root")
            }
            Self::CapacityChanged =>
            {
                f.write_str("window metadata capacity changed; prepare current capacity and retry \
                             the original admission")
            }
            Self::Publication(cause) => cause.fmt(f),
            Self::Facility(error) => error.fmt(f),
            Self::Binding(status) =>
            {
                write!(f, "WDEF original-source binding refused: status {status}")
            }
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
        self.manager
            .windows
            .get_mut(self.id.0)
            .map(|owned| &mut owned.window)
            .ok_or(CreationError::Stopping)
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
            #[cfg(systemrisc)]
            serial!("WINDOW_INITIALIZATION_ROLLBACK_REFUSED: {cause}");
            #[cfg(not(systemrisc))]
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

#[cfg(systemrisc)]
struct RecoveryWindow
{
    id:       WindowId,
    activity: crate::recovery::Activity,
    retry: Option<(mac_object_table::domain::ReleasedResource,
                   crate::toolbox::handles::cleanup::Cause)>,
    disposed: bool
}

/// Synchronous compositor scopes captured before mutably traversing geometry.
/// This pins at most the two real service guards once per traversal under the
/// manager's existing admission. Use with its synchronously borrowed Window;
/// this grants source/placement selection, not arbitrary object mutation.
#[cfg(systemrisc)]
pub(crate) struct WindowScopes
{
    recovery: [Option<(WindowId, crate::recovery::Activity)>; 2]
}
#[cfg(systemrisc)]
impl WindowScopes
{
    pub(crate) fn with_window<R>(&self, window: &Window, operation: impl FnOnce() -> R)
                                 -> Result<R, Status>
    {
        match window.source
        {
            WindowSource::System { resources,
                                   recovery: true } =>
            {
                let (_, activity) = self.recovery
                                        .iter()
                                        .flatten()
                                        .find(|(id, _)| *id == window.id)
                                        .ok_or(NOT_FOUND)?;
                if window.owner != activity.owner()
                   || !resources.same_source(activity.resource_source()?)
                {
                    return Err(mac_abi::ACCESS_DENIED);
                }
                // Existing Activity selection has bounded owned-capability work
                // once per replay scope; no source Arc is cloned per pixel.
                activity.run(operation)
            }
            _ => window.with_resources(operation)
        }
    }
}

pub struct WindowManager
{
    // Desktop admission already excludes reentrant/mutating WM requests while
    // drawing. Reborrow cached sweep storage without adding an interrupt lock
    // around definition drawing callbacks. The manager remains Send behind the
    // resident service lock; callers never share it without that admission.
    visibility:            RefCell<super::visibility::Scratch>,
    selection_damage:      Vec<(WindowId, Region)>,
    selection_redraw:      bool,
    // A single bounded memo, never an authority or a payload pin. Successful
    // window-list changes invalidate it; cached IDs still validate on lookup.
    front_cache:           Cell<Option<(u64, Option<WindowId>)>>,
    windows:               Store<OwnedWindow>,
    /// Main-screen bounds that interactive drag, grow and zoom stay within.
    screen:                Rect,
    hit_revision:          Option<u64>,
    #[cfg(systemrisc)]
    removals:              VecDeque<operations::RemovalDebt>,
    #[cfg(systemrisc)]
    removal_readers:       alloc::sync::Arc<operations::RemovalReaders>,
    #[cfg(systemrisc)]
    work_notice: alloc::sync::Arc<crate::toolbox::resources::definitions::callbacks::Notice>,
    retained_failures:     VecDeque<mac_object_table::domain::CompletionError>,
    // One guaranteed modal service has one main window and one auxiliary
    // sheet. Keep their guards outside the Copy geometry payload, keyed by
    // their canonical full window IDs, including while invalidated/quarantined.
    #[cfg(systemrisc)]
    recovery_windows:      [Option<RecoveryWindow>; 2],
    #[cfg(systemrisc)]
    recovery_publications:
        Option<[mac_object_table::store::PublicationReservation<InvocationGate>; 2]>,
    #[cfg(systemrisc)]
    recovery_closes:       Vec<alloc::sync::Arc<trusted::TrustedCloseHeader>>,
    order:                 Vec<WindowId>,
    len:                   usize,
    active:                Option<WindowId>,
    order_scratch:         Vec<WindowId>,
    geometry_order:        Vec<WindowId>,
    gate:                  mac_abi::TrapGate,
    tracking_revision:     Option<u64>,
    tracking:              Option<Tracking>
}
impl WindowManager
{
    #[cfg(systemrisc)]
    pub(crate) fn recovery_window_capacity(&self) -> usize
    {
        self.recovery_windows.len()
    }

    #[cfg(systemrisc)]
    pub(crate) fn resource_scopes(&self) -> WindowScopes
    {
        WindowScopes { recovery: core::array::from_fn(|index| {
                           self.recovery_windows[index].as_ref()
                                                       .map(|held| (held.id, held.activity.clone()))
                       }) }
    }

    pub(crate) fn with_kinds(kinds: (KindLease, KindLease), gate: mac_abi::TrapGate)
                             -> Result<Self, Status>
    {
        #[cfg(systemrisc)]
        let work_notice = crate::toolbox::resources::definitions::callbacks::Notice::new()?;
        Ok(Self { visibility: RefCell::new(Default::default()),
                  selection_damage: Vec::new(),
                  selection_redraw: false,
                  front_cache: Cell::new(None),
                  windows: Store::owned_prepared(kinds.0, kinds.1).map_err(|cause| {
                      crate::toolbox::handles::registry_status(&mac_object_table::registry::Error::Pool(cause))
                  })?,
                  retained_failures: VecDeque::new(),
                  hit_revision: Some(1),
                  #[cfg(systemrisc)]
                  removals: VecDeque::new(),
                  #[cfg(systemrisc)]
                  removal_readers: alloc::sync::Arc::try_new(operations::RemovalReaders::new(work_notice.clone()))
                      .map_err(|_| mac_abi::NO_MEMORY)?,
                  #[cfg(systemrisc)]
                  work_notice,
                  #[cfg(systemrisc)]
                  recovery_windows: core::array::from_fn(|_| None),
                  #[cfg(systemrisc)]
                  recovery_publications: None,
                  #[cfg(systemrisc)]
                  recovery_closes: Vec::new(),
                  screen: Rect::new(0, 0, crate::display::width() as i16, crate::display::height() as i16),
                  order: Vec::new(),
                  len: 0,
                  active: None,
                  order_scratch: Vec::new(),
                  geometry_order: Vec::new(),
                  gate,
                  tracking_revision: Some(1),
                  tracking: None })
    }


    pub(crate) fn owner(&self, id: WindowId) -> Option<u64>
    {
        self.windows.get(id.0).map(|window| window.owner)
    }

    /// Read-only metadata follows the authenticated logical recipient. Mutating
    /// a System service window still requires its private Activity capability.
    pub(crate) fn readable_by(&self, id: WindowId, recipient: u64) -> bool
    {
        let Some((storage, window)) = self.windows.get_with_owner(id.0)
        else
        {
            return false;
        };
        if !window.initialized || window.owner != recipient
        {
            return false;
        }
        if storage.kind() != mac_object_table::domain::DomainKind::System
        {
            return storage.incarnation() == recipient;
        }
        #[cfg(systemrisc)]
        {
            return self.recovery_windows
                       .iter()
                       .flatten()
                       .any(|held| held.id == id && held.activity.owner() == recipient);
        }
        #[cfg(not(systemrisc))]
        false
    }

    pub(crate) fn front_readable(&self, recipient: u64) -> Option<WindowId>
    {
        #[cfg(systemrisc)]
        let service_window =
            self.recovery_windows.iter().flatten().any(|held| held.activity.owner() == recipient);
        #[cfg(not(systemrisc))]
        let service_window = false;
        if !service_window
        {
            return self.front_for_owner(recipient);
        }
        self.order[..self.len].iter().rev().copied().find(|id| {
                                                        self.readable_by(*id, recipient)
                                                        && self.window(*id)
                                                               .is_some_and(|window| !window.hidden)
                                                    })
    }

    pub(crate) fn owned_by(&self, id: WindowId, recipient: u64) -> bool
    {
        let Some((storage, window)) = self.windows.get_with_owner(id.0)
        else
        {
            return false;
        };
        if storage.kind() != mac_object_table::domain::DomainKind::System
        {
            return storage.incarnation() == recipient && window.owner == recipient;
        }
        #[cfg(systemrisc)]
        if let Some(activity) = crate::context::recovery_activity()
        {
            return window.owner == activity.owner()
                   && self.recovery_windows.iter().flatten().any(|held| {
                                                                held.id == id
                                                                && held.activity
                                                                       .same_session(&activity)
                                                            });
        }
        false
    }

    #[cfg(systemrisc)]
    pub(crate) fn prepare_recovery(&mut self, storage: &OwnerRef) -> Result<(), Status>
    {
        if storage.owner().kind() != mac_object_table::domain::DomainKind::System
           || self.recovery_publications.is_some()
        {
            return Err(mac_abi::BAD_ARGUMENT);
        }
        let tokens = [self.windows.reserve_publication(storage).map_err(|_| mac_abi::NO_MEMORY)?,
                      self.windows.reserve_publication(storage).map_err(|_| mac_abi::NO_MEMORY)?];
        let source = crate::kernel::system_resources(storage)?;
        let mut closes = Vec::new();
        closes.try_reserve_exact(tokens.len()).map_err(|_| mac_abi::NO_MEMORY)?;
        for _ in 0..tokens.len()
        {
            closes.push(trusted::TrustedCloseHeader::prepare_system(source,
                                                                    self.work_notice.clone())?);
        }
        let required = self.len.checked_add(tokens.len()).ok_or(mac_abi::NO_MEMORY)?;
        self.visibility.get_mut().prepare(required).map_err(|_| mac_abi::NO_MEMORY)?;
        self.order.try_reserve(tokens.len()).map_err(|_| mac_abi::NO_MEMORY)?;
        self.order_scratch.try_reserve(required).map_err(|_| mac_abi::NO_MEMORY)?;
        self.geometry_order.try_reserve(required).map_err(|_| mac_abi::NO_MEMORY)?;
        self.retained_failures.try_reserve(required).map_err(|_| mac_abi::NO_MEMORY)?;
        self.removals
            .try_reserve(self.removal_capacity_required(tokens.len())
                             .map_err(|_| mac_abi::NO_MEMORY)?)
            .map_err(|_| mac_abi::NO_MEMORY)?;
        self.recovery_closes = closes;
        self.recovery_publications = Some(tokens);
        Ok(())
    }

    pub(crate) fn recovery_spare(&self) -> usize
    {
        #[cfg(systemrisc)]
        if let Some(tokens) = &self.recovery_publications
        {
            // A releasing endpoint has already left geometry/order, but its
            // reserved capacity must survive until ACK and subsequent reuse.
            return tokens.len();
        }
        0
    }

    #[cfg(systemrisc)]
    fn finish_recovery_acknowledgements(&mut self)
    {
        let Some(tokens) = &self.recovery_publications
        else
        {
            return;
        };
        for index in 0..self.recovery_windows.len()
        {
            if self.recovery_windows[index].as_ref().is_some_and(|held| held.disposed)
               && tokens[index].ready()
            {
                if let Some(held) = self.recovery_windows[index].take()
                {
                    held.activity.window_closed(held.id);
                    // This checkpoint follows actual payload destruction and
                    // canonical ACK, not an absent ordinary-ID lookup.
                    crate::toolbox::processes::finder::report_cleanup_progress();
                    drop(held);
                }
            }
        }
    }

    /// Run at a real cleanup/progress checkpoint, never as a pointer-motion
    /// retry loop. The unique ticket stays here if its WDEF still refuses.
    #[cfg(systemrisc)]
    pub(crate) fn retry_recovery_cleanup(&mut self)
    {
        use mac_object_table::registry::CleanupFailure;
        for index in 0..self.recovery_windows.len()
        {
            let pending = self.recovery_windows[index].as_mut().and_then(|held| {
                                                                   held.retry.take().map(|retry| {
                                                                              (held.activity
                                                                                   .clone(),
                                                                               retry)
                                                                          })
                                                               });
            let Some((activity, (ticket, previous))) = pending
            else
            {
                continue;
            };
            // Scope entry is fallible. Keep the unique ticket outside the
            // callback until entry actually succeeds; refusing the scope must
            // not drop a captured-but-never-invoked cleanup capability.
            let mut offered = Some(ticket);
            match activity.run(|| offered.take().map(|ticket| self.release(ticket)))
            {
                Ok(Some(Ok(_))) => (),
                Ok(Some(Err(failure))) =>
                {
                    let (ticket, cause) = failure.into_parts();
                    if cause != previous
                    {
                        serial!("RECOVERY_WINDOW_CLEANUP_RETAINED: {cause}");
                    }
                    if let Some(held) = &mut self.recovery_windows[index]
                    {
                        held.retry = Some((ticket, cause));
                    }
                    else
                    {
                        // The manager cannot lose the actual capability even
                        // if its bookkeeping is unexpectedly unavailable.
                        self.quarantine_or_retain(ticket);
                    }
                }
                Err(status) =>
                {
                    if let Some(ticket) = offered.take()
                    {
                        if let Some(held) = &mut self.recovery_windows[index]
                        {
                            held.retry = Some((ticket, previous));
                        }
                        else
                        {
                            self.quarantine_or_retain(ticket);
                        }
                    }
                    serial!("RECOVERY_WINDOW_CONTEXT_RETAINED status={status}");
                }
                Ok(None) =>
                {
                    // No success/ACK is manufactured when the staged offer is
                    // absent. Keep the actual window/activity barrier retained.
                    serial!("RECOVERY_WINDOW_CLEANUP_RETAINED: no staged offer");
                }
            }
        }
        self.finish_recovery_acknowledgements();
    }

    #[cfg(systemrisc)]
    pub(crate) fn recovery_close_pending(&mut self, id: WindowId,
                                         activity: &crate::recovery::Activity)
                                         -> Result<bool, Status>
    {
        self.retry_recovery_cleanup();
        if let Some(held) = self.recovery_windows.iter().flatten().find(|held| held.id == id)
        {
            if !held.activity.same_session(activity)
            {
                return Err(mac_abi::ACCESS_DENIED);
            }
            return Ok(true);
        }
        if activity.window_close_acknowledged(id) { Ok(false) } else { Err(NOT_FOUND) }
    }

    pub(crate) fn releasing_id(&self, ticket: &mac_object_table::domain::ReleasedResource)
                               -> Option<WindowId>
    {
        self.windows.releasing(ticket).map(|window| window.id)
    }

    pub(crate) fn releasing_uses_process_definition(&self,
                                                    ticket: &mac_object_table::domain::ReleasedResource)
                                                    -> Option<bool>
    {
        self.windows.releasing(ticket).map(|window| window.binding.is_process_source())
    }

    fn quarantine_or_retain(&mut self, ticket: mac_object_table::domain::ReleasedResource)
    {
        if let Err(error) = self.windows.quarantine(ticket)
        {
            #[cfg(systemrisc)]
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
        let preparing = self.windows.publication_preparations(ticket.key().owner());
        if preparing != 0
        {
            return Err(Failure::publication_preparations(ticket, preparing));
        }
        self.windows.release_storage(ticket).map_err(Failure::from)
    }

    fn retire_window_ui(&mut self, window: &Window)
    {
        let id = window.id;
        if let Some(index) = self.order.iter().position(|other| *other == id)
        {
            self.front_cache.set(None);
            self.hit_revision = self.hit_revision.and_then(|revision| revision.checked_add(1));
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
    }

    pub(crate) fn release(&mut self, ticket: mac_object_table::domain::ReleasedResource)
                          -> Result<WindowId, Failure>
    {
        use mac_object_table::registry::CleanupFailure;
        let Some(window) = self.windows.releasing(&ticket).map(|owned| owned.window)
        else
        {
            return Err(Failure::registration_mismatch(ticket));
        };
        let id = window.id;
        #[cfg(systemrisc)]
        {
            if let Some(owned) = self.windows.releasing(&ticket)
               && (owned.initialized
                   || owned.cleanup_job.is_some()
                   || owned.trusted_close.is_some())
            {
                match owned.canvas_guardian().and_then(|guardian| guardian.close())
                {
                    Ok(()) => (),
                    Err(mac_abi::SERVICE_BUSY) =>
                    {
                        return Err(Failure::window_paint_pending(ticket));
                    }
                    Err(status) =>
                    {
                        return Err(Failure::window_definition(ticket,
                                                              id.0,
                                                              window.procedure,
                                                              i64::from(status)));
                    }
                }
            }
            match crate::toolbox::resources::definitions::is_user(window.entry)
            {
                Ok(true) =>
                {
                    if !window.initialized && window.user_initialization == UserInitialization::None
                    {
                        return self.abort_uninitialized_user(ticket);
                    }
                    return self.defer_user_release(ticket);
                }
                Err(status) =>
                {
                    return Err(Failure::window_definition(ticket,
                                                          id.0,
                                                          window.procedure,
                                                          i64::from(status)));
                }
                Ok(false) => ()
            }
            if self.removals.len() == self.removals.capacity()
            {
                return Err(Failure::window_retirement_storage(ticket,
                                                              self.removals.len(),
                                                              self.removals.capacity()));
            }
        }
        #[cfg(systemrisc)]
        if self.windows.releasing(&ticket).is_some_and(|owned| owned.trusted_close.is_some())
        {
            return self.defer_trusted_release(ticket);
        }
        self.release_trusted_now(ticket, #[cfg(systemrisc)]
                                 None)
    }

    fn release_trusted_now(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                           #[cfg(systemrisc)] trusted: Option<trusted::TrustedCloseHandle>)
                           -> Result<WindowId, Failure>
    {
        use mac_object_table::registry::CleanupFailure;
        let Some(window) = self.windows.releasing(&ticket).map(|owned| owned.window)
        else
        {
            return Err(Failure::registration_mismatch(ticket));
        };
        let id = window.id;
        self.retire_window_ui(&window);
        let Some(owned) = self.windows.releasing(&ticket)
        else
        {
            return Err(Failure::registration_mismatch(ticket));
        };
        if !owned.binding.matches(owned.window.entry)
        {
            return Err(Failure::window_definition(ticket,
                                                  id.0,
                                                  window.procedure,
                                                  mac_abi::BAD_ABI as i64));
        }
        let mut disposal = owned.window;
        if let Some(state) = owned.disposal_state.get()
        {
            disposal.state = state;
        }
        let permit = DefinitionDisposal { binding: &owned.binding,
                                          _ticket: &ticket };
        let reply = if owned.disposal_complete.get()
        {
            0
        }
        else
        {
            disposal.call(definition::DISPOSE, 0, core::ptr::null_mut(), Some(permit))
        };
        // The ABI only mutates WindowDefinitionState. Retain those actual
        // effects before returning a refusal; retry uses the same original
        // canonical object/binding/ticket, with no mutable non-Copy extraction.
        owned.disposal_state.set(Some(disposal.state));
        if reply < 0
        {
            return Err(Failure::window_definition(ticket, id.0, window.procedure, reply));
        }
        owned.disposal_complete.set(true);
        #[cfg(systemrisc)]
        {
            let removed = self.windows.release(ticket).map_err(Failure::from)?;
            let debt = match trusted
            {
                Some(handle) =>
                {
                    operations::RemovalDebt::completed_trusted(removed,
                                                               self.removal_readers.clone(),
                                                               handle)
                }
                None =>
                {
                    operations::RemovalDebt::completed(removed, self.removal_readers.clone(), None)
                }
            };
            self.removals.push_back(debt);
            self.work_notice.request();
        }
        #[cfg(not(systemrisc))]
        drop(self.windows.release(ticket).map_err(Failure::from)?);
        #[cfg(systemrisc)]
        {
            if let Some(held) =
                self.recovery_windows.iter_mut().flatten().find(|held| held.id == id)
            {
                held.disposed = true;
            }
            self.finish_recovery_acknowledgements();
        }
        Ok(id)
    }

    pub fn count(&self) -> usize
    {
        self.len
    }

    pub(crate) fn window_for_owner(&self, id: WindowId, owner: u64) -> Option<&Window>
    {
        let window = self.windows.get(id.0)?;
        (window.initialized && self.owned_by(id, owner)).then_some(&window.window)
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

    /// Actual committed-content mutation only (including direct caret XOR).
    /// Callback completion/replay and unrelated input must not increment it.
    pub(crate) fn content_changed(&mut self, id: WindowId) -> Result<(), Status>
    {
        let owned = self.windows.get_mut(id.0).ok_or(NOT_FOUND)?;
        owned.content_revision =
            owned.content_revision.and_then(|revision| revision.checked_add(1));
        owned.content_revision.map(|_| ()).ok_or(mac_abi::BAD_ABI)
    }

    pub(crate) fn content_revision(&self, id: WindowId) -> Result<u64, Status>
    {
        self.windows
            .get(id.0)
            .filter(|owned| owned.initialized)
            .ok_or(NOT_FOUND)?
            .content_revision
            .ok_or(mac_abi::BAD_ABI)
    }

    pub fn window(&self, id: WindowId) -> Option<&Window>
    {
        self.windows.get(id.0).filter(|window| window.initialized).map(|owned| &owned.window)
    }

    fn window_mut(&mut self, id: WindowId) -> Option<&mut Window>
    {
        self.hit_revision = self.hit_revision.and_then(|revision| revision.checked_add(1));
        self.windows.get_mut(id.0).map(|owned| {
                                      owned.revision =
                                          owned.revision
                                               .and_then(|revision| revision.checked_add(1));
                                      &mut owned.window
                                  })
    }

    pub fn back_to_front(&self) -> impl Iterator<Item = &Window>
    {
        self.order[..self.len].iter().filter_map(|id| self.window(*id)).filter(|w| !w.hidden)
    }


    /// Pure creation checks before cold executable admission. A returned slot
    /// is only a current availability observation, never publication authority;
    /// the real creation turn repeats these checks after any guard-free work.
    pub(crate) fn preflight_creation(&self, bounds: Rect, owner: &OwnerRef, minimum: (i16, i16),
                                     stay_in_back: bool)
                                     -> Result<Option<usize>, CreationError>
    {
        if !owner.accepting()
        {
            return Err(CreationError::Stopping);
        }
        let (min_width, min_height) = minimum;
        #[cfg(systemrisc)]
        let recovery = crate::context::recovery_activity();
        #[cfg(systemrisc)]
        let recovery_slot = if let Some(activity) = &recovery
        {
            if owner.owner() != activity.storage_owner().owner()
               || !activity.recipient().accepting()
               || !crate::context::io_subject().is_some_and(|subject| {
                                                   subject.scope()
                                                          .same_scope(activity.root_subject()
                                                                              .scope())
                                               })
            {
                return Err(CreationError::RecoveryAuthority);
            }
            Some(self.recovery_publications
                     .as_ref()
                     .and_then(|tokens| {
                         tokens.iter()
                               .enumerate()
                               .find(|(index, token)| {
                                   token.ready()
                                   && self.recovery_windows[*index].is_none()
                                   && self.recovery_closes
                                          .get(*index)
                                          .is_some_and(|header| header.available())
                               })
                               .map(|(index, _)| index)
                     })
                     .ok_or(CreationError::RecoveryBusy)?)
        }
        else
        {
            None
        };
        if min_width < 1 || min_height < 1
        {
            return Err(CreationError::InvalidRequest);
        }
        // A stay-in-back window may cover the whole area below the menu bar;
        // ordinary windows keep room for chrome.
        let (limit_left, limit_top, limit_right, limit_bottom) = if stay_in_back
        {
            (0, MENU_BAR_BOTTOM, crate::display::width() as i16, crate::display::height() as i16)
        }
        else
        {
            (1, 25, crate::display::width() as i16 - 2, crate::display::height() as i16 - 2)
        };
        if bounds.is_empty()
           || bounds.left < limit_left
           || bounds.top < limit_top
           || bounds.right > limit_right
           || bounds.bottom > limit_bottom
           || bounds.right - bounds.left < min_width
           || bounds.bottom - bounds.top < min_height
        {
            return Err(CreationError::InvalidRequest);
        }
        #[cfg(not(systemrisc))]
        let recovery_slot = None;
        Ok(recovery_slot)
    }

    /// Background application windows enter the z order without transient focus.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_window_with_activation(&mut self, bounds: Rect, title: &str,
                                             owner: &OwnerRef, minimum: (i16, i16),
                                             procedure: i16, entry: DefinitionRef,
                                             events: &mut EventManager, when: u32, activate: bool,
                                             stay_in_back: bool,
                                             #[cfg(systemrisc)] owner_close: Option<trusted::TrustedBinding>)
                                             -> Result<WindowId, CreationError>
    {
        #[cfg(systemrisc)]
        let recovery = crate::context::recovery_activity();
        #[cfg(systemrisc)]
        let recovery_slot = self.preflight_creation(bounds, owner, minimum, stay_in_back)?;
        #[cfg(not(systemrisc))]
        self.preflight_creation(bounds, owner, minimum, stay_in_back)?;
        let binding =
            crate::toolbox::resources::definitions::bind(entry).map_err(CreationError::Binding)?;
        // These arrays index all owners. Only their capacity is System-owned;
        // the subsequent Window and owner Store backing retain caller charges.
        InvocationGate::with_system_allocation(|| {
            let padding = self.recovery_spare();
            #[cfg(systemrisc)]
            let padding = if recovery.is_some() { 0 } else { padding };
            let required = self.len
                               .checked_add(1)
                               .and_then(|n| n.checked_add(padding))
                               .ok_or(CreationError::NoMemory)?;
            self.visibility.get_mut().prepare(required).map_err(|_| CreationError::NoMemory)?;
            self.order.try_reserve(1 + padding).map_err(|_| CreationError::NoMemory)?;
            self.order_scratch
                .try_reserve(required.saturating_sub(self.order_scratch.len()))
                .map_err(|_| CreationError::NoMemory)?;
            self.geometry_order
                .try_reserve(required.saturating_sub(self.geometry_order.len()))
                .map_err(|_| CreationError::NoMemory)?;
            #[cfg(systemrisc)]
            self.removals
                .try_reserve(self.removal_capacity_required(1 + padding)?)
                .map_err(|_| CreationError::NoMemory)?;
            Ok(())
        })?;
        #[cfg(systemrisc)]
        let source = if owner.owner().kind() == mac_object_table::domain::DomainKind::System
        {
            WindowSource::System {
                resources: crate::kernel::system_resources(owner)
                    .map_err(|_| CreationError::RecoveryAuthority)?,
                recovery: recovery.is_some()
            }
        }
        else
        {
            WindowSource::Application
        };
        #[cfg(systemrisc)]
        let trusted_binding = if crate::toolbox::resources::definitions::is_user(entry)
            .map_err(CreationError::Binding)? { None }
        else if let Some(slot) = recovery_slot
        {
            Some(self.recovery_closes.get(slot).ok_or(CreationError::RecoveryBusy)?
                .prepare_binding().map_err(|_| CreationError::RecoveryBusy)?)
        }
        else if let WindowSource::System { resources, .. } = source
        {
            let header = crate::context::without_recovery(|| {
                crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                    trusted::TrustedCloseHeader::prepare_system(resources, self.work_notice.clone())
                })
            }).map_err(CreationError::Binding)?;
            Some(header.prepare_binding().map_err(CreationError::Binding)?)
        }
        else { owner_close };
        let mut logical_owner = owner.owner().incarnation();
        #[cfg(systemrisc)]
        if let Some(activity) = &recovery
        {
            logical_owner = activity.owner();
        }
        let mut window = Some(OwnedWindow::new(bounds, title, logical_owner, minimum, procedure,
                                               self.gate, binding, stay_in_back,
                                               #[cfg(systemrisc)]
                                               source));
        self.retry_retained_quarantines();
        InvocationGate::with_system_allocation(|| {
            let padding = self.recovery_spare();
            #[cfg(systemrisc)]
            let padding = if recovery.is_some() { 0 } else { padding };
            self.retained_failures.try_reserve(self.len + 1 + padding)
        }).map_err(|_| CreationError::NoMemory)?;
        #[cfg(systemrisc)]
        let reserved = if let (Some(slot), Some(activity)) = (recovery_slot, &recovery)
        {
            let subject = activity.root_subject();
            let tokens = self.recovery_publications.as_mut().ok_or(CreationError::RecoveryBusy)?;
            let mut payload = window.take().ok_or(CreationError::Binding(mac_abi::BAD_ABI))?;
            Some(WindowId(self.windows.insert_reserved_in_scope_with_acknowledgement(
                &mut tokens[slot], subject.scope(), Some(activity.acknowledgement_pin()), |id| {
                    payload.window.id = WindowId(id); payload
                }).map_err(|error| match error {
                    mac_object_table::store::PublicationError::Busy => CreationError::RecoveryBusy,
                    mac_object_table::store::PublicationError::Pool(error) => CreationError::Facility(error)
                })?.id()))
        }
        else
        {
            None
        };
        #[cfg(not(systemrisc))]
        let reserved = None;
        let id = match reserved
        {
            Some(id) => id,
            None =>
            {
                let mut payload = window.take().ok_or(CreationError::Binding(mac_abi::BAD_ABI))?;
                WindowId(self.windows
                             .insert_with(owner, |id| {
                                 payload.window.id = WindowId(id);
                                 payload
                             })
                             .map_err(CreationError::Facility)?)
            }
        };
        self.initialize_window(id, procedure, events, when, activate,
                               #[cfg(systemrisc)]
                               recovery,
                               #[cfg(systemrisc)]
                               recovery_slot,
                               #[cfg(systemrisc)]
                               trusted_binding)
    }

    /// One WDEF lifecycle for ordinary prepared and System-reserved births.
    /// Rollback still invokes the original ticket/quarantine path under Desktop.
    #[allow(clippy::too_many_arguments)]
    fn initialize_window(&mut self, id: WindowId, procedure: i16, events: &mut EventManager,
                         when: u32, activate: bool,
                         #[cfg(systemrisc)] recovery: Option<crate::recovery::Activity>,
                         #[cfg(systemrisc)] recovery_slot: Option<usize>,
                         #[cfg(systemrisc)] trusted_binding: Option<trusted::TrustedBinding>)
                         -> Result<WindowId, CreationError>
    {
        // The canonical slot exists, but public lookup deliberately hides it
        // until NEW and CALCULATE have completed below.
        let stay_in_back =
            self.windows.get(id.0).ok_or(CreationError::Binding(NOT_FOUND))?.window.stay_in_back;
        let previous = self.active;
        let mut guard = InitializingWindow { manager: self,
                                             id,
                                             committed: false };
        #[cfg(systemrisc)]
        if let (Some(activity), Some(slot)) = (recovery, recovery_slot)
        {
            activity.window_created(id).map_err(|_| CreationError::RecoveryBusy)?;
            guard.manager.recovery_windows[slot] = Some(RecoveryWindow { id,
                                                                         activity,
                                                                         retry: None,
                                                                         disposed: false });
        }
        #[cfg(systemrisc)]
        if let Some(binding) = trusted_binding
        {
            let (storage, _) = guard.manager
                                    .windows
                                    .get_with_owner(id.0)
                                    .ok_or(CreationError::Binding(NOT_FOUND))?;
            let owned =
                guard.manager.windows.get_mut(id.0).ok_or(CreationError::Binding(NOT_FOUND))?;
            let handle = binding.bind(owned.window,
                                      storage,
                                      owned.revision
                                           .ok_or(CreationError::Binding(mac_abi::BAD_ABI))?)
                                .map_err(CreationError::Binding)?;
            owned.trusted_close = Some(handle);
        }
        let user_ready = guard.value()?.user_initialization == UserInitialization::Ready;
        for operation in [definition::NEW, definition::CALCULATE]
        {
            if user_ready
            {
                continue;
            }
            let reply = guard.value()?.call(operation, 0, core::ptr::null_mut(), None);
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
           || structure.top < if stay_in_back { MENU_BAR_BOTTOM } else { 25 }
           || structure.right > crate::display::width() as i16
           || structure.bottom > crate::display::height() as i16
        {
            return Err(CreationError::Geometry { procedure });
        }
        guard.value()?.initialized = true;
        let id = guard.commit();
        // Stay-in-back windows sit below every ordinary window, whatever the
        // creation order; among themselves creation order applies.
        let position = if stay_in_back { self.back_prefix() } else { self.len };
        self.hit_revision = self.hit_revision.and_then(|revision| revision.checked_add(1));
        self.order.insert(position, id);
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

    /// Selection changes retained pixels without invalidating application drawing.
    fn queue_selection_damage(&mut self, id: WindowId, region: Result<Region, RegionError>)
    {
        let Ok(region) = region
        else
        {
            self.selection_redraw = true;
            return;
        };
        if region.is_empty() || self.selection_redraw
        {
            return;
        }
        if self.selection_damage.try_reserve(1).is_err()
        {
            self.selection_redraw = true;
            return;
        }
        self.selection_damage.push((id, region));
    }

    pub(crate) fn pop_selection_damage(&mut self) -> Option<(WindowId, Region)>
    {
        self.selection_damage.pop()
    }

    pub(crate) fn take_selection_redraw(&mut self) -> bool
    {
        core::mem::take(&mut self.selection_redraw)
    }

    fn focus_damage(&mut self, id: WindowId)
    {
        let Some(window) = self.window(id).filter(|window| !window.hidden).copied()
        else
        {
            return;
        };
        let frame = window.frame_rects();
        if frame.iter().all(|rect| rect.is_empty())
        {
            return;
        }
        let screen =
            Rect::new(self.screen.left, MENU_BAR_BOTTOM, self.screen.right, self.screen.bottom);
        InvocationGate::with_system_allocation(|| {
            let pieces =
                window.structure_pieces().iter().flat_map(|piece| {
                                                    frame.into_iter().filter_map(move |part| {
                    let mut chrome = Rect::ZERO;
                    let mut visible = Rect::ZERO;
                    (sect_rect(part, *piece, &mut chrome)
                     && sect_rect(chrome, screen, &mut visible)).then_some(visible)
                })
                                                });
            let damage = Self::local_selection_region(&window, pieces);
            self.queue_selection_damage(id, damage);
        });
    }

    fn local_selection_region(window: &Window, pieces: impl Iterator<Item = Rect>)
                              -> Result<Region, RegionError>
    {
        let region = Region::from_rectangles(pieces)?;
        if region.is_empty()
        {
            return Ok(region);
        }
        region.offset(window.bounds.left.checked_neg().ok_or(RegionError::CoordinateOverflow)?,
                      window.bounds.top.checked_neg().ok_or(RegionError::CoordinateOverflow)?)
    }

    /// Raising one window can expose only that window. Intersect the original
    /// occluders with its structure, including the frame outside its content.
    fn raise_damage(&self, id: WindowId, index: usize) -> Result<Region, RegionError>
    {
        let Some(window) = self.window(id).filter(|window| !window.hidden)
        else
        {
            return Ok(Region::new());
        };
        let screen =
            Rect::new(self.screen.left, MENU_BAR_BOTTOM, self.screen.right, self.screen.bottom);
        let covered =
            self.order[index + 1..self.len].iter()
                                           .filter_map(|id| self.window(*id))
                                           .filter(|higher| !higher.hidden)
                                           .flat_map(|higher| {
                                               higher.structure_pieces().iter().copied()
                                           })
                                           .flat_map(|cover| {
                                               window.structure_pieces()
                                                     .iter()
                                                     .filter_map(move |piece| {
                                                         let mut overlap = Rect::ZERO;
                                                         let mut visible = Rect::ZERO;
                                                         (sect_rect(*piece, cover, &mut overlap)
                                   && sect_rect(overlap, screen, &mut visible)).then_some(visible)
                                                     })
                                           });
        Self::local_selection_region(window, covered)
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

    fn back_prefix(&self) -> usize
    {
        self.order[..self.len].iter()
                              .take_while(|id| self.window(**id).is_some_and(|w| w.stay_in_back))
                              .count()
    }

    pub fn select_window(&mut self, id: WindowId, events: &mut EventManager, when: u32)
    {
        if self.window(id).is_some_and(|window| window.stay_in_back)
           || self.order[..self.len].last() == Some(&id)
        {
            self.activate_in_place(id, events, when);
            return;
        }
        if self.active == Some(id) && self.front_window() == Some(id)
        {
            return;
        }
        let Some(index) = self.order[..self.len].iter().position(|other| *other == id)
        else
        {
            return;
        };
        InvocationGate::with_system_allocation(|| {
            let exposed = self.raise_damage(id, index);
            self.front_cache.set(None);
            self.hit_revision = self.hit_revision.and_then(|revision| revision.checked_add(1));
            self.order.copy_within(index + 1..self.len, index);
            self.order[self.len - 1] = id;
            match &exposed
            {
                Ok(region) => self.invalidate_region(id, region),
                Err(_) =>
                {
                    if let Some(bounds) = self.window(id).map(Window::local_bounds)
                    {
                        self.invalidate(id, bounds);
                    }
                }
            }
            self.queue_selection_damage(id, exposed);
        });
        self.activate_in_place(id, events, when);
    }

    /// Activation without a z-order change.
    fn activate_in_place(&mut self, id: WindowId, events: &mut EventManager, when: u32)
    {
        if self.active == Some(id) || self.window(id).is_none()
        {
            return;
        }
        let previous = self.active;
        self.active = Some(id);
        self.refresh_active(previous);
        if let Some(previous) = previous
        {
            self.focus_damage(previous);
        }
        self.focus_damage(id);
        Self::activation(previous, false, events, when);
        Self::activation(Some(id), true, events, when);
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
        for bucket in 0..3
        {
            for id in self.order[..self.len].iter().copied()
            {
                let stays = self.window(id).is_some_and(|window| window.stay_in_back);
                let rank = if stays { 0 } else { 1 + usize::from(ids.contains(&id)) };
                if rank == bucket
                {
                    order.push(id);
                }
            }
        }
        if let GroupFocus::Window(front) = focus
           && ids.contains(&front)
           && !self.window(front).is_some_and(|window| window.stay_in_back)
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
            GroupFocus::Toolbox => None,
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
        self.hit_revision = self.hit_revision.and_then(|revision| revision.checked_add(1));
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
                let mut pending = Some(ticket);
                #[cfg(systemrisc)]
                if let Some(held) =
                    self.recovery_windows.iter_mut().flatten().find(|held| held.id == id)
                {
                    held.retry = pending.take().map(|ticket| (ticket, cause));
                }
                if let Some(ticket) = pending
                {
                    self.quarantine_or_retain(ticket);
                }
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

    #[cfg(systemrisc)]
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

    /// Resolve the actual original full-key payload without window_mut's
    /// revision side effects. Only changed scalar commit advances dependencies.
    #[cfg(systemrisc)]
    pub(crate) fn prepare_control_outline(&mut self, id: WindowId, next: Option<Rect>)
                                          -> Result<ControlOutlinePermit<'_>, mac_abi::Status>
    {
        let owned = self.windows
                        .get_mut(id.0)
                        .filter(|owned| owned.window.initialized)
                        .ok_or(mac_abi::NOT_FOUND)?;
        let revisions = if owned.window.control_outline == next
        {
            None
        }
        else
        {
            Some((owned.revision.and_then(|n| n.checked_add(1)).ok_or(mac_abi::BAD_ABI)?,
                  self.hit_revision.and_then(|n| n.checked_add(1)).ok_or(mac_abi::BAD_ABI)?))
        };
        Ok(ControlOutlinePermit { outline: &mut owned.window.control_outline,
                                  revision: &mut owned.revision,
                                  hit_revision: &mut self.hit_revision,
                                  next,
                                  revisions })
    }

    #[cfg(systemrisc)]
    pub(crate) fn control_outline(&mut self, id: WindowId, next: Option<Rect>)
                                  -> Result<bool, mac_abi::Status>
    {
        let window = self.window_mut(id).ok_or(mac_abi::NOT_FOUND)?;
        Ok(core::mem::replace(&mut window.control_outline, next) != next)
    }

    /// Advisory cursor/drag-hover geometry only. Input admission still runs the
    /// actual definition HIT operation. This never invokes application code or
    /// grants a click/drop target from a cached rectangle.
    pub(crate) fn cached_content_window(&self, point: Point) -> Option<WindowId>
    {
        if point.v >= 0 && point.v < 25
        {
            return None;
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
            return window.bounds.contains(point).then_some(*id);
        }
        None
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
            let part = match probe.call(definition::HIT, param as i64, core::ptr::null_mut(), None)
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

    /// Original captured go-away target; neither front-window state nor mouse
    /// position can substitute for this gesture's actual close admission.
    pub(crate) fn tracking_close_window(&self) -> Option<WindowId>
    {
        match self.tracking
        {
            Some(Tracking::Close { window,
                                   armed: true })
                if self.window(window).is_some() =>
            {
                Some(window)
            }
            _ => None
        }
    }

    /// Cancel only the original close capture after a refused child preflight.
    /// No synthetic input, author call, geometry/content invalidation or release
    /// occurs. The caller schedules this window's changed chrome presentation.
    pub(crate) fn cancel_close_tracking(&mut self, id: WindowId) -> bool
    {
        if !matches!(self.tracking, Some(Tracking::Close { window, .. }) if window == id)
        {
            return false;
        }
        self.tracking = None;
        self.tracking_revision =
            self.tracking_revision.and_then(|revision| revision.checked_add(1));
        if let Some(window) = self.window_mut(id)
        {
            window.state.close_pressed = 0;
        }
        true
    }

    pub fn is_tracking(&self) -> bool
    {
        self.tracking.is_some()
    }

    pub(crate) fn tracking_visual(&self) -> TrackingVisual
    {
        let chrome = match self.tracking
        {
            Some(Tracking::Grow { window,
                                  preview,
                                  .. }) =>
            {
                self.window(window).map(|actual| {
                                       TrackingChrome::Grow { window,
                                                              structure: actual.structure(),
                                                              preview }
                                   })
            }
            Some(Tracking::Close { window,
                                   armed: true }) =>
            {
                self.window(window).map(|actual| {
                                       TrackingChrome::Close { window,
                                                               structure: actual.structure() }
                                   })
            }
            Some(Tracking::Zoom { window,
                                  armed: true }) =>
            {
                self.window(window).map(|actual| {
                                       TrackingChrome::Zoom { window,
                                                              structure: actual.structure() }
                                   })
            }
            _ => None
        };
        TrackingVisual { tracking: self.is_tracking(),
                         outline: self.drag_outline(),
                         chrome }
    }

    /// Pressed close/zoom chrome belongs to the scene compositor; GROW is an overlay.
    pub fn tracking_repaints_scene(&self) -> bool
    {
        matches!(self.tracking, Some(Tracking::Close { .. } | Tracking::Zoom { .. }))
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
        self.tracking_revision =
            self.tracking_revision.and_then(|revision| revision.checked_add(1));
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

    pub fn track(&mut self, point: Point, released: bool, events: &mut EventManager, when: u32,
                 mut prepare: impl FnMut(&Self, WindowId, Rect) -> Result<(), Status>)
                 -> Result<Option<Action>, Status>
    {
        let mut order = core::mem::take(&mut self.geometry_order);
        order.clear();
        if released
           && matches!(self.tracking,
                       Some(Tracking::Drag { .. } | Tracking::Grow { .. } | Tracking::Zoom { .. }))
        {
            order.extend_from_slice(&self.order);
        }
        let result = self.track_prepared(point, released, events, when, &order, &mut prepare);
        if released
        {
            self.tracking = None;
        }
        self.geometry_order = order;
        result
    }

    fn track_prepared(&mut self, point: Point, released: bool, events: &mut EventManager,
                      when: u32, old_order: &[WindowId],
                      prepare: &mut impl FnMut(&Self, WindowId, Rect) -> Result<(), Status>)
                      -> Result<Option<Action>, Status>
    {
        let Some(tracking) = self.tracking
        else
        {
            return Ok(None);
        };
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
                   && let Some(mut next) = self.window(window).copied()
                {
                    let w = &mut next;
                    zoom_geometry(w, self.screen);
                    #[cfg(systemrisc)]
                    self.preflight_geometry(window)?;
                    if !w.calculate()
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    if self.window(window)
                           .is_some_and(|old| old.local_bounds() != next.local_bounds())
                    {
                        prepare(self, window, next.local_bounds())?;
                    }
                    if let Some(w) = self.window_mut(window)
                    {
                        *w = next;
                    }
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
                let preview = drag_geometry(bounds, start, point, self.screen);
                self.tracking = Some(Tracking::Drag { window,
                                                      start,
                                                      bounds,
                                                      preview,
                                                      activate });
                if released && let Some(next) = preview
                {
                    if let Some(mut staged) = self.window(window).copied()
                       && next != staged.bounds
                    {
                        staged.bounds = next;
                        staged.user_bounds = None;
                        staged.state.zoomed = 0;
                        #[cfg(systemrisc)]
                        self.preflight_geometry(window)?;
                        if !staged.calculate()
                        {
                            return Err(BAD_ARGUMENT);
                        }
                        *self.window_mut(window).ok_or(NOT_FOUND)? = staged;
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
                let w = self.window(window).ok_or(NOT_FOUND)?;
                let next = grow_geometry(w, bounds, start, point, self.screen);
                self.tracking = Some(Tracking::Grow { window,
                                                      start,
                                                      bounds,
                                                      preview: next });
                if released && self.window(window).is_some_and(|w| next != w.bounds)
                {
                    let mut staged = *self.window(window).ok_or(NOT_FOUND)?;
                    staged.bounds = next;
                    staged.user_bounds = None;
                    staged.state.zoomed = 0;
                    #[cfg(systemrisc)]
                    self.preflight_geometry(window)?;
                    if !staged.calculate()
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    prepare(self, window, staged.local_bounds())?;
                    if let Some(w) = self.window_mut(window)
                    {
                        *w = staged;
                    }
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
                        let owner = self.window(window).ok_or(NOT_FOUND)?.owner;
                        if let Err(error) = self.dispose_window(window, events, when)
                        {
                            #[cfg(systemrisc)]
                            serial!("WINDOW_CLOSE_REFUSED: owner={owner} {error}");
                            #[cfg(not(systemrisc))]
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
        Ok(action)
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
            Region::from_rect(window.bounds).combine(&Region::from_rect(Rect::new(0, 20, crate::display::width() as i16,
                                                                                  crate::display::height() as i16)),
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

    /// Stage new bounds, let the caller prepare content for them, then commit.
    /// Nothing changes if preparation or geometry is refused.
    pub fn set_bounds(&mut self, id: WindowId, bounds: Rect,
                      mut prepare: impl FnMut(&Self, WindowId, Rect) -> Result<(), Status>)
                      -> Result<Option<Action>, Status>
    {
        let current = self.window(id).ok_or(NOT_FOUND)?;
        if current.bounds == bounds
        {
            return Ok(None);
        }
        #[cfg(systemrisc)]
        self.preflight_geometry(id)?;
        let mut staged = *current;
        staged.bounds = bounds;
        staged.user_bounds = None;
        staged.state.zoomed = 0;
        if !staged.calculate()
        {
            return Err(BAD_ARGUMENT);
        }
        prepare(self, id, staged.local_bounds())?;
        *self.window_mut(id).ok_or(NOT_FOUND)? = staged;
        Ok(Some(Action::Resize(id)))
    }

    fn screen_fit_bounds(window: &Window, screen: Rect, bar: i16) -> Option<Rect>
    {
        let structure = window.structure();
        let mut dx = (screen.right - structure.right).min(0);
        dx = dx.max(screen.left - (structure.left + dx));
        let mut dy = (screen.bottom - structure.bottom).min(0);
        dy = dy.max(screen.top + bar - (structure.top + dy));
        (dx != 0 || dy != 0).then(|| {
                                Rect::new(window.bounds.left + dx,
                                          window.bounds.top + dy,
                                          window.bounds.right + dx,
                                          window.bounds.bottom + dy)
                            })
    }

    /// Before device or geometry effects, in the SAME Desktop exclusion as
    /// fit_to_screen. No author executes here. Actual claimed painters are
    /// cancelled to drain, and the caller retains the original settled size.
    #[cfg(systemrisc)]
    pub(crate) fn preflight_fit_to_screen(&self, screen: Rect, bar: i16) -> Result<(), Status>
    {
        for &id in &self.order[..self.len]
        {
            let Some(window) = self.window(id).filter(|window| !window.stay_in_back)
            else
            {
                continue;
            };
            if Self::screen_fit_bounds(window, screen, bar).is_none()
            {
                continue;
            }
            let owned = self.windows.get(window.id.0).ok_or(NOT_FOUND)?;
            if !owned.binding.matches(window.entry)
            {
                return Err(mac_abi::BAD_ABI);
            }
            if crate::toolbox::resources::definitions::is_user(window.entry)?
            {
                return Err(mac_abi::NOT_SUPPORTED);
            }
            owned.canvas_guardian()?.preflight_geometry()?;
        }
        Ok(())
    }

    /// Adopt new main-screen bounds and move every ordinary window back onto
    /// them without changing stacking: whole window when it fits, otherwise at
    /// least its title bar, below the menu bar. Enlarging moves nothing.
    /// Stay-in-back windows are left to their owner, who resizes them normally.
    /// Shipping callers must preflight_fit_to_screen in this SAME Desktop turn
    /// before changing the device. The commit never silently skips a claim.
    pub(crate) fn fit_to_screen(&mut self, screen: Rect, bar: i16, mut moved: impl FnMut(WindowId))
    {
        self.screen = screen;
        let mut order = core::mem::take(&mut self.geometry_order);
        order.clear();
        order.extend_from_slice(&self.order[..self.len]);
        for index in 0..order.len()
        {
            let id = order[index];
            let Some(previous) = self.window(id).copied().filter(|w| !w.stay_in_back)
            else
            {
                continue;
            };
            let Some(bounds) = Self::screen_fit_bounds(&previous, screen, bar)
            else
            {
                continue;
            };
            let mut staged = previous;
            staged.bounds = bounds;
            staged.user_bounds = None;
            staged.state.zoomed = 0;
            if !staged.calculate()
            {
                continue;
            }
            if let Some(window) = self.window_mut(id)
            {
                *window = staged;
            }
            self.invalidate_geometry_exposure(&previous, &order, Action::Drag(id));
            moved(id);
        }
        self.geometry_order = order;
    }

    pub fn set_standard_bounds(&mut self, id: WindowId, bounds: Option<Rect>)
                               -> Result<(), Status>
    {
        if self.window(id).ok_or(NOT_FOUND)?.standard_bounds == bounds
        {
            return Ok(());
        }
        #[cfg(systemrisc)]
        self.preflight_geometry(id)?;
        let window = self.window_mut(id).ok_or(NOT_FOUND)?;
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
               || structure.right > crate::display::width() as i16
               || structure.bottom > (crate::display::height() as i16 - 21)
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

    /// Draw one selected layer through the same definitions/content renderer.
    /// Occlusion comes only from included windows. Transient tracking belongs
    /// to the present-time overlay and is excluded from the scene buffer.
    pub(crate) fn draw_layer(&self, c: &mut Canvas, include: impl Fn(&Window) -> bool,
                             mut paint: impl FnMut(&Window, &mut Canvas) -> Result<(), Status>)
                             -> Result<(), Status>
    {
        #[cfg(systemrisc)]
        let scopes = self.resource_scopes();
        for window in self.back_to_front().filter(|window| include(window))
        {
            let draw = || -> Result<(), Status> {
                let mut copy = *window;
                copy.state.close_pressed = u32::from(matches!(self.tracking,
                    Some(Tracking::Close {window: id, armed: true}) if id == window.id));
                copy.state.zoom_pressed = u32::from(matches!(self.tracking,
                    Some(Tracking::Zoom {window: id, armed: true}) if id == window.id));
                #[cfg(systemrisc)]
                let system = window.uses_system_resources();
                #[cfg(not(systemrisc))]
                let system = false;
                self.draw_layer_piece(c, &mut copy, definition::DRAW, system)?;
                let mut content = Ok(());
                let mut visible = |rect| {
                    if content.is_err()
                    {
                        return;
                    }
                    c.reset_port();
                    c.set_origin(window.bounds.left, window.bounds.top);
                    let mut local: Rect = rect;
                    local.offset(-window.bounds.left, -window.bounds.top);
                    c.set_clip(local);
                    content = paint(window, c);
                };
                if system
                {
                    self.for_each_visible_rect(window.id, &mut visible);
                }
                else
                {
                    self.for_each_layer_visible_rect(window.id, &include, &mut visible);
                }
                content?;
                self.draw_layer_piece(c, &mut copy, definition::DRAW_GROW_ICON, system)
            };
            #[cfg(systemrisc)]
            let result = scopes.with_window(window, draw).and_then(core::convert::identity);
            #[cfg(not(systemrisc))]
            let result = {
                let mut draw = draw;
                draw()
            };
            if let Err(status) = result
            {
                c.reset_port();
                return Err(status);
            }
        }
        c.reset_port();
        Ok(())
    }

    /// Source selection never changes z-order. Genuine System chrome and grow
    /// pieces remain clipped below every actual higher window, including apps.
    fn draw_layer_piece(&self, c: &mut Canvas, window: &mut Window, message: i16,
                        actual_occlusion: bool)
                        -> Result<(), Status>
    {
        let id = window.id;
        let structure = window.structure();
        let mut result = Ok(());
        let mut draw = |mut clip| {
            if result.is_err()
            {
                return;
            }
            if message == definition::DRAW_GROW_ICON
               && let Some(mut grow) = window.grow_icon_clip
            {
                grow.offset(window.bounds.left, window.bounds.top);
                let mut hit = Rect::ZERO;
                if !sect_rect(grow, clip, &mut hit)
                {
                    return;
                }
                clip = hit;
            }
            c.reset_port();
            c.set_clip(clip);
            let mut surface = Surface::from_canvas(c);
            let reply = window.call(message, 0, &mut surface, None);
            if reply < 0
            {
                result = Err(i32::try_from(reply).unwrap_or(mac_abi::BAD_ABI));
            }
        };
        if actual_occlusion
        {
            let Some(index) = self.order[..self.len].iter().position(|other| *other == id)
            else
            {
                return Err(mac_abi::NOT_FOUND);
            };
            self.visible_fragments(structure, &self.order[index + 1..self.len], &mut draw);
        }
        else
        {
            draw(Rect::new(0, 0, crate::display::width() as i16, crate::display::height() as i16));
        }
        result
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

    /// The same prepared sweep, restricted to one renderer layer.
    pub(crate) fn for_each_layer_visible_rect(&self, id: WindowId,
                                              include: &impl Fn(&Window) -> bool,
                                              visit: impl FnMut(Rect))
    {
        let Some(index) = self.order[..self.len].iter().position(|other| *other == id)
        else
        {
            return;
        };
        let Some(window) = self.window(id).filter(|window| include(window))
        else
        {
            return;
        };
        let rectangles =
            self.order[index + 1..self.len].iter()
                                           .filter_map(|id| self.window(*id))
                                           .filter(|window| !window.hidden && include(window))
                                           .flat_map(|window| {
                                               window.structure_pieces().iter().copied()
                                           });
        self.visibility.borrow_mut().visit(window.bounds, rectangles, visit);
    }

    /// The same visibility sweep for bounded frame damage. Content visibility
    /// alone excludes the title bar, so it cannot be used for Close/Zoom paint.
    pub(crate) fn for_each_layer_frame_rect(&self, id: WindowId, bounds: Rect,
                                            include: &impl Fn(&Window) -> bool,
                                            visit: impl FnMut(Rect))
    {
        let Some(index) = self.order[..self.len].iter().position(|other| *other == id)
        else
        {
            return;
        };
        let Some(window) = self.window(id).filter(|window| include(window))
        else
        {
            return;
        };
        let mut target = Rect::ZERO;
        if !sect_rect(bounds, window.structure(), &mut target)
        {
            return;
        }
        let higher =
            self.order[index + 1..self.len].iter()
                                           .filter_map(|id| self.window(*id))
                                           .filter(|window| !window.hidden && include(window))
                                           .flat_map(|window| {
                                               window.structure_pieces().iter().copied()
                                           });
        self.visibility.borrow_mut().visit(target, higher, visit);
    }

    /// Copy committed ordinary pixels only where no genuine System source
    /// structure covers them. This never recomposes or samples service pixels.
    #[cfg(systemrisc)]
    pub(crate) fn for_each_ordinary_underlay_rect(&self, bounds: Rect, mut visit: impl FnMut(Rect))
    {
        if !self.back_to_front().any(|window| window.uses_system_resources())
        {
            visit(bounds);
            return;
        }
        self.for_each_desktop_rect(bounds, &mut visit);
        for (index, id) in self.order[..self.len].iter().enumerate()
        {
            let Some(window) =
                self.window(*id).filter(|window| !window.hidden && !window.uses_system_resources())
            else
            {
                continue;
            };
            for piece in window.structure_pieces()
            {
                let mut requested = Rect::ZERO;
                if sect_rect(bounds, *piece, &mut requested)
                {
                    self.visible_fragments(requested, &self.order[index + 1..self.len], &mut visit);
                }
            }
        }
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


