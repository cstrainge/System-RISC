//! Ordered retained CDEF calls. Plans are source/geometry snapshots, never
//! canonical manager admission, callback completion or scratch-account authority.
#![forbid(unsafe_code)]
pub(crate) mod producer;

use alloc::sync::Arc;

use mac_abi::{BAD_ABI, BAD_ARGUMENT, DefinitionCall, Header, SERVICE_BUSY, Status, Surface,
              TrapGate,
              controls::{CDEF_SPLITTER, ScrollbarState, TABLE_LINE_FLAGS},
              definitions::{CDEF, ControlDefinitionState},
              geometry::{Point, Rect}};

use super::{Drawing, State, intersection, snapshot, standard_definition, variation};
use crate::toolbox::{quickdraw::{Canvas, SurfaceExt},
                     resources::definitions::{self, BoundDefinition, DefinitionRef,
                                              RetainedDefinition,
                                              callbacks::packet::{Arguments, Object, SurfacePlan}}};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase
{
    Body,
    Bar(usize),
    Corner,
    Divider(usize),
    Complete
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Geometry
{
    width:  usize,
    height: usize,
    origin: Point,
    clip:   Rect
}
impl Geometry
{
    fn of(canvas: &Canvas<'_>) -> Self
    {
        Self { width:  canvas.width,
               height: canvas.height,
               origin: canvas.origin(),
               clip:   canvas.clip() }
    }
}
/// These real handle values are correlation hints, explicitly not a managed
/// completion key, revision seal or permission to invoke an author.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnclaimedSubject
{
    Stateless,
    Control(u64),
    ScrollBox(u64)
}
/// Original producer family, independent of correlation hints and the current
/// mutable-object readset. No variant can be minted from a raw author id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaintOrigin
{
    Stateless,
    Managed(super::objects::ManagedPaintOrigin),
    ScrollBox(super::scrollbox::ScrollBoxPaintOrigin)
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct WaitKey
{
    phase:    Phase,
    source:   DefinitionRef,
    subject:  UnclaimedSubject,
    origin:   PaintOrigin,
    geometry: Geometry
}

/// Affine position only. This does not acknowledge an author or release a claim.
pub(crate) struct Resume
{
    key: WaitKey
}
pub(crate) struct Cursor
{
    phase:   Phase,
    waiting: Option<WaitKey>
}
impl Default for Cursor
{
    fn default() -> Self
    {
        Self { phase:   Phase::Body,
               waiting: None }
    }
}
impl Cursor
{
    /// Call only after actual canonical replay/claim validation. The scalar
    /// token cannot manufacture callback completion or object admission.
    pub(crate) fn advance_user(&mut self, resume: Resume) -> Result<(), Status>
    {
        if self.waiting != Some(resume.key) || self.phase != resume.key.phase
        {
            return Err(BAD_ABI);
        }
        self.next()?;
        self.waiting = None;
        Ok(())
    }

    fn next(&mut self) -> Result<(), Status>
    {
        self.phase = match self.phase
        {
            Phase::Body => Phase::Bar(0),
            Phase::Bar(i) => Phase::Bar(i.checked_add(1).ok_or(BAD_ABI)?),
            Phase::Corner => Phase::Divider(0),
            Phase::Divider(i) => Phase::Divider(i.checked_add(1).ok_or(BAD_ABI)?),
            Phase::Complete => return Err(BAD_ABI)
        };
        Ok(())
    }
}
#[derive(Clone, Copy)]
enum Input
{
    Control(ControlDefinitionState),
    Scrollbar(ScrollbarState)
}
impl Input
{
    fn object(self) -> Object
    {
        match self
        {
            Self::Control(v) => Object::Control(v),
            Self::Scrollbar(v) => Object::Scrollbar(v)
        }
    }
}
/// An owned original input for the future canonical producer. No Canvas,
/// application pointer, Pending, account selector or fake CTRL seal is stored.
pub(crate) struct PaintPlan
{
    subject:    UnclaimedSubject,
    origin:     PaintOrigin,
    input:      Input,
    id:         i16,
    variation:  i16,
    reflow:     u32,
    grid:       Option<Arc<snapshot::Grid>>,
    surface:    SurfacePlan,
    geometry:   Geometry,
    resume:     Option<Resume>,
    // Release all owned snapshot payloads before the original source reader.
    source:     RetainedDefinition,
    // Source and original raster readers drop BEFORE the real Session Activity.
    scroll_box: Option<super::scrollbox::ScrollBoxPaintSnapshot>
}
impl PaintPlan
{
    pub(crate) fn origin(&self) -> PaintOrigin
    {
        self.origin
    }

    /// Only a genuinely published SCBX Drawing can supply this original scope.
    pub(crate) fn with_scroll_box_scope<R>(&self, operation: impl FnOnce() -> R)
                                           -> Result<R, Status>
    {
        let snapshot = self.scroll_box.as_ref().ok_or(BAD_ABI)?;
        if self.origin != PaintOrigin::ScrollBox(snapshot.origin())
        {
            return Err(BAD_ABI);
        }
        snapshot.with_scope(operation)
    }

    pub(crate) fn source(&self) -> &RetainedDefinition
    {
        &self.source
    }

    pub(crate) fn subject(&self) -> UnclaimedSubject
    {
        self.subject
    }

    pub(crate) fn id(&self) -> i16
    {
        self.id
    }

    pub(crate) fn variation(&self) -> i16
    {
        self.variation
    }

    pub(crate) fn reflow(&self) -> u32
    {
        self.reflow
    }

    pub(crate) fn object(&self) -> Object
    {
        self.input.object()
    }

    pub(crate) fn surface(&self) -> SurfacePlan
    {
        self.surface
    }

    /// Dynamic grid admission belongs outside Desktop, under the actual
    /// storage authority retained by the canonical producer, never code owner.
    pub(crate) fn arguments(&self) -> Result<Arguments, Status>
    {
        let Some(grid) = &self.grid
        else
        {
            return Ok(Arguments::Scalar(0));
        };
        let mut cells = alloc::vec::Vec::new();
        cells.try_reserve_exact(grid.cells.len()).map_err(|_| mac_abi::NO_MEMORY)?;
        cells.extend_from_slice(&grid.cells);
        Ok(Arguments::GridPaint(cells))
    }

    pub(crate) fn take_resume(&mut self) -> Result<Resume, Status>
    {
        self.resume.take().ok_or(BAD_ABI)
    }

    pub(super) fn scroll_box_subject(&mut self, cursor: &mut Cursor,
                                     drawing: &super::scrollbox::Drawing)
                                     -> Result<(), Status>
    {
        let origin = drawing.paint_origin();
        if self.origin != PaintOrigin::Stateless
           || self.scroll_box.is_some()
           || self.subject != UnclaimedSubject::Stateless
           || self.id != 1
           || self.reflow != 0
           || !matches!(self.input, Input::Scrollbar(_))
           || drawing.id != origin.scroll_box_id()
           || !origin.source_matches(&self.source)
        {
            return Err(BAD_ABI);
        }
        let resume = self.resume.as_mut().ok_or(BAD_ABI)?;
        if cursor.waiting != Some(resume.key)
        {
            return Err(BAD_ABI);
        }
        // All fallible checks precede these same-producer scalar publications
        // and infallible original reader clones. No owned refusal is dropped.
        self.subject = UnclaimedSubject::ScrollBox(origin.scroll_box_id());
        self.origin = PaintOrigin::ScrollBox(origin);
        resume.key.subject = self.subject;
        resume.key.origin = self.origin;
        cursor.waiting = Some(resume.key);
        self.scroll_box = Some(drawing.paint_snapshot());
        Ok(())
    }

    /// Recreate the exact fragment/subcall Canvas value geometry under Desktop.
    /// Root and the actual origin manager must validate their readsets first.
    pub(crate) fn with_canvas<R>(&self, canvas: &mut Canvas<'_>,
                                 apply: impl FnOnce(&mut Canvas<'_>) -> R)
                                 -> Result<R, Status>
    {
        let g = self.geometry;
        let outer = Geometry::of(canvas);
        if outer.width != g.width
           || outer.height != g.height
           || outer.origin != g.origin
           || intersection(g.clip, outer.clip) != g.clip
        {
            return Err(BAD_ARGUMENT);
        }
        set_global_clip(canvas, g.clip);
        let result = apply(canvas);
        canvas.set_origin(0, 0);
        canvas.set_clip(outer.clip);
        canvas.set_origin(outer.origin.h, outer.origin.v);
        Ok(result)
    }
}
pub(crate) enum Progress
{
    Complete,
    User(PaintPlan)
}
enum Source<'a>
{
    Retained(&'a RetainedDefinition),
    Bound(&'a BoundDefinition)
}
impl Source<'_>
{
    fn reference(&self) -> DefinitionRef
    {
        match self
        {
            Self::Retained(v) => v.reference(),
            Self::Bound(v) => v.reference()
        }
    }

    fn user(&self) -> bool
    {
        match self
        {
            Self::Retained(v) => v.is_process_source(),
            Self::Bound(v) => v.is_process_source()
        }
    }

    fn retain(&self) -> RetainedDefinition
    {
        match self
        {
            Self::Retained(v) => RetainedDefinition::clone(v),
            Self::Bound(v) => v.retain_source()
        }
    }
}
struct Call<'a>
{
    source:    Source<'a>,
    input:     Input,
    id:        i16,
    variation: i16,
    reflow:    u32,
    grid:      Option<&'a Arc<snapshot::Grid>>,
    gate:      TrapGate
}
impl Call<'_>
{
    fn visit(&self, cursor: &mut Cursor, canvas: &mut Canvas<'_>, staged: bool,
             subject: UnclaimedSubject, origin: PaintOrigin)
             -> Result<Option<PaintPlan>, Status>
    {
        if self.source.user()
        {
            if !staged
            {
                return Err(SERVICE_BUSY);
            }
            let geometry = Geometry::of(canvas);
            let o = geometry.origin;
            let c = geometry.clip;
            let local = Rect::new(c.left.checked_sub(o.h).ok_or(BAD_ARGUMENT)?,
                                  c.top.checked_sub(o.v).ok_or(BAD_ARGUMENT)?,
                                  c.right.checked_sub(o.h).ok_or(BAD_ARGUMENT)?,
                                  c.bottom.checked_sub(o.v).ok_or(BAD_ARGUMENT)?);
            let surface = SurfacePlan::new(canvas, local)?;
            let key = WaitKey { phase: cursor.phase,
                                source: self.source.reference(),
                                subject,
                                origin,
                                geometry };
            cursor.waiting = Some(key);
            return Ok(Some(PaintPlan { source: self.source.retain(),
                                       subject,
                                       origin,
                                       input: self.input,
                                       id: self.id,
                                       variation: self.variation,
                                       reflow: self.reflow,
                                       grid: self.grid.cloned(),
                                       surface,
                                       geometry,
                                       resume: Some(Resume { key }),
                                       scroll_box: None }));
        }
        // This is the original audited synchronous definition ABI boundary.
        // User sources cannot reach it, and no pointer escapes this call.
        let mut state = self.input;
        let object = match &mut state
        {
            Input::Control(v) => (v as *mut ControlDefinitionState).cast(),
            Input::Scrollbar(v) => (v as *mut ScrollbarState).cast()
        };
        let grid = self.grid.map(|g| {
                                mac_abi::controls::reflow::GridPaint { cells: g.cells.as_ptr(),
                                                                       count: g.cells.len() }
                            });
        let mut surface = Surface::from_canvas(canvas);
        let mut call =
            DefinitionCall { header: Header::for_type::<DefinitionCall>(),
                             kind: CDEF,
                             id: self.id,
                             variation: self.variation,
                             message: 0,
                             reserved: 0,
                             reflow: self.reflow,
                             param:
                                 grid.as_ref().map_or(0, |g| {
                                                  (g as *const mac_abi::controls::reflow::GridPaint)
                                                  as usize
                                                  as i64
                                              }),
                             object,
                             gate: self.gate,
                             surface: &mut surface };
        let result = definitions::invoke(self.source.reference(), &mut call);
        if result < 0
        {
            return Err(result as Status);
        }
        Ok(None)
    }
}
fn set_global_clip(canvas: &mut Canvas<'_>, clip: Rect)
{
    let origin = canvas.origin();
    canvas.set_origin(0, 0);
    canvas.set_clip(clip);
    canvas.set_origin(origin.h, origin.v);
}
fn clipped<R>(canvas: &mut Canvas<'_>, local: Rect,
              apply: impl FnOnce(&mut Canvas<'_>) -> Result<R, Status>)
              -> Result<Option<R>, Status>
{
    let previous = canvas.clip();
    let origin = canvas.origin();
    let mut bounds = local;
    bounds.offset(origin.h, origin.v);
    let clip = intersection(previous, bounds);
    if clip.is_empty()
    {
        return Ok(None);
    }
    set_global_clip(canvas, clip);
    let result = apply(canvas);
    canvas.set_origin(0, 0);
    canvas.set_clip(previous);
    canvas.set_origin(origin.h, origin.v);
    result.map(Some)
}
fn body<'a>(drawing: &'a Drawing, bounds: Option<Rect>, corner: bool) -> Result<Call<'a>, Status>
{
    let (input, id) = match &drawing.state
    {
        State::Scrollbar(v) => (Input::Scrollbar(*v), 1),
        _ =>
        {
            let mut v = drawing.state.standard().ok_or(BAD_ABI)?;
            if let Some(bounds) = bounds
            {
                v.bounds = bounds;
            }
            if corner
            {
                v.flags = TABLE_LINE_FLAGS;
            }
            (Input::Control(v), standard_definition(drawing.variation)?)
        }
    };
    Ok(Call { source: Source::Retained(&drawing.source),
              input,
              id,
              variation: variation(drawing.variation, id),
              reflow: drawing.reflow,
              grid: drawing.grid.as_ref().filter(|_| !corner),
              gate: drawing.gate })
}
pub(crate) fn run(drawing: &Drawing, cursor: &mut Cursor, canvas: &mut Canvas<'_>, staged: bool)
                  -> Result<Progress, Status>
{
    if cursor.waiting.is_some()
    {
        return Err(SERVICE_BUSY);
    }
    let managed = drawing.managed_origin();
    if managed.is_some_and(|held| held.control_id() != drawing.id)
    {
        return Err(BAD_ABI);
    }
    let origin = managed.map_or(PaintOrigin::Stateless, PaintOrigin::Managed);
    let subject = if drawing.id == 0
    {
        UnclaimedSubject::Stateless
    }
    else
    {
        UnclaimedSubject::Control(drawing.id)
    };
    let enabled = drawing.grid.as_ref().and_then(|grid| grid.dividers.as_ref()).is_some()
                  && drawing.state.standard().is_some_and(|state| state.enabled != 0);
    let result = clipped(canvas, drawing.visible_bounds(), |canvas| {
        loop
        {
            let plan = match cursor.phase
            {
                Phase::Body =>
                {
                    let viewport = drawing.bars.as_ref().map(|bars| bars.viewport);
                    let call = body(drawing, viewport, false)?;
                    if let Some(viewport) = viewport
                    {
                        clipped(canvas, viewport, |canvas| {
                            call.visit(cursor, canvas, staged, subject, origin)
                        })?.flatten()
                    }
                    else
                    {
                        call.visit(cursor, canvas, staged, subject, origin)?
                    }
                }
                Phase::Bar(index) =>
                {
                    let Some(bars) = &drawing.bars
                    else
                    {
                        cursor.phase = Phase::Corner;
                        continue;
                    };
                    let Some(state) = bars.bars.get(index)
                    else
                    {
                        cursor.phase = Phase::Corner;
                        continue;
                    };
                    let Some(state) = state
                    else
                    {
                        cursor.next()?;
                        continue;
                    };
                    let call = Call { source:    Source::Bound(bars.definition.as_ref()),
                                      input:     Input::Scrollbar(*state),
                                      id:        1,
                                      variation: 0,
                                      reflow:    1,
                                      grid:      None,
                                      gate:      drawing.gate };
                    clipped(canvas, state.bounds, |canvas| {
                        call.visit(cursor, canvas, staged, subject, origin)
                    })?.flatten()
                }
                Phase::Corner =>
                {
                    let Some(bars) =
                        drawing.bars.as_ref().filter(|bars| bars.bars.iter().all(Option::is_some))
                    else
                    {
                        cursor.phase = Phase::Divider(0);
                        continue;
                    };
                    let bounds = drawing.bounds();
                    let corner = Rect::new(bars.viewport.right, bars.viewport.bottom,
                                           bounds.right, bounds.bottom);
                    let call = body(drawing, Some(corner), true)?;
                    clipped(canvas, corner, |canvas| {
                        call.visit(cursor, canvas, staged, subject, origin)
                    })?.flatten()
                }
                Phase::Divider(index) =>
                {
                    let Some(dividers) = drawing.grid.as_ref().and_then(|g| g.dividers.as_ref())
                    else
                    {
                        cursor.phase = Phase::Complete;
                        continue;
                    };
                    let Some(divider) = dividers.segments.get(index)
                    else
                    {
                        cursor.phase = Phase::Complete;
                        continue;
                    };
                    let call = Call { source:    Source::Bound(dividers.definition.as_ref()),
                                      input:     Input::Control(divider.state(dividers.viewport,
                                                                              enabled)),
                                      id:        CDEF_SPLITTER,
                                      variation: 0,
                                      reflow:    1,
                                      grid:      None,
                                      gate:      drawing.gate };
                    call.visit(cursor, canvas, staged, subject, origin)?
                }
                Phase::Complete => return Ok(Progress::Complete)
            };
            if let Some(plan) = plan
            {
                return Ok(Progress::User(plan));
            }
            cursor.next()?;
        }
    })?;
    Ok(result.unwrap_or(Progress::Complete))
}
