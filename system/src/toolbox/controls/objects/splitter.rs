//! One loaded Splitter CDEF supplies standalone and container drag geometry.
//! A capture retains its initial geometry; only its outline moves until release.
use super::*;
#[derive(Clone, Copy)]
pub(super) struct Capture
{
    initial:  ControlDefinitionState,
    entry:    DefinitionEntry,
    anchor:   Point,
    point:    Point,
    position: i16,
    clip:     Option<Rect>,
    boundary: Option<(u32, u32)>
}
impl Capture
{
    fn new(mut initial: ControlDefinitionState, entry: DefinitionEntry, anchor: Point,
           clip: Option<Rect>)
           -> Self
    {
        initial.value = if initial.flags & SPLITTER_HORIZONTAL != 0
        {
            initial.bounds.top
        }
        else
        {
            initial.bounds.left
        };
        Self { position: initial.value,
               initial,
               entry,
               anchor,
               point: anchor,
               clip,
               boundary: None }
    }

    fn move_to(&mut self, point: Point, gate: TrapGate) -> Result<(), Status>
    {
        if self.point == point
        {
            return Ok(());
        }
        let mut call = ControlCall { operation: TRACK_SPLITTER,
                                     procedure: SPLITTER_PROCEDURE,
                                     standard: self.initial,
                                     anchor: self.anchor,
                                     point,
                                     ..Default::default() };
        super::super::prepare(&mut call, self.entry, gate)?;
        if !(0..=65535).contains(&call.result)
        {
            return Err(BAD_ABI);
        }
        self.position = call.result as u16 as i16;
        self.point = point;
        Ok(())
    }

    fn outline(self) -> Result<Option<Rect>, Status>
    {
        let mut b = self.initial.bounds;
        if self.initial.flags & SPLITTER_PARENT != 0
        {
            let parent = self.initial.splitter_parent;
            if self.initial.flags & SPLITTER_HORIZONTAL != 0
            {
                b.left = parent.left;
                b.right = parent.right;
            }
            else
            {
                b.top = parent.top;
                b.bottom = parent.bottom;
            }
        }
        let delta = i32::from(self.position) - i32::from(self.initial.value);
        let translate = |v: i16| i16::try_from(i32::from(v) + delta).map_err(|_| BAD_ARGUMENT);
        let rect = if self.initial.flags & SPLITTER_HORIZONTAL != 0
        {
            Rect::new(b.left, translate(b.top)?, b.right, translate(b.bottom)?)
        }
        else
        {
            Rect::new(translate(b.left)?, b.top, translate(b.right)?, b.bottom)
        };
        let rect = self.clip.map_or(rect, |c| super::super::intersection(c, rect));
        Ok((!rect.is_empty()).then_some(rect))
    }
}

impl State
{
    pub(super) fn divider_hit(&self, point: Point, gate: TrapGate)
                              -> Result<Option<super::super::snapshot::Divider>, Status>
    {
        let Some(l) = &self.layout
        else
        {
            return Ok(None);
        };
        if l.clip().is_some_and(|c| !c.contains(point))
        {
            return Ok(None);
        }
        let Some(d) = l.outlines.as_ref().and_then(|g| g.dividers.as_ref())
        else
        {
            return Ok(None);
        };
        let enabled = matches!(self.spec,Spec::Standard {state,..} if state.enabled!=0);
        for segment in &d.segments
        {
            if !segment.bounds.contains(point)
            {
                continue;
            }
            let mut call = ControlCall { operation: HIT_STANDARD,
                                         procedure: SPLITTER_PROCEDURE,
                                         reflow: 1,
                                         standard: segment.state(d.viewport, enabled),
                                         point,
                                         ..Default::default() };
            super::super::prepare(&mut call, d.entry, gate)?;
            if call.result == i32::from(SPLITTER_PART)
            {
                return Ok(Some(*segment));
            }
        }
        Ok(None)
    }
}
impl Manager
{
    pub(crate) fn splitter_request(&self, call: &ControlCall, owner: u64, gate: TrapGate)
                                   -> Result<bool, Status>
    {
        let o = self.get(call.id, owner)?;
        if o.state.splitter.is_some()
        {
            return Ok(true);
        }
        if matches!(o.state.spec,
                    Spec::Standard { procedure: SPLITTER_PROCEDURE,
                                     .. })
        {
            return Ok(true);
        }
        Ok(call.operation == BEGIN_TRACKING
           && self.visible_clip(call.id).is_none_or(|c| c.contains(call.point))
           && o.state.divider_hit(call.point, gate)?.is_some())
    }

    pub(crate) fn splitter_capture(&self, id: u64, owner: u64) -> Result<bool, Status>
    {
        Ok(self.get(id, owner)?.state.splitter.is_some())
    }

    pub(crate) fn window_splitter_capture(&self, window: u64) -> bool
    {
        self.objects.iter().any(|(_, o)| o.window == window && o.state.splitter.is_some())
    }

    pub(crate) fn prepare_splitter(&self, call: &mut ControlCall, owner: u64, paint_pending: bool,
                                   gate: TrapGate,
                                   mut resolve: impl FnMut(i16) -> Result<DefinitionEntry, Status>)
                                   -> Result<Prepared, Status>
    {
        let root = self.get(call.id, owner)?;
        if call.window != 0 && call.window != root.window
        {
            return Err(BAD_ARGUMENT);
        }
        if paint_pending
        {
            return Err(SERVICE_BUSY);
        }
        if call.operation == BEGIN_TRACKING
           && self.objects.iter().any(|(_, o)| o.recipient == owner && o.state.tracking.is_some())
        {
            return Err(SERVICE_BUSY);
        }
        let table = matches!(root.state.spec,
                             Spec::Standard { procedure: TABLE_PROCEDURE,
                                              .. });
        let mut prepared = if table
                              && call.operation == BEGIN_TRACKING
                              && root.state.layout.as_ref().is_some_and(|l| l.arrangement_pending)
        {
            let assignment=ControlCall {id:call.id,operation:REFLOW_OBJECT,layout:mac_abi::controls::reflow::Payload {
                message:mac_abi::controls::reflow::ASSIGN_RECT,rect:root.state.spec.bounds(),..Default::default()},..Default::default()};
            self.prepare_layout(&assignment, owner, false, gate, &mut resolve, None)?
        }
        else
        {
            Prepared::empty()
        };
        let mut next = prepared.state(call.id).unwrap_or(&root.state).clone();
        let mut handled = false;
        let mut action = false;
        let mut feedback = None;
        let mut position = None;
        match call.operation
        {
            BEGIN_TRACKING =>
            {
                let clip = tree::clip(self.inherited_clip(call.id),
                                      next.layout.as_ref().and_then(Layout::clip));
                if !clip.is_some_and(|c| !c.contains(call.point))
                {
                    let capture = if table
                    {
                        if let Some(segment) = next.divider_hit(call.point, gate)?
                        {
                            let d = next.layout
                                        .as_ref()
                                        .and_then(|l| l.outlines.as_ref())
                                        .and_then(|g| g.dividers.as_ref())
                                        .ok_or(CORRUPT_DATA)?;
                            let mut capture = Capture::new(segment.state(d.viewport, true),
                                                           d.entry,
                                                           call.point,
                                                           tree::clip(clip, Some(d.viewport)));
                            capture.boundary = Some((segment.axis, segment.index));
                            Some(capture)
                        }
                        else
                        {
                            None
                        }
                    }
                    else
                    {
                        let Spec::Standard { state: spec,
                                             procedure: SPLITTER_PROCEDURE } = next.spec
                        else
                        {
                            return Err(NOT_SUPPORTED);
                        };
                        let entry = resolve(CDEF_SPLITTER)?;
                        if next.hit(call.point, entry, gate)? == SPLITTER_PART
                        {
                            Some(Capture::new(spec, entry, call.point, clip))
                        }
                        else
                        {
                            None
                        }
                    };
                    if let Some(capture) = capture
                    {
                        position = Some(capture.position);
                        feedback = Some(capture.outline()?);
                        next.splitter = Some(capture);
                        next.tracking = Some(Tracking::new(SPLITTER_PART, call.point,
                                                           capture.position, call.ticks));
                        handled = true;
                    }
                }
            }
            POLL_TRACKING =>
            {
                if let Some(mut capture) = next.splitter
                {
                    let old = capture.outline()?;
                    capture.move_to(call.point, gate)?;
                    position = Some(capture.position);
                    handled = true;
                    if call.flags & BUTTON_DOWN != 0
                    {
                        let new = capture.outline()?;
                        if old != new
                        {
                            feedback = Some(new);
                        }
                        next.splitter = Some(capture);
                    }
                    else
                    {
                        next.splitter = None;
                        next.tracking = None;
                        feedback = Some(None);
                        action = true;
                        if let Some((axis, index)) = capture.boundary
                        {
                            let delta =
                                i64::from(capture.position) - i64::from(capture.initial.value);
                            if delta != 0
                            {
                                prepared = self.prepare_split_release(call.id, owner, next, axis,
                                                                      index, delta, gate,
                                                                      &mut resolve)?;
                                next = prepared.state(call.id).ok_or(CORRUPT_DATA)?.clone();
                            }
                        }
                        else if let Spec::Standard { state,
                                                       .. } = &mut next.spec
                        {
                            state.value = capture.position;
                        }
                    }
                }
            }
            _ => return Err(NOT_SUPPORTED)
        }
        call.window = root.window;
        call.relation = root.tree.relation();
        next.spec.write(call);
        if let Some(value) = position
        {
            call.standard.value = value;
        }
        call.reflow = u32::from(next.layout.is_some());
        call.layout.anchors = next.layout.as_ref().map_or(0, |l| l.anchors.into());
        call.layout.window_border =
            next.layout.as_ref().map_or(Default::default(), |l| l.window_border.into());
        call.layout.invalidation = next.layout.as_ref().map_or(0, Layout::pending);
        call.layout.status = self.layout_status(call.id);
        call.result = if handled { i32::from(SPLITTER_PART) } else { 0 };
        call.flags = u32::from(handled) * HANDLED
                     | u32::from(action) * ACTION
                     | u32::from(next.tracking.is_some()) * TRACKING;
        prepared.set_state(call.id, next);
        prepared.feedback = feedback;
        Ok(prepared)
    }
}

impl Manager
{
    /// Hover uses the accepted, rectangular divider geometry. Covered controls
    /// and clipped/hidden children cannot select a resize cursor through a pane.
    pub(crate) fn splitter_axis_at(&self, window: u64, point: Point, hover: bool) -> Option<usize>
    {
        let mut front = None;
        for (id, o) in self.objects.iter().filter(|(_, o)| o.window == window)
        {
            if let Some(capture) = o.state.splitter
            {
                return Some(usize::from(capture.initial.flags & SPLITTER_HORIZONTAL != 0));
            }
            if !hover
            {
                continue;
            }
            if !o.state.spec.bounds().contains(point)
               || self.visible_clip(id).is_some_and(|c| !c.contains(point))
            {
                continue;
            }
            if front.is_none_or(|old| self.paint_order(id, old).is_gt())
            {
                front = Some(id);
            }
        }
        let o = self.objects.get(front?)?;
        let Spec::Standard { state,
                             procedure } = o.state.spec
        else
        {
            return None;
        };
        if state.enabled == 0
        {
            return None;
        }
        if procedure == SPLITTER_PROCEDURE
        {
            return Some(usize::from(state.flags & SPLITTER_HORIZONTAL != 0));
        }
        o.state
         .layout
         .as_ref()?
         .outlines
         .as_ref()?
         .dividers
         .as_ref()?
         .segments
         .iter()
         .find(|s| s.bounds.contains(point))
         .map(|s| s.axis as usize)
    }
}
