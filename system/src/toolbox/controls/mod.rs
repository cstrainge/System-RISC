//! Resident Control Manager. Each CDEF owns drawing, hit geometry and tracking math.
use crate::toolbox::resources::definitions::DefinitionRef;
mod snapshot;
mod tracking;
mod reflow;
mod effects;
pub(crate) mod offscreen;
pub(crate) mod paint;
use alloc::sync::Arc;

pub use mac_abi::definitions::{CDEF, ControlDefinitionState};
use mac_abi::{controls::*, geometry::Rect, *};

use crate::toolbox::quickdraw::{Canvas, SurfaceExt};
enum State
{
    Scrollbar(ScrollbarState),
    Progress
    {
        bounds:   Rect,
        progress: ProgressRange,
        flags:    i16
    },
    Standard(snapshot::Standard)
}
impl State
{
    fn standard(&self) -> Option<ControlDefinitionState>
    {
        match self
        {
            Self::Standard(s) => Some(s.wire()),
            Self::Progress { bounds,
                             progress,
                             flags } =>
            {
                Some(ControlDefinitionState { bounds: *bounds,
                                              progress: *progress,
                                              flags: *flags,
                                              ..Default::default() })
            }
            _ => None
        }
    }
}
/// Retained immutable definition state, loader-validated entry and service gate.
pub struct Drawing
{
    pub id:    u64,
    origin:    Option<objects::ManagedPaintOrigin>,
    state:     State,
    gate:      TrapGate,
    variation: i16,
    damage:    [Rect; 2],
    reflow:    u32,
    visible:   Option<Rect>,
    grid:      Option<Arc<snapshot::Grid>>,
    bars:      Option<alloc::boxed::Box<snapshot::Bars>>,
    // All retained appearance/auxiliary payloads precede this original reader.
    source:    crate::toolbox::resources::definitions::RetainedDefinition
}
impl Drawing
{
    pub(crate) fn managed_origin(&self) -> Option<objects::ManagedPaintOrigin>
    {
        self.origin
    }

    /// Clear a retained capture highlight without executing a definition.
    pub fn cancel_tracking(&mut self) -> bool
    {
        let mut changed = false;
        if let Some(bars) = &mut self.bars
        {
            for bar in bars.bars.iter_mut().flatten()
            {
                changed |= core::mem::replace(&mut bar.highlighted, 0) != 0;
            }
        }
        changed
        | match &mut self.state
        {
            State::Standard(state) => state.clear_highlight(),
            State::Scrollbar(state) => core::mem::replace(&mut state.highlighted, 0) != 0,
            State::Progress { .. } => false
        }
    }

    pub(crate) fn capture_bounds(&self) -> Rect
    {
        let bounds =
            self.bars
                .as_ref()
                .and_then(|bars| bars.bars.iter().flatten().find(|bar| bar.highlighted != 0))
                .map_or(self.bounds(), |bar| bar.bounds);
        self.visible.map_or(bounds, |clip| intersection(bounds, clip))
    }

    pub(crate) fn visible_bounds(&self) -> Rect
    {
        let bounds = self.bounds();
        self.visible.map_or(bounds, |visible| intersection(bounds, visible))
    }

    pub(crate) fn set_visible(&mut self, visible: Option<Rect>)
    {
        self.visible = visible;
        let clip = self.visible_bounds();
        for damage in &mut self.damage
        {
            *damage = intersection(*damage, clip);
        }
    }

    pub fn damage(&self) -> [Rect; 2]
    {
        self.damage
    }

    /// CDEF-owned delta geometry. A changed value need not change a pixel.
    pub fn change_from(&mut self, before: ControlDefinitionState) -> Result<(), Status>
    {
        let author = effects::Author::trusted(self.gate);
        effects::immediate(self.change_from_with(before, &author))
    }

    async fn change_from_with(&mut self, before: ControlDefinitionState, author: &effects::Author)
                              -> Result<(), Status>
    {
        if self.variation != PROGRESS_BAR_PROCEDURE
        {
            return Ok(());
        }
        let after = self.state.standard().ok_or(BAD_ABI)?;
        let change = author.change(self.source.reference(),
                                   self.reflow,
                                   ControlChange { before,
                                                   after,
                                                   damage: [Rect::ZERO; 2] })
                           .await?;
        for r in change.damage
        {
            // A definition cannot invalidate its parent or another object.
            if !r.is_empty()
               && (r.left < after.bounds.left
                   || r.top < after.bounds.top
                   || r.right > after.bounds.right
                   || r.bottom > after.bounds.bottom)
            {
                return Err(BAD_ABI);
            }
        }
        self.damage = change.damage;
        Ok(())
    }

    pub fn bounds(&self) -> Rect
    {
        match &self.state
        {
            State::Standard(s) => s.bounds,
            State::Progress { bounds,
                              .. } => *bounds,
            State::Scrollbar(s) => s.bounds
        }
    }

    pub fn storage(&self) -> usize
    {
        core::mem::size_of::<Self>()
        + self.grid.as_ref().map_or(0, |grid| {
                                core::mem::size_of::<snapshot::Grid>()
                                + grid.cells.len() * core::mem::size_of::<Rect>()
                                + grid.dividers.as_ref().map_or(0, |d| d.storage())
                                + grid.logical.len()
                                  * core::mem::size_of::<mac_abi::controls::reflow::LogicalRect>()
                            })
        + self.bars.as_ref().map_or(0, |_| core::mem::size_of::<snapshot::Bars>())
        + match &self.state
        {
            State::Standard(state) => state.storage(),
            _ => 0
        }
    }

    pub(crate) fn has_user_sources(&self) -> bool
    {
        self.source.is_process_source()
        || self.bars.as_ref().is_some_and(|bars| bars.definition.is_process_source())
        || self.grid
               .as_ref()
               .and_then(|grid| grid.dividers.as_ref())
               .is_some_and(|dividers| dividers.definition.is_process_source())
    }

    pub fn paint(&self, canvas: &mut Canvas)
    {
        let _ = self.paint_clipped(canvas);
    }

    pub fn paint_clipped(&self, canvas: &mut Canvas) -> Result<(), Status>
    {
        // The immediate production path uses the same ordered cursor. User
        // work is honestly deferred until a canonical owner can retain it.
        match paint::run(self, &mut paint::Cursor::default(), canvas, false)?
        {
            paint::Progress::Complete => Ok(()),
            paint::Progress::User(_) => Err(SERVICE_BUSY)
        }
    }

    pub(crate) fn paint_step(&self, cursor: &mut paint::Cursor, canvas: &mut Canvas)
                             -> Result<paint::Progress, Status>
    {
        paint::run(self, cursor, canvas, true)
    }
}
fn standard_definition(procedure: i16) -> Result<i16, Status>
{
    match procedure
    {
        0..=2
        | 8..=10
        | PROGRESS_BAR_PROCEDURE
        | STATIC_TEXT_PROCEDURE
        | TABLE_PROCEDURE
        | SPLITTER_PROCEDURE
        | SEPARATOR_PROCEDURE => Ok(procedure >> 4),
        _ => Err(NOT_SUPPORTED)
    }
}
fn variation(procedure: i16) -> i16
{
    procedure & 15
}
pub fn definition_id(call: &ControlCall) -> Result<i16, Status>
{
    match call.operation
    {
        DRAW_SCROLLBAR | HIT_SCROLLBAR | TRACK_SCROLLBAR => Ok(1),
        DRAW_STANDARD | HIT_STANDARD => standard_definition(call.procedure),
        TRACK_SPLITTER if call.procedure == SPLITTER_PROCEDURE => Ok(CDEF_SPLITTER),
        _ => Err(NOT_SUPPORTED)
    }
}
fn validate_state(call: &ControlCall, id: i16) -> Result<(), Status>
{
    if call.reflow > 1
    {
        return Err(BAD_ARGUMENT);
    }
    let bounds = if id == 1 { call.scrollbar.bounds } else { call.standard.bounds };
    if call.reflow == 1
    {
        sr_control_reflow_handling::LayoutRect::try_from(bounds).map_err(|e| e.status())?;
    }
    if id == 1
    {
        let state = &call.scrollbar;
        if state.maximum < 0
           || state.value < 0
           || state.value > state.maximum
           || (state.bounds.is_empty() && call.reflow == 0)
        {
            return Err(BAD_ARGUMENT);
        }
    }
    else
    {
        let state = &call.standard;
        if state.bounds.is_empty() && call.reflow == 0
        {
            return Err(BAD_ARGUMENT);
        }
        let title = state.title.get(..state.title_length as usize).ok_or(BAD_ARGUMENT)?;
        core::str::from_utf8(title).map_err(|_| BAD_ARGUMENT)?;
    }
    Ok(())
}
/// Validate wire/state structure without invoking a definition or allocating.
/// Cold-load preflight and the actual callback path share these checks.
pub(crate) fn preflight(call: &ControlCall) -> Result<i16, Status>
{
    if call.reserved != 0
    {
        return Err(BAD_ARGUMENT);
    }
    let id = definition_id(call)?;
    validate_state(call, id)?;
    if id == 1
    {
        mac_definition::validation::scrollbar(&call.scrollbar).map_err(|e| e.status())?;
    }
    else
    {
        mac_definition::validation::standard(id, variation(call.procedure), &call.standard)
            .map_err(|e| e.status())?;
    }
    Ok(id)
}
/// Validate CDEF state without allocating a retained drawing.
pub(crate) fn validate(call: &mut ControlCall, entry: DefinitionRef, gate: TrapGate)
                       -> Result<(), Status>
{
    let id = preflight(call)?;
    let object = if id == 1
    {
        (&mut call.scrollbar as *mut ScrollbarState).cast()
    }
    else
    {
        (&mut call.standard as *mut ControlDefinitionState).cast()
    };
    let mut validation = DefinitionCall { reflow: call.reflow,
                                          header: Header::for_type::<DefinitionCall>(),
                                          kind: CDEF,
                                          id,
                                          variation: variation(call.procedure),
                                          message: VALIDATE_DEFINITION,
                                          reserved: 0,
                                          param: 0,
                                          object,
                                          gate,
                                          surface: core::ptr::null_mut() };
    let status = super::resources::definitions::invoke(entry, &mut validation);
    if status < 0
    {
        return Err(status as Status);
    }
    Ok(())
}

/// Select a creation's genuine definition through its corresponding draw
/// family, retaining CREATE as the canonical manager publication intent.
pub(crate) fn creation_definition_id(call: &ControlCall) -> Result<i16, Status>
{
    if !matches!(call.operation, CREATE_STANDARD | CREATE_SCROLLBAR)
    {
        return Err(BAD_ARGUMENT);
    }
    definition_id(&validation_state(call))
}
fn validation_state(call: &ControlCall) -> ControlCall
{
    let mut state = *call;
    state.operation = match call.operation
    {
        CREATE_STANDARD => DRAW_STANDARD,
        CREATE_SCROLLBAR => DRAW_SCROLLBAR,
        other => other
    };
    state
}

pub(crate) fn user_validation_request(
    call: &ControlCall)
    -> Result<crate::toolbox::resources::definitions::callbacks::packet::Request, Status>
{
    use crate::toolbox::resources::definitions::callbacks::packet::{Arguments, Object, Request};
    let id = preflight(&validation_state(call))?;
    Ok(Request { kind: CDEF,
                 id,
                 variation: variation(call.procedure),
                 message: VALIDATE_DEFINITION,
                 reflow: call.reflow,
                 object: if id == 1
                 {
                     Object::Scrollbar(call.scrollbar)
                 }
                 else
                 {
                     Object::Control(call.standard)
                 },
                 arguments: Arguments::Scalar(0),
                 surface: None })
}

/// Canonical retained state (a DRAW-shaped call) disposed through the author.
pub(crate) fn user_disposal_request(
    call: &ControlCall, id: i16)
    -> crate::toolbox::resources::definitions::callbacks::packet::Request
{
    use crate::toolbox::resources::definitions::callbacks::packet::{Arguments, Object, Request};
    Request { kind: CDEF,
              id,
              variation: variation(call.procedure),
              message: mac_abi::definitions::DISPOSE,
              reflow: call.reflow,
              object: if id == 1
              {
                  Object::Scrollbar(call.scrollbar)
              }
              else
              {
                  Object::Control(call.standard)
              },
              arguments: Arguments::Scalar(0),
              surface: None }
}

pub(crate) fn apply_user_validation(call: &ControlCall,
                                    reply: &crate::toolbox::resources::definitions::callbacks::packet::Reply)
                                    -> Result<ControlCall, Status>
{
    let id = preflight(&validation_state(call))?;
    if reply.result < 0
    {
        return Err(Status::try_from(reply.result).unwrap_or(BAD_ABI));
    }
    let mut validated = *call;
    validated.operation = if id == 1 { DRAW_SCROLLBAR } else { DRAW_STANDARD };
    if id == 1
    {
        validated.scrollbar = reply.scrollbar()?;
    }
    else
    {
        validated.standard = reply.control()?;
    }
    preflight(&validated)?;
    Ok(validated)
}
/// Validate and resolve a typed request. No per-control geometry lives here.
pub(crate) fn prepare(call: &mut ControlCall, entry: DefinitionRef, gate: TrapGate)
                      -> Result<Option<Drawing>, Status>
{
    prepare_with_title(call, entry, gate, None)
}
fn prepare_with_title(call: &mut ControlCall, entry: DefinitionRef, gate: TrapGate,
                      title: Option<&Arc<snapshot::Title>>)
                      -> Result<Option<Drawing>, Status>
{
    let id = preflight(call)?;
    if matches!(call.operation, DRAW_SCROLLBAR | DRAW_STANDARD)
    {
        validate(call, entry, gate)?;
        return drawing_validated(call, entry, gate, title).map(Some);
    }
    validate_state(call, id)?;
    let object = if id == 1
    {
        (&mut call.scrollbar as *mut ScrollbarState).cast()
    }
    else
    {
        (&mut call.standard as *mut ControlDefinitionState).cast()
    };
    let mut bits = (call.point.h as u16 as u64) | ((call.point.v as u16 as u64) << 16);
    if matches!(call.operation, TRACK_SCROLLBAR | TRACK_SPLITTER)
    {
        bits |= ((call.anchor.h as u16 as u64) << 32) | ((call.anchor.v as u16 as u64) << 48);
    }
    let mut definition = DefinitionCall { reflow: call.reflow,
                                          header: Header::for_type::<DefinitionCall>(),
                                          kind: CDEF,
                                          id,
                                          variation: variation(call.procedure),
                                          message: if matches!(call.operation,
                                                               TRACK_SCROLLBAR | TRACK_SPLITTER)
                                          {
                                              2
                                          }
                                          else
                                          {
                                              1
                                          },
                                          reserved: 0,
                                          param: bits as i64,
                                          object,
                                          gate,
                                          surface: core::ptr::null_mut() };
    let result = super::resources::definitions::invoke(entry, &mut definition);
    if result < 0
    {
        return Err(result as Status);
    }
    call.result = result as i32;
    Ok(None)
}
/// Shared construction after the actual definition VALIDATE has completed.
fn drawing_validated(call: &ControlCall, entry: DefinitionRef, gate: TrapGate,
                     title: Option<&Arc<snapshot::Title>>)
                     -> Result<Drawing, Status>
{
    let id = definition_id(call)?;
    let source = super::resources::definitions::bind(entry)?.into_retained_source();
    Ok(Drawing { id: 0,
                 origin: None,
                 reflow: call.reflow,
                 visible: None,
                 grid: None,
                 bars: None,
                 state: if id == 1
                 {
                     State::Scrollbar(call.scrollbar)
                 }
                 else if id == CDEF_PROGRESS_BAR
                 {
                     State::Progress { bounds:   call.standard.bounds,
                                       progress: call.standard.progress,
                                       flags:    call.standard.flags }
                 }
                 else
                 {
                     State::Standard(snapshot::Standard::new(&call.standard, title)?)
                 },
                 source,
                 gate,
                 variation: if id != 1 { call.procedure } else { 0 },
                 damage: [if id == 1 { call.scrollbar.bounds } else { call.standard.bounds },
                          Rect::ZERO] })
}
/// Draw into the checked exclusive borrowed surface, preserving clipping/origin.
/// # Safety
/// The surface and its pixels must remain live and exclusively borrowed.
pub unsafe fn paint_surface(drawing: &Drawing, surface: *mut Surface) -> Result<(), Status>
{
    if surface.is_null() || !(surface as usize).is_multiple_of(core::mem::align_of::<Surface>())
    {
        return Err(BAD_ARGUMENT);
    }
    let mut canvas = unsafe { (&mut *surface).canvas() }.ok_or(BAD_ARGUMENT)?;
    drawing.paint_clipped(&mut canvas)
}
pub mod native
{
    use super::*;
    /// Shared ABI adapter for offscreen control services, including loaded CDEF calls.
    /// # Safety
    /// Request/surface storage must be live and exclusively borrowed for this call.
    pub(crate) unsafe fn dispatch(number: u32, pointer: *mut (), size: usize,
                                  mut resolve: impl FnMut(i16) -> Result<DefinitionRef, Status>,
                                  gate: TrapGate)
                                  -> i64
    {
        let result = (|| -> Result<(), Status> {
            if number != mac_abi::traps::CONTROL
            {
                return Err(NOT_SUPPORTED);
            }
            if pointer.is_null()
               || size != core::mem::size_of::<ControlCall>()
               || !(pointer as usize).is_multiple_of(core::mem::align_of::<ControlCall>())
            {
                return Err(BAD_ARGUMENT);
            }
            let call = unsafe { &mut *pointer.cast::<ControlCall>() };
            if call.reflow != 0
            {
                return Err(BAD_ARGUMENT);
            }
            let id = preflight(call)?;
            if matches!(call.operation, DRAW_SCROLLBAR | DRAW_STANDARD)
            {
                if call.surface.is_null()
                   || !(call.surface as usize).is_multiple_of(core::mem::align_of::<Surface>())
                {
                    return Err(BAD_ARGUMENT);
                }
                // End the checked pixel borrow before cold executable loading.
                // The real drawing path revalidates it before painting; no
                // canvas or surface reference crosses allocation/publication.
                unsafe { (&mut *call.surface).canvas() }.ok_or(BAD_ARGUMENT)?;
            }
            let entry = resolve(id)?;
            if let Some(drawing) = prepare(call, entry, gate)?
            {
                unsafe { paint_surface(&drawing, call.surface) }?;
            }
            Ok(())
        })();
        result.map_or_else(|e| e as i64, |_| 0)
    }
}
pub mod scrollbox;
pub mod scrollbox_native;
pub mod objects;

fn intersection(a: Rect, b: Rect) -> Rect
{
    let result = Rect::new(a.left.max(b.left),
                           a.top.max(b.top),
                           a.right.min(b.right),
                           a.bottom.min(b.bottom));
    if result.is_empty() { Rect::ZERO } else { result }
}
