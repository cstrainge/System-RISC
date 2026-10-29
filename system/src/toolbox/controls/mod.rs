//! Resident Control Manager. Each CDEF owns drawing, hit geometry and tracking math.
mod snapshot;
mod tracking;
use alloc::sync::Arc;

pub use mac_abi::definitions::{CDEF, ControlDefinitionState};
use mac_abi::{controls::*, *};

use crate::toolbox::quickdraw::{Canvas, SurfaceExt};
enum State
{
    Scrollbar(ScrollbarState),
    Progress
    {
        bounds:   Rect,
        progress: ProgressRange
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
                             progress } =>
            {
                Some(ControlDefinitionState { bounds: *bounds,
                                              progress: *progress,
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
    state:     State,
    entry:     DefinitionEntry,
    gate:      TrapGate,
    variation: i16,
    damage:    [Rect; 2]
}
impl Drawing
{
    /// Clear a retained capture highlight without executing a definition.
    pub fn cancel_tracking(&mut self) -> bool
    {
        match &mut self.state
        {
            State::Standard(state) => state.clear_highlight(),
            State::Scrollbar(state) => core::mem::replace(&mut state.highlighted, 0) != 0,
            State::Progress { .. } => false
        }
    }

    pub fn damage(&self) -> [Rect; 2]
    {
        self.damage
    }

    /// CDEF-owned delta geometry. A changed value need not change a pixel.
    pub fn change_from(&mut self, before: ControlDefinitionState) -> Result<(), Status>
    {
        if !matches!(self.variation, PROGRESS_BAR_PROCEDURE | MONOCHROME_PROGRESS_BAR_PROCEDURE)
        {
            return Ok(());
        }
        let after = self.state.standard().ok_or(BAD_ABI)?;
        let mut change = ControlChange { before,
                                         after,
                                         damage: [Rect::ZERO; 2] };
        let mut call = DefinitionCall { header:    Header::for_type::<DefinitionCall>(),
                                        kind:      CDEF,
                                        id:        CDEF_PROGRESS_BAR,
                                        variation: 0,
                                        message:   CHANGE_BOUNDS_DEFINITION,
                                        reserved:  0,
                                        param:     0,
                                        object:    (&mut change as *mut ControlChange).cast(),
                                        gate:      self.gate,
                                        surface:   core::ptr::null_mut() };
        let status = super::resources::definitions::invoke(self.entry, &mut call);
        if status < 0
        {
            return Err(status as Status);
        }
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
        + match &self.state
        {
            State::Standard(state) => state.storage(),
            _ => 0
        }
    }

    pub fn paint(&self, canvas: &mut Canvas)
    {
        let _ = self.paint_clipped(canvas);
    }

    pub fn paint_clipped(&self, canvas: &mut Canvas) -> Result<(), Status>
    {
        let mut bounds = self.bounds();
        bounds.offset(canvas.origin().h, canvas.origin().v);
        let mut clip = Rect::ZERO;
        if !crate::toolbox::quickdraw::geometry::sect_rect(bounds, canvas.clip(), &mut clip)
        {
            return Ok(());
        }
        let previous = canvas.clip();
        clip.offset(-canvas.origin().h, -canvas.origin().v);
        canvas.set_clip(clip);
        let result = self.paint_checked(canvas);
        let mut restore = previous;
        restore.offset(-canvas.origin().h, -canvas.origin().v);
        canvas.set_clip(restore);
        result
    }

    fn paint_checked(&self, canvas: &mut Canvas) -> Result<(), Status>
    {
        let mut surface = Surface::from_canvas(canvas);
        let mut standard = self.state.standard();
        let mut scrollbar = match &self.state
        {
            State::Scrollbar(s) => Some(*s),
            _ => None
        };
        let (id, object) = if let Some(s) = standard.as_mut()
        {
            (standard_definition(self.variation)?, (s as *mut ControlDefinitionState).cast())
        }
        else
        {
            let Some(scrollbar) = scrollbar.as_mut()
            else
            {
                return Err(BAD_ABI);
            };
            (1, (scrollbar as *mut ScrollbarState).cast())
        };
        let mut call = DefinitionCall { header: Header::for_type::<DefinitionCall>(),
                                        kind: CDEF,
                                        id,
                                        variation: variation(self.variation, id),
                                        message: 0,
                                        reserved: 0,
                                        param: 0,
                                        object,
                                        gate: self.gate,
                                        surface: &mut surface };
        let result = super::resources::definitions::invoke(self.entry, &mut call);
        if result < 0 { Err(result as Status) } else { Ok(()) }
    }
}
fn standard_definition(procedure: i16) -> Result<i16, Status>
{
    match procedure
    {
        0..=2 | 8..=10 => Ok(0),
        PROGRESS_BAR_PROCEDURE | MONOCHROME_PROGRESS_BAR_PROCEDURE => Ok(CDEF_PROGRESS_BAR),
        STATIC_TEXT_PROCEDURE => Ok(CDEF_STATIC_TEXT),
        _ => Err(NOT_SUPPORTED)
    }
}
fn variation(procedure: i16, id: i16) -> i16
{
    if id == 0
    {
        procedure
    }
    else if procedure == MONOCHROME_PROGRESS_BAR_PROCEDURE
    {
        1
    }
    else
    {
        0
    }
}
pub fn definition_id(call: &ControlCall) -> Result<i16, Status>
{
    match call.operation
    {
        DRAW_SCROLLBAR | HIT_SCROLLBAR | TRACK_SCROLLBAR => Ok(1),
        DRAW_STANDARD | HIT_STANDARD => standard_definition(call.procedure),
        _ => Err(NOT_SUPPORTED)
    }
}
fn validate_state(call: &ControlCall, id: i16) -> Result<(), Status>
{
    if id == 1
    {
        let state = &call.scrollbar;
        if state.maximum < 0
           || state.value < 0
           || state.value > state.maximum
           || state.bounds.is_empty()
        {
            return Err(BAD_ARGUMENT);
        }
    }
    else
    {
        let state = &call.standard;
        if state.bounds.is_empty()
        {
            return Err(BAD_ARGUMENT);
        }
        let title = state.title.get(..state.title_length as usize).ok_or(BAD_ARGUMENT)?;
        core::str::from_utf8(title).map_err(|_| BAD_ARGUMENT)?;
    }
    Ok(())
}
/// Validate CDEF state without allocating a retained drawing.
pub fn validate(call: &mut ControlCall, entry: DefinitionEntry, gate: TrapGate)
                -> Result<(), Status>
{
    let id = definition_id(call)?;
    validate_state(call, id)?;
    let object = if id == 1
    {
        (&mut call.scrollbar as *mut ScrollbarState).cast()
    }
    else
    {
        (&mut call.standard as *mut ControlDefinitionState).cast()
    };
    let mut validation = DefinitionCall { header: Header::for_type::<DefinitionCall>(),
                                          kind: CDEF,
                                          id,
                                          variation: variation(call.procedure, id),
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
/// Validate and resolve a typed request. No per-control geometry lives here.
pub fn prepare(call: &mut ControlCall, entry: DefinitionEntry, gate: TrapGate)
               -> Result<Option<Drawing>, Status>
{
    prepare_with_title(call, entry, gate, None)
}
fn prepare_with_title(call: &mut ControlCall, entry: DefinitionEntry, gate: TrapGate,
                      title: Option<&Arc<snapshot::Title>>)
                      -> Result<Option<Drawing>, Status>
{
    let id = definition_id(call)?;
    if matches!(call.operation, DRAW_SCROLLBAR | DRAW_STANDARD)
    {
        validate(call, entry, gate)?;
        return Ok(Some(Drawing { id: 0,
                                 state: if id == 1
                                 {
                                     State::Scrollbar(call.scrollbar)
                                 }
                                 else if id == CDEF_PROGRESS_BAR
                                 {
                                     State::Progress { bounds:   call.standard.bounds,
                                                       progress: call.standard.progress }
                                 }
                                 else
                                 {
                                     State::Standard(snapshot::Standard::new(&call.standard,
                                                                             title)?)
                                 },
                                 entry,
                                 gate,
                                 variation: if id != 1 { call.procedure } else { 0 },
                                 damage: [if id == 1
                                          {
                                              call.scrollbar.bounds
                                          }
                                          else
                                          {
                                              call.standard.bounds
                                          },
                                          Rect::ZERO] }));
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
    if call.operation == TRACK_SCROLLBAR
    {
        bits |= ((call.anchor.h as u16 as u64) << 32) | ((call.anchor.v as u16 as u64) << 48);
    }
    let mut definition =
        DefinitionCall { header: Header::for_type::<DefinitionCall>(),
                         kind: CDEF,
                         id,
                         variation: variation(call.procedure, id),
                         message: if call.operation == TRACK_SCROLLBAR { 2 } else { 1 },
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
    pub unsafe fn dispatch(number: u32, pointer: *mut (), size: usize,
                           mut resolve: impl FnMut(i16) -> Result<DefinitionEntry, Status>,
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
            let id = definition_id(call)?;
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
