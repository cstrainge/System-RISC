//! One loaded Splitter CDEF supplies standalone and container drag geometry.
//! A capture retains its initial geometry; only its outline moves until release.
use super::*;
use crate::toolbox::runtime::TrackingStage;
#[derive(Clone)]
pub(super) struct Capture
{
    initial:     ControlDefinitionState,
    entry:       DefinitionRef,
    anchor:      Point,
    point:       Point,
    position:    i16,
    clip:        Option<Rect>,
    boundary:    Option<(u32, u32)>,
    // Standalone controls retain their own binding. Table captures share the
    // actual divider dependency across later layout snapshot replacement.
    _definition: Option<Arc<BoundDefinition>>
}
impl Capture
{
    pub(super) fn same_snapshot(&self, other: &Self) -> bool
    {
        self.initial == other.initial
        && self.entry == other.entry
        && self.anchor == other.anchor
        && self.point == other.point
        && self.position == other.position
        && self.clip == other.clip
        && self.boundary == other.boundary
        && match (&self._definition, &other._definition)
        {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false
        }
    }

    pub(super) fn has_process_source(&self) -> bool
    {
        self._definition.as_ref().is_some_and(|source| source.is_process_source())
    }

    fn new(mut initial: ControlDefinitionState, entry: DefinitionRef, anchor: Point,
           clip: Option<Rect>, definition: Option<Arc<BoundDefinition>>)
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
               boundary: None,
               _definition: definition }
    }

    async fn move_to_with(&mut self, point: Point, author: &super::super::effects::Author)
                          -> Result<(), Status>
    {
        if self.point == point
        {
            return Ok(());
        }
        let call =
            super::super::effects::DetachedCall::capture(&ControlCall { operation:
                                                                            TRACK_SPLITTER,
                                                                        procedure:
                                                                            SPLITTER_PROCEDURE,
                                                                        standard: self.initial,
                                                                        anchor: self.anchor,
                                                                        point,
                                                                        ..Default::default() })?;
        let (call, _) = author.prepare(call, self.entry, None).await?;
        if !(0..=65535).contains(&call.result)
        {
            return Err(BAD_ABI);
        }
        self.position = call.result as u16 as i16;
        self.point = point;
        Ok(())
    }

    fn outline(&self) -> Result<Option<Rect>, Status>
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
        let author = super::super::effects::Author::trusted(gate);
        super::super::effects::immediate(self.divider_hit_with(point, &author))
    }

    pub(super) async fn divider_hit_with(
        &self, point: Point, author: &super::super::effects::Author)
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
            let call = super::super::effects::DetachedCall::capture(&ControlCall { operation: HIT_STANDARD,
                                         procedure: SPLITTER_PROCEDURE,
                                         reflow: 1,
                                         standard: segment.state(d.viewport, enabled),
                                         point,
                                         ..Default::default() })?;
            let (call, _) = author.prepare(call, d.definition.reference(), None).await?;
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
        Ok(self.snapshot(id, owner)?.state.splitter.is_some())
    }

    pub(crate) fn window_splitter_capture(&self, window: u64) -> bool
    {
        self.objects.iter().any(|(_, o)| o.window == window && o.state.splitter.is_some())
    }

    pub(crate) fn prepare_splitter(&self, call: &mut ControlCall, owner: u64, paint_pending: bool,
                                   gate: TrapGate, diagnostic: &mut Option<TrackingStage>)
                                   -> Result<Prepared, Status>
    {
        let snapshot = self.snapshot_splitter(call, owner, paint_pending, diagnostic)?;
        let author = super::super::effects::Author::trusted(gate);
        let (prepared, output) =
            super::super::effects::immediate(snapshot.calculate(&author)).map_err(|status| {
                TrackingStage::refusal(diagnostic, TrackingStage::Calculate, status)
            })?;
        *call = output.wire();
        Ok(prepared)
    }

    pub(super) fn snapshot_splitter(&self, call: &ControlCall, owner: u64, paint_pending: bool,
                                    diagnostic: &mut Option<TrackingStage>)
                                    -> Result<SplitterSnapshot, Status>
    {
        let root = self.get(call.id, owner).map_err(|status| {
                                                TrackingStage::refusal(diagnostic,
                                                                TrackingStage::SnapshotLookup,
                                                                status)
                                            })?;
        if call.window != 0 && call.window != root.window
        {
            return Err(BAD_ARGUMENT);
        }
        if paint_pending
        {
            return Err(TrackingStage::refusal(diagnostic,
                                              TrackingStage::SnapshotPendingPaint,
                                              SERVICE_BUSY));
        }
        if call.operation == BEGIN_TRACKING
           && self.objects.iter().any(|(_, o)| o.recipient == owner && o.state.tracking.is_some())
        {
            return Err(TrackingStage::refusal(diagnostic,
                                              TrackingStage::SnapshotExistingCapture,
                                              SERVICE_BUSY));
        }
        let table = matches!(root.state.spec,
                             Spec::Standard { procedure: TABLE_PROCEDURE,
                                              .. });
        let arrange = if table
                         && call.operation == BEGIN_TRACKING
                         && root.state.layout.as_ref().is_some_and(|l| l.arrangement_pending)
        {
            let assignment=ControlCall {id:call.id,operation:REFLOW_OBJECT,layout:mac_abi::controls::reflow::Payload {
                message:mac_abi::controls::reflow::ASSIGN_RECT,rect:root.state.spec.bounds(),..Default::default()},..Default::default()};
            self.snapshot_layout_change(&assignment, owner, false, None, None, None)
                .map_err(|status| {
                    TrackingStage::refusal(diagnostic, TrackingStage::SnapshotArrangement, status)
                })?
        }
        else
        {
            grid::LayoutSnapshot::Ready(Prepared::empty())
        };
        let release = if table
                         && call.operation == POLL_TRACKING
                         && call.flags & BUTTON_DOWN == 0
                         && root.state
                                .splitter
                                .as_ref()
                                .is_some_and(|capture| capture.boundary.is_some())
        {
            // A releasing POLL ends this control's own capture before it assigns
            // the released geometry, so the plan must carry that post-release
            // state. A stale capture left in the plan makes the assignment walk
            // find this control tracking and refuse it as though some other
            // control were mid-gesture, which is what the walk is there to stop.
            let mut released = root.state.clone();
            released.tracking = None;
            released.splitter = None;
            let assignment=ControlCall{id:call.id,operation:REFLOW_OBJECT,
                layout:mac_abi::controls::reflow::Payload{message:mac_abi::controls::reflow::ASSIGN_RECT,
                    rect:root.state.spec.bounds(),..Default::default()},..Default::default()};
            Some(self.snapshot_layout_change(&assignment,
                                             owner,
                                             false,
                                             None,
                                             None,
                                             Some(released))
                     .map_err(|status| {
                         TrackingStage::refusal(diagnostic, TrackingStage::SnapshotRelease, status)
                     })?)
        }
        else
        {
            None
        };
        Ok(SplitterSnapshot { initial: root.state.clone(),
                              entry: root.definition.reference(),
                              window: root.window,
                              node: root.tree,
                              clip: self.inherited_clip(call.id),
                              status: self.layout_status(call.id),
                              table,
                              arrange,
                              release,
                              call: super::super::effects::DetachedCall::capture(call).map_err(|status| TrackingStage::refusal(diagnostic, TrackingStage::SnapshotCapture, status))? })
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
            if let Some(capture) = o.state.splitter.as_ref()
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

pub(super) struct SplitterSnapshot
{
    initial: State,
    entry:   DefinitionRef,
    window:  u64,
    node:    tree::Node,
    clip:    Option<Rect>,
    status:  Status,
    table:   bool,
    arrange: grid::LayoutSnapshot,
    release: Option<grid::LayoutSnapshot>,
    call:    super::super::effects::DetachedCall
}
impl SplitterSnapshot
{
    pub(super) async fn calculate(
        self, author: &super::super::effects::Author)
        -> Result<(Prepared, super::super::effects::DetachedCall), Status>
    {
        let Self { initial,
                   entry,
                   window,
                   node,
                   clip,
                   status,
                   table,
                   arrange,
                   mut release,
                   mut call } = self;
        let mut prepared = arrange.calculate(author).await?;
        let mut next =
            if let Some(state) = prepared.state(call.id) { state.clone() } else { initial };
        let mut handled = false;
        let mut action = false;
        let mut feedback = None;
        let mut position = None;
        match call.operation
        {
            BEGIN_TRACKING =>
            {
                let clip = tree::clip(clip, next.layout.as_ref().and_then(Layout::clip));
                if !clip.is_some_and(|c| !c.contains(call.point))
                {
                    let capture = if table
                    {
                        if let Some(segment) = next.divider_hit_with(call.point, author).await?
                        {
                            let d = next.layout
                                        .as_ref()
                                        .and_then(|l| l.outlines.as_ref())
                                        .and_then(|g| g.dividers.as_ref())
                                        .ok_or(CORRUPT_DATA)?;
                            let mut capture = Capture::new(segment.state(d.viewport, true),
                                                           d.definition.reference(),
                                                           call.point,
                                                           tree::clip(clip, Some(d.viewport)),
                                                           Some(d.definition.clone()));
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
                        if next.hit_with(call.point, entry, author).await? == SPLITTER_PART
                        {
                            Some(Capture::new(spec, entry, call.point, clip, None))
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
                        next.tracking = Some(Tracking::new(SPLITTER_PART, call.point,
                                                           capture.position, call.ticks));
                        next.splitter = Some(capture);
                        handled = true;
                    }
                }
            }
            POLL_TRACKING =>
            {
                if let Some(mut capture) = next.splitter.take()
                {
                    let old = capture.outline()?;
                    capture.move_to_with(call.point, author).await?;
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
                                let state = Manager::split_release_state(next, axis, index, delta)?;
                                let snapshot =
                                    release.take().ok_or(BAD_ABI)?.with_root_state(state)?;
                                prepared = snapshot.calculate(author).await?;
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
        call.window = window;
        call.relation = node.relation();
        {
            let mut wire = call.wire();
            next.spec.write(&mut wire);
            call = super::super::effects::DetachedCall::capture(&wire)?;
        }
        if let Some(value) = position
        {
            call.standard.value = value;
        }
        call.reflow = u32::from(next.layout.is_some());
        call.layout.anchors = next.layout.as_ref().map_or(0, |l| l.anchors.into());
        call.layout.window_border =
            next.layout.as_ref().map_or(Default::default(), |l| l.window_border.into());
        call.layout.invalidation = next.layout.as_ref().map_or(0, Layout::pending);
        call.layout.status = status;
        call.result = if handled { i32::from(SPLITTER_PART) } else { 0 };
        call.flags = u32::from(handled) * HANDLED
                     | u32::from(action) * ACTION
                     | u32::from(next.tracking.is_some()) * TRACKING;
        prepared.set_state(call.id, next);
        prepared.feedback = feedback;
        Ok((prepared, call))
    }
}
