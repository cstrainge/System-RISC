//! Resident Control Manager. Each CDEF owns drawing, hit geometry and tracking math.
mod snapshot;
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
/// Validate CDEF state without allocating a retained drawing.
pub fn validate(call: &mut ControlCall, entry: DefinitionEntry, gate: TrapGate)
                -> Result<(), Status>
{
    let id = definition_id(call)?;
    if id == 1
       && (call.scrollbar.maximum < 0
           || call.scrollbar.value < 0
           || call.scrollbar.value > call.scrollbar.maximum
           || call.scrollbar.bounds.is_empty())
    {
        return Err(BAD_ARGUMENT);
    }
    if id != 1 && (call.standard.bounds.is_empty()
        || call.standard.title_length as usize > call.standard.title.len()
        || core::str::from_utf8(&call.standard.title[..(call.standard.title_length as usize).min(call.standard.title.len())]).is_err()) {
        return Err(BAD_ARGUMENT);
    }
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
    if id == 1
    {
        if call.scrollbar.maximum < 0
           || call.scrollbar.value < 0
           || call.scrollbar.value > call.scrollbar.maximum
           || call.scrollbar.bounds.is_empty()
        {
            return Err(BAD_ARGUMENT);
        }
    }
    else
    {
        let s = &call.standard;
        let length = s.title_length as usize;
        if s.bounds.is_empty()
           || length > s.title.len()
           || core::str::from_utf8(&s.title[..length]).is_err()
        {
            return Err(BAD_ARGUMENT);
        }
    }
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
#[cfg(test)]
mod tests
{
    use alloc::vec;

    use mac_abi::{Point, Rect};

    use super::*;
    // This replay uses an offscreen surface rather than a process service table.
    // Definitions still query the same resident theme used by live controls.
    unsafe extern "C" fn definition_services(number: u32, request: *mut (), size: usize) -> i64
    {
        if number == mac_abi::traps::THEME
        {
            if request.is_null()
               || size != core::mem::size_of::<mac_abi::theme::Palette>()
               || !(request as usize).is_multiple_of(core::mem::align_of::<mac_abi::theme::Palette>())
            {
                return BAD_ARGUMENT as i64;
            }
            unsafe {
                request.cast::<mac_abi::theme::Palette>().write(crate::toolbox::theme::palette())
            };
            return OK as i64;
        }
        unsafe { crate::toolbox::quickdraw::surface_gate(number, request, size) }
    }
    #[test]
    fn resource_replay_preserves_window_origin_clip_and_global_track_phase()
    {
        let state = ScrollbarState { bounds: Rect::new(8, 10, 24, 90),
                                     value: 50,
                                     maximum: 100,
                                     active: 1,
                                     ..Default::default() };
        let drawing = Drawing { id:        0,
                                state:     State::Scrollbar(state),
                                variation: 0,
                                entry:     crate::test_scrollbar::scrollbar_definition,
                                gate:      definition_services,
                                damage:    [state.bounds, Rect::ZERO] };
        let background = 0xff00ff;
        let mut full = vec![background; 80 * 100];
        let mut clipped = full.clone();
        let clip = Rect::new(10, 15, 50, 50);
        for (pixels, limited) in [(&mut full, false), (&mut clipped, true)]
        {
            let mut canvas = Canvas::new(pixels, 80, 100);
            canvas.set_origin(5, 7);
            if limited
            {
                let mut local_clip = clip;
                local_clip.offset(-5, -7);
                canvas.set_clip(local_clip);
            }
            drawing.paint(&mut canvas);
        }
        assert!(full.iter().any(|&p| p != background));
        for y in 0..100
        {
            for x in 0..80
            {
                let index = y * 80 + x;
                assert_eq!(clipped[index],
                           if clip.contains(Point { h: x as i16,
                                                    v: y as i16 })
                           {
                               full[index]
                           }
                           else
                           {
                               background
                           });
            }
        }
    }
}

#[cfg(test)]
mod sdk_tests
{
    use mac_abi::{controls::*, *};
    use toolbox_controls::{Client, Controls};
    unsafe extern "C" fn gate(number: u32, request: *mut (), size: usize) -> i64
    {
        assert_eq!(number, mac_abi::traps::CONTROL);
        assert_eq!(size, core::mem::size_of::<ControlCall>());
        let call = unsafe { &mut *request.cast::<ControlCall>() };
        assert_eq!(call.window, 42);
        assert_eq!(call.scrollbar.bounds, Rect::new(10, 20, 26, 220));
        assert_eq!(call.scrollbar.value, 25);
        assert_eq!(call.scrollbar.maximum, 100);
        assert_eq!(call.scrollbar.active, 1);
        match call.operation
        {
            DRAW_SCROLLBAR =>
            {}
            HIT_SCROLLBAR =>
            {
                assert_eq!(call.point,
                           Point { h: 18,
                                   v: 30 });
                call.result = SCROLL_UP as i32;
            }
            TRACK_SCROLLBAR =>
            {
                assert_eq!(call.anchor,
                           Point { h: 18,
                                   v: 30 });
                assert_eq!(call.point,
                           Point { h: 18,
                                   v: 80 });
                call.result = 63;
            }
            _ => return NOT_SUPPORTED as i64
        };
        OK as i64
    }
    #[test]
    fn typed_scrollbar_sdk_sends_only_scalar_control_state()
    {
        let services = SystemServices { header:       Header::for_type::<SystemServices>(),
                                        trap:         gate,
                                        capabilities: 0 };
        let client = Client::new(&services).unwrap();
        let controls = Controls::new(client);
        controls.draw_scrollbar(WindowId(42), Rect::new(10, 20, 26, 220), 25, 100, true).unwrap();
        assert_eq!(controls.scrollbar_hit(WindowId(42),
                                          Rect::new(10, 20, 26, 220),
                                          25,
                                          100,
                                          true,
                                          Point { h: 18,
                                                  v: 30 })
                           .unwrap(),
                   toolbox_controls::Part::Up);
        assert_eq!(controls.scrollbar_track_value(WindowId(42),
                                                  Rect::new(10, 20, 26, 220),
                                                  25,
                                                  100,
                                                  Point { h: 18,
                                                          v: 30 },
                                                  Point { h: 18,
                                                          v: 80 })
                           .unwrap(),
                   63);
    }
}

pub mod scrollbox;
pub mod scrollbox_native;
pub mod objects;
