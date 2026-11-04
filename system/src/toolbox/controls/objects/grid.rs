//! Subtree prepare/admit/commit. The desktop service guard excludes concurrent
//! tree/content edits throughout this synchronous request. Definition invocations
//! admit only the existing drawing/font services, never recursive control edits.
//! Every invocation uses cloned state and scratch arrays: no mutable Store borrow
//! crosses a CDEF call. The definition cache retains all resolved images. No plan
//! escapes the guard, so IDs/content cannot change between measurement and commit.
use core::ops::Range;

use mac_abi::controls::reflow as wire;

use super::*;

pub(super) struct Configuration
{
    tracks:     Vec<wire::TrackSetting>,
    boundaries: Vec<wire::SplitterSetting>,
    manual:     [Option<Arc<Vec<i64>>>; 2],
    splitter:   Option<Arc<BoundDefinition>>
}
impl Configuration
{
    pub(super) fn has_process_source(&self) -> bool
    {
        self.splitter.as_ref().is_some_and(|source| source.is_process_source())
    }

    pub(super) fn has_splitters(&self) -> bool
    {
        self.splitter.is_some()
    }

    fn percent_axis(&self, axis: u32) -> bool
    {
        self.tracks
            .iter()
            .any(|t| t.axis == axis && (t.bounds.min.kind == 2 || t.bounds.max.kind == 2))
    }

    fn copy(old: Option<&Self>) -> Result<Self, Status>
    {
        let mut tracks = Vec::new();
        let mut boundaries = Vec::new();
        if let Some(old) = old
        {
            tracks.try_reserve_exact(old.tracks.len()).map_err(|_| NO_MEMORY)?;
            boundaries.try_reserve_exact(old.boundaries.len()).map_err(|_| NO_MEMORY)?;
            tracks.extend_from_slice(&old.tracks);
            boundaries.extend_from_slice(&old.boundaries);
        }
        Ok(Self { tracks,
                  boundaries,
                  manual: old.map_or([None, None], |c| c.manual.clone()),
                  splitter: old.and_then(|c| c.splitter.clone()) })
    }
}
#[derive(Default)]
struct Axis
{
    tracks: Vec<wire::GridTrack>,
    items:  Vec<wire::GridItem>
}
pub(super) struct Item
{
    id:              u64,
    children:        Range<usize>,
    node:            tree::Node,
    state:           State,
    before:          Spec,
    before_hidden:   Option<bool>,
    before_clip:     Option<Rect>,
    before_grid:     Option<Arc<super::super::snapshot::Grid>>,
    entry:           DefinitionRef,
    table:           bool,
    outline_changed: bool,
    columns_cached:  bool,
    rows_cached:     bool,
    intrinsic:       layout::Measurement,
    constrained:     layout::Measurement,
    width:           i16,
    rect:            Rect,
    screen:          wire::LogicalRect,
    clip:            Option<Rect>,
    columns:         Axis,
    rows:            Axis
}
pub(crate) struct Prepared
{
    request:                   Option<alloc::boxed::Box<callbacks::RequestCalculation>>,
    retired_binding:           Option<BoundDefinition>,
    metadata:                  Option<(u64, State)>,
    items:                     Vec<Item>,
    pub(crate) drawings:       Vec<super::super::Drawing>,
    pub(crate) measurement:    wire::Measurement,
    pub(crate) limited_damage: Option<Rect>,
    pub(crate) feedback:       Option<Option<Rect>>,
    parent:                    u64,
    assign:                    bool
}
impl Prepared
{
    pub(super) fn assigns_geometry(&self) -> bool
    {
        self.assign
        || (!self.drawings.is_empty()
            && self.items.iter().any(|item| {
                                    item.before.bounds() != item.state.spec.bounds()
                                    || item.before_hidden
                                       != item.state.layout.as_ref().map(|layout| layout.hidden)
                                    || item.before_clip
                                       != tree::clip(item.clip,
                                                     item.state
                                                         .layout
                                                         .as_ref()
                                                         .and_then(Layout::clip))
                                }))
    }

    pub(crate) fn take(&mut self) -> Self
    {
        core::mem::replace(self, Self::empty())
    }

    pub(super) fn request_id(&self) -> Option<u64>
    {
        self.request.as_ref().map(|request| request.call.id)
    }

    pub(super) fn from_request(calculated: callbacks::RequestCalculation) -> Result<Self, Status>
    {
        let mut request = alloc::boxed::Box::try_new(calculated).map_err(|_| NO_MEMORY)?;
        let mut prepared = Self::empty();
        if request.drawing.is_some()
        {
            prepared.drawings.try_reserve_exact(1).map_err(|_| NO_MEMORY)?;
            if let Some(drawing) = request.drawing.take()
            {
                prepared.drawings.push(drawing);
            }
        }
        prepared.request = Some(request);
        Ok(prepared)
    }

    pub(super) fn state(&self, id: u64) -> Option<&State>
    {
        self.items.iter().find(|i| i.id == id).map(|i| &i.state).or_else(|| {
                                                                    self.metadata
                                                                        .as_ref()
                                                                        .filter(|(key, _)| {
                                                                            *key == id
                                                                        })
                                                                        .map(|(_, s)| s)
                                                                })
    }

    pub(super) fn set_state(&mut self, id: u64, state: State)
    {
        if let Some(i) = self.items.iter_mut().find(|i| i.id == id)
        {
            i.state = state;
        }
        else
        {
            self.metadata = Some((id, state));
        }
    }

    pub(super) fn empty() -> Self
    {
        Self { request:         None,
               retired_binding: None,
               metadata:        None,
               items:           Vec::new(),
               drawings:        Vec::new(),
               measurement:     Default::default(),
               limited_damage:  None,
               feedback:        None,
               parent:          0,
               assign:          false }
    }

    pub(super) fn metadata(id: u64, state: State) -> Self
    {
        Self { metadata: Some((id, state)),
               ..Self::empty() }
    }

    pub(super) fn extend(&mut self, mut next: Self) -> Result<(), Status>
    {
        if self.items.is_empty() && self.drawings.is_empty() && self.request.is_none()
        {
            *self = next;
            return Ok(());
        }
        if self.request.is_some() || next.request.is_some()
        {
            return Err(BAD_ABI);
        }
        self.items.try_reserve(next.items.len()).map_err(|_| NO_MEMORY)?;
        self.drawings.try_reserve(next.drawings.len()).map_err(|_| NO_MEMORY)?;
        self.items.append(&mut next.items);
        self.drawings.append(&mut next.drawings);
        Ok(())
    }
}
fn size(width: i64, height: i64) -> Result<layout::Size, Status>
{
    Ok(layout::Size::new(layout::Extent::from_wide(width).map_err(|e| e.status())?,
                         layout::Extent::from_wide(height).map_err(|e| e.status())?))
}
fn exact(width: i16) -> Result<layout::AxisConstraint, Status>
{
    Ok(layout::AxisConstraint::Exact(layout::Extent::new(width).map_err(|e| e.status())?))
}
async fn measure(state: &mut State, entry: DefinitionRef, author: &super::super::effects::Author,
                 constraints: layout::Constraints)
                 -> Result<layout::Measurement, Status>
{
    if let Some(l) = state.layout.as_mut().filter(|l| l.scroll.is_some())
    {
        let creation = l.sizing.creation();
        let raw = requirements(32,
                               32,
                               i64::from(creation.width().pixels()),
                               i64::from(creation.height().pixels()))?;
        l.sizing
         .prepare_calculated(layout::SizePreferences { minimum:   Some(raw.minimum()),
                                                       preferred: Some(raw.preferred()) })
         .commit();
        let measured = layout::Measurement { intrinsic:   raw,
                                             constrained: raw };
        l.intrinsic = Some(measured);
        l.measured = Some((constraints, measured));
        return Ok(layout::Measurement { intrinsic:   l.sizing.resolve(raw),
                                        constrained: l.sizing.resolve(raw) });
    }
    let mut payload = wire::Payload { message: wire::ASK_SIZE,
                                      constraints: constraints.into(),
                                      ..Default::default() };
    state.reflow_with(&mut payload, entry, author).await?;
    payload.measurement.try_into().map_err(|_| BAD_ABI)
}
fn requirements(min_x: i64, min_y: i64, pref_x: i64, pref_y: i64)
                -> Result<layout::Requirements, Status>
{
    Ok(layout::Requirements::new(size(min_x, min_y)?, size(pref_x, pref_y)?))
}
impl Axis
{
    fn allocate(count: usize, children: usize) -> Result<Self, Status>
    {
        let mut result = Self::default();
        result.tracks.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
        result.tracks.resize(count, Default::default());
        result.items.try_reserve_exact(children).map_err(|_| NO_MEMORY)?;
        result.items.resize(children, Default::default());
        Ok(result)
    }
}
impl Manager
{
    pub(crate) fn grid_configuration(&mut self, call: &mut ControlCall, owner: u64)
                                     -> Result<(), Status>
    {
        if call.layout.reserved != 0
           || call.layout.track.axis > 1
           || call.layout.track.index == u32::MAX
        {
            return Err(BAD_ARGUMENT);
        }
        let o = self.table(call.id, owner)?;
        let old = o.state.layout.as_ref().ok_or(NOT_SUPPORTED)?.grid.as_ref();
        let requested = call.layout.track;
        let previous =
            old.and_then(|c| {
                   c.tracks.iter().find(|t| t.axis == requested.axis && t.index == requested.index)
               })
               .copied()
               .unwrap_or(wire::TrackSetting { axis: requested.axis,
                                               index: requested.index,
                                               ..Default::default() });
        if call.layout.message == wire::GET_GRID_TRACK
        {
            call.layout.track = previous;
            return Ok(());
        }
        if call.layout.message != wire::SET_GRID_TRACK
        {
            return Err(NOT_SUPPORTED);
        }
        layout::SizeBounds::decode(requested.bounds).map_err(|e| e.status())?;
        if requested == previous && old.is_none_or(|c| c.manual[requested.axis as usize].is_none())
        {
            return Ok(());
        }
        if o.state.tracking.is_some()
        {
            return Err(SERVICE_BUSY);
        }
        let mut config = Configuration::copy(old.map(|c| c.as_ref()))?;
        config.tracks.retain(|t| t.axis != requested.axis || t.index != requested.index);
        if requested.bounds != wire::SizeBounds::default()
        {
            config.tracks.try_reserve(1).map_err(|_| NO_MEMORY)?;
            config.tracks.push(requested);
        }
        config.tracks.sort_unstable_by_key(|t| (t.axis, t.index));
        config.manual[requested.axis as usize] = None;
        let next = Some(Arc::try_new(config).map_err(|_| NO_MEMORY)?);
        self.objects
            .get_mut(call.id)
            .ok_or(NOT_FOUND)?
            .state
            .layout
            .as_mut()
            .ok_or(NOT_SUPPORTED)?
            .grid = next;
        self.invalidate_ancestors(call.id);
        Ok(())
    }

    pub(crate) fn splitter_configuration(&mut self, call: &mut ControlCall, owner: u64)
                                         -> Result<(), Status>
    {
        let setting = call.layout.splitter;
        if setting.axis > 1
           || setting.index >= u32::MAX - 1
           || setting.enabled > 1
           || setting.reserved != 0
           || call.layout.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        let root = self.table(call.id, owner)?;
        let old = root.state.layout.as_ref().ok_or(NOT_SUPPORTED)?.grid.as_deref();
        let enabled =
            old.is_some_and(|c| {
                   c.boundaries.iter().any(|b| b.axis == setting.axis && b.index == setting.index)
               });
        if call.layout.message == wire::GET_SPLITTER
        {
            call.layout.splitter.enabled = u32::from(enabled);
            return Ok(());
        }
        if root.state.tracking.is_some()
        {
            return Err(SERVICE_BUSY);
        }
        if call.layout.message == wire::SET_SPLITTER && enabled == (setting.enabled != 0)
        {
            return Ok(());
        }
        if call.layout.message == wire::RESET_SPLIT_SIZES
           && old.is_none_or(|c| c.manual[setting.axis as usize].is_none())
           && setting.index == 0
           && setting.enabled == 0
        {
            return Ok(());
        }
        let mut config = Configuration::copy(old)?;
        if call.layout.message == wire::RESET_SPLIT_SIZES
        {
            if setting.index != 0 || setting.enabled != 0
            {
                return Err(BAD_ARGUMENT);
            }
            if config.manual[setting.axis as usize].is_none()
            {
                return Ok(());
            }
            config.manual[setting.axis as usize] = None;
        }
        else if call.layout.message == wire::SET_SPLITTER
        {
            if enabled == (setting.enabled != 0)
            {
                return Ok(());
            }
            if setting.enabled != 0
            {
                if config.splitter.is_none()
                {
                    let entry =
                        crate::toolbox::runtime::Toolbox::control_definition(CDEF_SPLITTER)?;
                    config.splitter = Some(Arc::try_new(crate::toolbox::resources::definitions::bind(entry)?)
                                               .map_err(|_| NO_MEMORY)?);
                }
                config.boundaries.try_reserve(1).map_err(|_| NO_MEMORY)?;
                config.boundaries.push(setting);
                config.boundaries.sort_unstable_by_key(|b| (b.axis, b.index));
            }
            else
            {
                config.boundaries.retain(|b| b.axis != setting.axis || b.index != setting.index);
                if config.boundaries.is_empty()
                {
                    config.splitter = None;
                }
            }
        }
        else
        {
            return Err(NOT_SUPPORTED);
        }
        let config = Arc::try_new(config).map_err(|_| NO_MEMORY)?;
        self.objects
            .get_mut(call.id)
            .ok_or(NOT_FOUND)?
            .state
            .layout
            .as_mut()
            .ok_or(NOT_SUPPORTED)?
            .grid = Some(config);
        self.invalidate_ancestors(call.id);
        Ok(())
    }

    pub(crate) fn is_table(&self, id: u64, owner: u64) -> Result<bool, Status>
    {
        let o = self.snapshot(id, owner)?;
        Ok(o.state.layout.is_some()
           && matches!(o.state.spec,
                       Spec::Standard { procedure: TABLE_PROCEDURE,
                                        .. }))
    }

    /// Prepare with cloned state only. The caller admits all retained output
    /// before committing; multiple roots can share one window transaction.
    pub(crate) fn prepare_layout(&self, call: &ControlCall, owner: u64, paint_pending: bool,
                                 gate: TrapGate,
                                 window: Option<(Rect, layout::Anchors, layout::BorderOffsets)>)
                                 -> Result<Prepared, Status>
    {
        let snapshot = self.snapshot_layout_change(call, owner, paint_pending, window, None, None)?;
        let author = super::super::effects::Author::trusted(gate);
        super::super::effects::immediate(snapshot.calculate(&author))
    }

    pub(super) fn snapshot_layout_change(&self, call: &ControlCall, owner: u64,
                                         paint_pending: bool,
                                         window: Option<(Rect,
                                                 layout::Anchors,
                                                 layout::BorderOffsets)>,
                                         scroll_change: Option<Option<Arc<scrolling::Scroll>>>,
                                         root_change: Option<State>)
                                         -> Result<LayoutSnapshot, Status>
    {
        if call.layout.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        let root = self.get(call.id, owner)?;
        let assign = call.layout.message == wire::ASSIGN_RECT;
        let constraints: layout::Constraints = if assign
        {
            let rect = layout::LayoutRect::try_from(call.layout.rect).map_err(|e| e.status())?;
            layout::Constraints { width:  exact(rect.size().width().pixels())?,
                                  height: exact(rect.size().height().pixels())? }
        }
        else if call.layout.message == wire::ASK_SIZE
        {
            call.layout.constraints.try_into().map_err(|e: layout::Error| e.status())?
        }
        else
        {
            return Err(NOT_SUPPORTED);
        };
        let root_layout = root.state.layout.as_ref().ok_or(NOT_SUPPORTED)?;
        if window.is_none()
           && scroll_change.is_none()
           && root_change.is_none()
           && assign
           && root.state.spec.bounds() == call.layout.rect
           && !root_layout.arrangement_pending
        {
            return Ok(LayoutSnapshot::Ready(Prepared::empty()));
        }
        if assign && paint_pending
        {
            return Err(SERVICE_BUSY);
        }
        if !assign
        {
            let cached = if constraints == layout::Constraints::default()
            {
                root_layout.intrinsic
            }
            else
            {
                root_layout.measured.filter(|(key, _)| *key == constraints).map(|(_, value)| value)
            };
            if let Some(measured) = cached
            {
                let mut prepared = Prepared::empty();
                prepared.measurement = layout::Measurement { intrinsic: root_layout.sizing.resolve(measured.intrinsic), constrained: root_layout.sizing.resolve(measured.constrained) }.into();
                return Ok(LayoutSnapshot::Ready(prepared));
            }
        }
        let mut plan = Vec::new();
        self.append_plan(&mut plan, call.id, owner)?;
        if let Some(state) = root_change
        {
            plan[0].state = state;
            plan[0].outline_changed = true;
        }
        if let Some(scroll) = scroll_change
        {
            let l = plan[0].state.layout.as_mut().ok_or(NOT_SUPPORTED)?;
            l.scroll = scroll;
            l.intrinsic = None;
            l.measured = None;
            l.arrangement_pending = true;
            plan[0].outline_changed = true;
        }
        if let Some((viewport, anchors, border)) = window
        {
            let l = plan[0].state.layout.as_mut().ok_or(NOT_SUPPORTED)?;
            l.anchors = anchors;
            l.window_border = border;
            l.window_extent = Some((viewport.right, viewport.bottom));
        }
        let mut index = 0;
        while index < plan.len()
        {
            let start = plan.len();
            let mut id = plan[index].node.first;
            while id != 0
            {
                let next = self.get(id, owner)?.tree.next;
                self.append_plan(&mut plan, id, owner)?;
                id = next;
            }
            plan[index].children = start..plan.len();
            if assign && plan[index].state.tracking.is_some()
            {
                return Err(SERVICE_BUSY);
            }
            index += 1;
        }
        let inherited_clip = self.inherited_clip(call.id);
        let root_screen = if root.tree.parent != 0 && call.layout.rect == root.state.spec.bounds()
        {
            Some(self.screen_rect(call.id, owner)?)
        }
        else
        {
            None
        };
        Ok(LayoutSnapshot::Plan { plan,
                                  constraints,
                                  window,
                                  assign,
                                  requested_rect: call.layout.rect,
                                  parent: root.tree.parent,
                                  inherited_clip,
                                  root_screen })
    }

    pub(crate) fn prepare_scroll(&self, call: &mut ControlCall, owner: u64, paint_pending: bool,
                                 gate: TrapGate,
                                 resolve: impl FnMut(i16) -> Result<DefinitionRef, Status>)
                                 -> Result<Prepared, Status>
    {
        let snapshot = self.snapshot_scroll(call, owner, paint_pending, resolve)?;
        let author = super::super::effects::Author::trusted(gate);
        let (prepared, output) = super::super::effects::immediate(snapshot.calculate(&author))?;
        *call = output.wire();
        Ok(prepared)
    }

    pub(super) fn snapshot_scroll(&self, call: &mut ControlCall, owner: u64, paint_pending: bool,
                                  mut resolve: impl FnMut(i16) -> Result<DefinitionRef, Status>)
                                  -> Result<ScrollSnapshot, Status>
    {
        let root = self.table(call.id, owner)?;
        if call.layout.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        if call.window != 0 && call.window != root.window
        {
            return Err(BAD_ARGUMENT);
        }
        if paint_pending
        {
            return Err(SERVICE_BUSY);
        }
        let current = root.state.layout.as_ref().ok_or(NOT_SUPPORTED)?;
        call.window = root.window;
        call.relation = root.tree.relation();
        root.state.spec.write(call);
        call.reflow = 1;
        call.layout.anchors = current.anchors.into();
        call.layout.window_border = current.window_border.into();
        call.layout.invalidation = current.pending();
        call.layout.status = self.layout_status(call.id);
        let mut change = None;
        if call.layout.message == wire::SET_SCROLLING
        {
            let axes = call.layout.scroll.axes;
            if axes & !3 != 0
            {
                return Err(BAD_ARGUMENT);
            }
            if axes == current.scroll.as_ref().map_or(0, |s| s.data.axes)
            {
                return Ok(ScrollSnapshot::Ready(Prepared::empty(), super::super::effects::DetachedCall::capture(call)?));
            }
            change = Some(if axes == 0
                          {
                              None
                          }
                          else
                          {
                              Some(Arc::try_new(scrolling::Scroll::new(axes, resolve(1)?)?).map_err(|_| NO_MEMORY)?)
                          });
        }
        else if current.scroll.is_none()
        {
            return Err(NOT_SUPPORTED);
        }
        if call.layout.message == wire::SCROLL_TO
           && (call.layout.scroll.x < 0 || call.layout.scroll.y < 0)
        {
            return Err(BAD_ARGUMENT);
        }
        let reveal = if call.layout.message == wire::REVEAL_CHILD
        {
            let child = self.get(call.layout.child, owner)?;
            if child.tree.parent != call.id
            {
                return Err(BAD_ARGUMENT);
            }
            Some(child.state.layout.as_ref().ok_or(NOT_SUPPORTED)?.logical)
        }
        else
        {
            None
        };
        if call.operation == BEGIN_TRACKING
           && self.objects.iter().any(|(id, o)| {
                                     o.recipient == owner
                                     && o.state.tracking.is_some()
                                     && (id != call.id || root.state.tracking.is_some())
                                 })
        {
            return Err(SERVICE_BUSY);
        }
        if call.operation == REFLOW_OBJECT
           && !current.arrangement_pending
           && change.is_none()
           && let Some(old) = &current.scroll
        {
            let mut next = old.as_ref().clone();
            if call.layout.message == wire::SCROLL_TO
            {
                next.position(call.layout.scroll.x, call.layout.scroll.y)?;
            }
            else if call.layout.message == wire::REVEAL_CHILD
            {
                next.reveal(reveal.ok_or(BAD_ARGUMENT)?)?;
            }
            if next.data == old.data
            {
                return Ok(ScrollSnapshot::Ready(Prepared::empty(),
                super::super::effects::DetachedCall::capture(call)?));
            }
        }
        let changing = change.is_some();
        let assignment = ControlCall { id: call.id,
                                       operation: REFLOW_OBJECT,
                                       layout: wire::Payload { message: wire::ASSIGN_RECT,
                                                               rect: root.state.spec.bounds(),
                                                               ..Default::default() },
                                       ..Default::default() };
        let layout = if call.operation == POLL_TRACKING && root.state.tracking.is_some()
        {
            LayoutSnapshot::Ready(Prepared::empty())
        }
        else
        {
            self.snapshot_layout_change(&assignment, owner, false, None, change, None)?
        };
        let fallback = if matches!(layout, LayoutSnapshot::Ready(_))
        {
            let mut prepared = Prepared::empty();
            self.append_plan(&mut prepared.items, call.id, owner)?;
            let mut index = 0;
            while index < prepared.items.len()
            {
                let start = prepared.items.len();
                let mut id = prepared.items[index].node.first;
                while id != 0
                {
                    self.append_plan(&mut prepared.items, id, owner)?;
                    id = self.get(id, owner)?.tree.next;
                }
                prepared.items[index].children = start..prepared.items.len();
                index += 1;
            }
            prepared.parent = root.tree.parent;
            Some(prepared)
        }
        else
        {
            None
        };
        Ok(ScrollSnapshot::Plan { initial: root.state.clone(),
                                  changing,
                                  layout,
                                  fallback,
                                  call: super::super::effects::DetachedCall::capture(call)?,
                                  clip: self.visible_clip(call.id),
                                  screen: self.screen_rect(call.id, owner)?,
                                  inherited_clip: self.inherited_clip(call.id),
                                  reveal,
                                  other_tracking: self.objects.iter().any(|(id, o)| {
                                                                         id != call.id
                                                                         && o.recipient == owner
                                                                         && o.state
                                                                             .tracking
                                                                             .is_some()
                                                                     }) })
    }

    async fn translate_plan_with(prepared: &mut Prepared, screen: wire::LogicalRect,
                                 inherited_clip: Option<Rect>,
                                 author: &super::super::effects::Author)
                                 -> Result<(), Status>
    {
        let plan = &mut prepared.items;
        plan[0].screen = screen;
        plan[0].clip = inherited_clip;
        prepared.drawings.clear();
        prepared.drawings.try_reserve(plan.len()).map_err(|_| NO_MEMORY)?;
        for i in 0..plan.len()
        {
            let (rect, hidden) = scrolling::project(plan[i].screen, plan[i].clip)?;
            plan[i].rect = rect;
            plan[i].state.spec.set_bounds(rect);
            let screen = plan[i].screen;
            let l = plan[i].state.layout.as_mut().ok_or(CORRUPT_DATA)?;
            l.hidden = hidden;
            if let Some(old) = &l.scroll
            {
                let mut next = old.as_ref().clone();
                next.arrange(rect, old.data.width, old.data.height)?;
                if next.data != old.data
                {
                    l.scroll = Some(Arc::try_new(next).map_err(|_| NO_MEMORY)?);
                }
            }
            let (sx, sy) = l.scroll.as_ref().map_or((0, 0), |s| (s.data.x, s.data.y));
            if let Some(old) = &l.outlines
            {
                let mut cells = Vec::new();
                let mut logical = Vec::new();
                cells.try_reserve_exact(old.logical.len()).map_err(|_| NO_MEMORY)?;
                logical.try_reserve_exact(old.logical.len()).map_err(|_| NO_MEMORY)?;
                for r in &old.logical
                {
                    cells.push(Self::outline(Self::translate(*r, screen.left, screen.top, sx,
                                                             sy)?,
                                             l.viewport(rect)));
                    logical.push(*r);
                }
                if cells != old.cells
                {
                    l.outlines = Some(Arc::try_new(super::super::snapshot::Grid { cells, logical, dividers:old.dividers.clone() }).map_err(|_| NO_MEMORY)?);
                }
            }
            let local_clip = l.clip();
            let viewport = l.viewport(rect);
            if let Some(old) = plan[i].state.layout.as_ref().and_then(|l| l.outlines.clone())
               && let Some(d) = &old.dividers
            {
                let next = Self::divider_snapshot(&plan[i], plan, &old.logical, d.tracks.clone())?;
                if next.segments != d.segments || next.viewport != d.viewport
                {
                    let mut cells = Vec::new();
                    let mut logical = Vec::new();
                    cells.try_reserve_exact(old.cells.len()).map_err(|_| NO_MEMORY)?;
                    logical.try_reserve_exact(old.logical.len()).map_err(|_| NO_MEMORY)?;
                    cells.extend_from_slice(&old.cells);
                    logical.extend_from_slice(&old.logical);
                    plan[i].state.layout.as_mut().ok_or(CORRUPT_DATA)?.outlines=Some(Arc::try_new(super::super::snapshot::Grid {cells,logical,dividers:Some(next)}).map_err(|_|NO_MEMORY)?);
                }
            }
            let clip = tree::clip(tree::clip(plan[i].clip, local_clip), Some(viewport));
            for child in plan[i].children.clone()
            {
                let logical = plan[child].state.layout.as_ref().ok_or(CORRUPT_DATA)?.logical;
                plan[child].screen = Self::translate(logical, screen.left, screen.top, sx, sy)?;
                plan[child].clip = clip;
            }
            let visible = tree::clip(plan[i].clip, local_clip);
            let old_clip = plan[i].before_clip;
            let same_grid = match (plan[i].before_grid.as_ref(),
                                   plan[i].state.layout.as_ref().and_then(|l| l.outlines.as_ref()))
            {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false
            };
            if i != 0 && plan[i].before == plan[i].state.spec && old_clip == visible && same_grid
            {
                continue;
            }
            let state = {
                let mut wire = plan[i].state.spec.draw();
                wire.reflow = 1;
                super::super::effects::DetachedCall::capture(&wire)?
            };
            let (_, drawing) =
                author.prepare(state, plan[i].entry, plan[i].state.title.as_ref()).await?;
            let mut drawing = drawing.ok_or(BAD_ABI)?;
            drawing.id = plan[i].id;
            drawing.grid = plan[i].state.layout.as_ref().and_then(|l| l.outlines.clone());
            Self::scroll_drawing(&plan[i].state, &mut drawing)?;
            drawing.set_visible(visible);
            prepared.drawings.push(drawing);
        }
        Ok(())
    }

    /// No allocation, definition call or fallible work after scene publication.
    /// The desktop guard and definition nonreentrancy keep every staged ID live.
    pub(crate) fn commit_layout(&mut self, mut prepared: Prepared)
    {
        self.commit_layout_staged(&mut prepared);
    }

    /// Same infallible publication, retaining every actual retired State in the
    /// SAME owned plan until its enclosing carrier leaves Desktop.
    pub(crate) fn commit_layout_staged(&mut self, prepared: &mut Prepared)
    {
        if let Some(request) = prepared.request.as_mut()
        {
            let call = request.call.wire();
            // Original full-key claim and shared publication preflight retain
            // this canonical object throughout the same exclusive turn.
            if let Some(object) = self.objects.get_mut(call.id)
            {
                let effects = callbacks::CommitEffects::apply(object, &mut request.next,
                                                              request.previous, call.operation,
                                                              None);
                prepared.retired_binding = self.apply_request_effects(&call, effects);
            }
        }
        if let Some((id, state)) = &mut prepared.metadata
        {
            let released =
                self.objects
                    .get(*id)
                    .is_some_and(|o| o.state.tracking.is_some() && state.tracking.is_none());
            if let Some(object) = self.objects.get_mut(*id)
            {
                core::mem::swap(&mut object.state, state);
            }
            self.layout_committed(*id);
            if released
            {
                self.wake_capture(*id);
            }
        }
        for staged in &mut prepared.items
        {
            let released =
                self.objects
                    .get(staged.id)
                    .is_some_and(|o| o.state.tracking.is_some() && staged.state.tracking.is_none());
            if let Some(object) = self.objects.get_mut(staged.id)
            {
                core::mem::swap(&mut object.state, &mut staged.state);
            }
            self.layout_committed(staged.id);
            if released
            {
                self.wake_capture(staged.id);
            }
        }
        if prepared.assign
        {
            self.invalidate_arrangement_ancestors(prepared.parent);
        }
    }

    fn append_plan(&self, plan: &mut Vec<Item>, id: u64, owner: u64) -> Result<(), Status>
    {
        let o = self.get(id, owner)?;
        if o.state.layout.is_none()
        {
            return Err(NOT_SUPPORTED);
        }
        let table = matches!(o.state.spec,
                             Spec::Standard { procedure: TABLE_PROCEDURE,
                                              .. });
        if o.tree.first != 0 && !table
        {
            return Err(CORRUPT_DATA);
        }
        // The canonical object's original source remains authoritative across
        // a detached turn; current selection cannot replace its binding.
        let entry = o.definition.reference();
        plan.try_reserve(1).map_err(|_| NO_MEMORY)?;
        let zero = layout::Requirements::default();
        let empty = layout::Measurement { intrinsic:   zero,
                                          constrained: zero };
        plan.push(Item { id,
                         children: 0..0,
                         node: o.tree,
                         state: o.state.clone(),
                         before: o.state.spec,
                         before_hidden: o.state.layout.as_ref().map(|l| l.hidden),
                         before_clip: self.visible_clip(id),
                         before_grid: o.state.layout.as_ref().and_then(|l| l.outlines.clone()),
                         entry,
                         table,
                         outline_changed: false,
                         columns_cached: false,
                         rows_cached: false,
                         intrinsic: empty,
                         constrained: empty,
                         width: 0,
                         rect: o.state.spec.bounds(),
                         screen: scrolling::logical(o.state.spec.bounds()),
                         clip: None,
                         columns: Axis::default(),
                         rows: Axis::default() });
        Ok(())
    }

    fn cell_axis(item: &Item, offset: usize, start: u32, span: u32, vertical: bool)
                 -> Result<(i64, i64), Status>
    {
        if if vertical { item.rows_cached } else { item.columns_cached }
        {
            let old = item.state
                          .layout
                          .as_ref()
                          .and_then(|l| l.outlines.as_ref())
                          .and_then(|g| g.logical.get(offset))
                          .ok_or(CORRUPT_DATA)?;
            return Ok(if vertical { (old.top, old.bottom) } else { (old.left, old.right) });
        }
        let axis = if vertical { &item.rows } else { &item.columns };
        let first = axis.tracks.get(start as usize).ok_or(CORRUPT_DATA)?;
        let end = start.checked_add(span).and_then(|n| n.checked_sub(1)).ok_or(CORRUPT_DATA)?;
        let last = axis.tracks.get(end as usize).ok_or(CORRUPT_DATA)?;
        Ok((first.position, last.position.checked_add(last.assigned).ok_or(BAD_ARGUMENT)?))
    }

    fn translate(rect: wire::LogicalRect, x: i64, y: i64, sx: i64, sy: i64)
                 -> Result<wire::LogicalRect, Status>
    {
        let x = x.checked_sub(sx).ok_or(BAD_ARGUMENT)?;
        let y = y.checked_sub(sy).ok_or(BAD_ARGUMENT)?;
        Ok(wire::LogicalRect { left:   x.checked_add(rect.left).ok_or(BAD_ARGUMENT)?,
                               right:  x.checked_add(rect.right).ok_or(BAD_ARGUMENT)?,
                               top:    y.checked_add(rect.top).ok_or(BAD_ARGUMENT)?,
                               bottom: y.checked_add(rect.bottom).ok_or(BAD_ARGUMENT)? })
    }

    fn outline(rect: wire::LogicalRect, viewport: Rect) -> Rect
    {
        // CDEF compares cell edges strictly inside its viewport, so clipping an
        // offscreen edge to the viewport cannot create a false interior line.
        // This also keeps merged spans within the finite drawing ABI.
        let x = |v: i64| v.clamp(viewport.left.into(), viewport.right.into()) as i16;
        let y = |v: i64| v.clamp(viewport.top.into(), viewport.bottom.into()) as i16;
        Rect::new(x(rect.left), y(rect.top), x(rect.right), y(rect.bottom))
    }

    fn allocate_axes(plan: &mut [Item], index: usize) -> Result<(), Status>
    {
        if !plan[index].table
        {
            return Err(CORRUPT_DATA);
        }
        let mut rows = 0u32;
        let mut columns = 0u32;
        for child in plan[index].children.clone()
        {
            let n = plan[child].node;
            rows = rows.max(n.cell.row.checked_add(n.span.rows).ok_or(CORRUPT_DATA)?);
            columns = columns.max(n.cell.column.checked_add(n.span.columns).ok_or(CORRUPT_DATA)?);
        }
        if let Some(config) = plan[index].state.layout.as_ref().and_then(|l| l.grid.as_ref())
        {
            for t in &config.tracks
            {
                let end = t.index.checked_add(1).ok_or(BAD_ARGUMENT)?;
                if t.axis == 0
                {
                    columns = columns.max(end);
                }
                else
                {
                    rows = rows.max(end);
                }
            }
        }
        plan[index].columns = Axis::allocate(columns as usize, plan[index].children.len())?;
        plan[index].rows = Axis::allocate(rows as usize, plan[index].children.len())?;
        Ok(())
    }

    async fn axis(plan: &mut [Item], index: usize, vertical: bool, constrained: bool,
                  constraint: layout::AxisConstraint, basis: Option<i64>,
                  author: &super::super::effects::Author)
                  -> Result<(i64, i64), Status>
    {
        let entry = plan[index].entry;
        for (offset, child) in plan[index].children.clone().enumerate()
        {
            let n = plan[child].node;
            let anchors: u32 =
                plan[child].state.layout.as_ref().ok_or(CORRUPT_DATA)?.anchors.into();
            let requirement = if constrained
            {
                plan[child].constrained.constrained
            }
            else
            {
                plan[child].intrinsic.intrinsic
            };
            let (minimum, preferred) = (requirement.minimum(), requirement.preferred());
            let item =
                wire::GridItem { start: if vertical { n.cell.row } else { n.cell.column },
                                 count: if vertical { n.span.rows } else { n.span.columns },
                                 minimum: i64::from(if vertical
                                                    {
                                                        minimum.height()
                                                    }
                                                    else
                                                    {
                                                        minimum.width()
                                                    }.pixels()),
                                 preferred: i64::from(if vertical
                                                      {
                                                          preferred.height()
                                                      }
                                                      else
                                                      {
                                                          preferred.width()
                                                      }.pixels()),
                                 anchors: if vertical { anchors >> 2 } else { anchors & 3 },
                                 ..Default::default() };
            let axis = if vertical { &mut plan[index].rows } else { &mut plan[index].columns };
            axis.items[offset] = item;
        }
        let item = &mut plan[index];
        let axis = if vertical { &mut item.rows } else { &mut item.columns };
        if let Some(config) = item.state.layout.as_ref().and_then(|l| l.grid.as_ref())
        {
            for t in config.tracks.iter().filter(|t| (t.axis == 1) == vertical)
            {
                let track = axis.tracks.get_mut(t.index as usize).ok_or(CORRUPT_DATA)?;
                track.bounds = t.bounds;
            }
        }
        if let Some(config) = item.state.layout.as_ref().and_then(|l| l.grid.as_ref())
        {
            for b in config.boundaries.iter().filter(|b| (b.axis == 1) == vertical)
            {
                if (b.index as usize) + 1 < axis.tracks.len()
                {
                    axis.tracks[b.index as usize].gap = 4;
                }
            }
            if let Some(manual) = &config.manual[usize::from(vertical)]
            {
                for (t, value) in axis.tracks.iter_mut().zip(manual.iter())
                {
                    t.manual = 1;
                    t.requested = *value;
                }
            }
        }
        author.grid_axis(entry, constraint.into(), basis, &mut axis.tracks, &mut axis.items).await
    }
}

impl Manager
{
    fn divider_snapshot(item: &Item, plan: &[Item], logical: &[wire::LogicalRect],
                        tracks: [Arc<Vec<wire::GridTrack>>; 2])
                        -> Result<Arc<super::super::snapshot::Dividers>, Status>
    {
        use super::super::snapshot::{Divider, Dividers};
        let l = item.state.layout.as_ref().ok_or(CORRUPT_DATA)?;
        let config = l.grid.as_ref().ok_or(CORRUPT_DATA)?;
        let viewport = l.viewport(item.rect);
        let (sx, sy) = l.scroll.as_ref().map_or((0, 0), |s| (s.data.x, s.data.y));
        let mut segments = Vec::new();
        for b in config.boundaries.iter().filter(|_| !l.hidden)
        {
            let axis = &tracks[b.axis as usize];
            let Some(a) = axis.get(b.index as usize)
            else
            {
                continue;
            };
            let Some(z) = axis.get(b.index as usize + 1)
            else
            {
                continue;
            };
            if a.assigned_gap == 0
            {
                continue;
            }
            let origin = if b.axis == 0 { item.screen.left - sx } else { item.screen.top - sy };
            let value = origin.checked_add(a.position)
                              .and_then(|v| v.checked_add(a.assigned))
                              .ok_or(BAD_ARGUMENT)?;
            let finish = value.checked_add(a.assigned_gap).ok_or(BAD_ARGUMENT)?;
            let (near, far) = if b.axis == 0
            {
                (viewport.left, viewport.right)
            }
            else
            {
                (viewport.top, viewport.bottom)
            };
            if value < i64::from(near) || finish > i64::from(far)
            {
                continue;
            }
            let min = origin.checked_add(a.position)
                            .and_then(|v| v.checked_add(a.minimum.min(a.assigned)))
                            .ok_or(BAD_ARGUMENT)?
                            .max(value.saturating_sub(z.maximum.saturating_sub(z.assigned)));
            let max = origin.checked_add(z.position)
                            .and_then(|v| v.checked_add(z.assigned - z.minimum.min(z.assigned)))
                            .and_then(|v| v.checked_sub(a.assigned_gap))
                            .ok_or(BAD_ARGUMENT)?
                            .min(value.saturating_add(a.maximum.saturating_sub(a.assigned)));
            let clamp = |n: i64| n.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16;
            let (cross_start, cross_end) = if b.axis == 0
            {
                (viewport.top, viewport.bottom)
            }
            else
            {
                (viewport.left, viewport.right)
            };
            let mut blocked = Vec::new();
            blocked.try_reserve_exact(item.children.len()).map_err(|_| NO_MEMORY)?;
            for (offset, child) in item.children.clone().enumerate()
            {
                let n = plan[child].node;
                let (start, count) = if b.axis == 0
                {
                    (n.cell.column, n.span.columns)
                }
                else
                {
                    (n.cell.row, n.span.rows)
                };
                if start <= b.index && start.checked_add(count).ok_or(CORRUPT_DATA)? > b.index + 1
                {
                    let r = *logical.get(offset).ok_or(CORRUPT_DATA)?;
                    let (lo, hi) = if b.axis == 0
                    {
                        (item.screen.top - sy + r.top, item.screen.top - sy + r.bottom)
                    }
                    else
                    {
                        (item.screen.left - sx + r.left, item.screen.left - sx + r.right)
                    };
                    let lo = clamp(lo).max(cross_start);
                    let hi = clamp(hi).min(cross_end);
                    if lo < hi
                    {
                        blocked.push((lo, hi));
                    }
                }
            }
            blocked.sort_unstable();
            let mut start = cross_start;
            for (lo, hi) in blocked.into_iter().chain(core::iter::once((cross_end, cross_end)))
            {
                if lo > start
                {
                    segments.try_reserve(1).map_err(|_| NO_MEMORY)?;
                    let bounds = if b.axis == 0
                    {
                        Rect::new(value as i16, start, finish as i16, lo)
                    }
                    else
                    {
                        Rect::new(start, value as i16, lo, finish as i16)
                    };
                    segments.push(Divider { axis: b.axis,
                                            index: b.index,
                                            bounds,
                                            minimum: clamp(min),
                                            maximum: clamp(max),
                                            position: value as i16 });
                }
                start = start.max(hi);
            }
        }
        Arc::try_new(Dividers { definition: config.splitter.clone().ok_or(CORRUPT_DATA)?,
                                viewport,
                                segments,
                                tracks }).map_err(|_| NO_MEMORY)
    }
}

impl Manager
{
    pub(super) fn split_release_state(mut state: State, axis: u32, index: u32, delta: i64)
                                      -> Result<State, Status>
    {
        let l = state.layout.as_mut().ok_or(NOT_SUPPORTED)?;
        let d = l.outlines.as_ref().and_then(|g| g.dividers.as_ref()).ok_or(CORRUPT_DATA)?;
        let tracks = &d.tracks[axis as usize];
        let mut sizes = Vec::new();
        sizes.try_reserve_exact(tracks.len()).map_err(|_| NO_MEMORY)?;
        sizes.extend(tracks.iter().map(|t| t.assigned));
        let a = sizes.get_mut(index as usize).ok_or(CORRUPT_DATA)?;
        *a = a.checked_add(delta).filter(|v| *v >= 0).ok_or(BAD_ARGUMENT)?;
        let b = sizes.get_mut(index as usize + 1).ok_or(CORRUPT_DATA)?;
        *b = b.checked_sub(delta).filter(|v| *v >= 0).ok_or(BAD_ARGUMENT)?;
        let mut config = Configuration::copy(l.grid.as_deref())?;
        config.manual[axis as usize] = Some(Arc::try_new(sizes).map_err(|_| NO_MEMORY)?);
        l.grid = Some(Arc::try_new(config).map_err(|_| NO_MEMORY)?);
        l.intrinsic = None;
        l.measured = None;
        l.arrangement_pending = true;
        Ok(state)
    }
}

/// All manager observations precede the first author effect. This owned value
/// can calculate outside Desktop; final publication must validate its claims.
pub(super) enum LayoutSnapshot
{
    Ready(Prepared),
    Plan
    {
        plan:           Vec<Item>,
        constraints:    layout::Constraints,
        window:         Option<(Rect, layout::Anchors, layout::BorderOffsets)>,
        assign:         bool,
        requested_rect: Rect,
        parent:         u64,
        inherited_clip: Option<Rect>,
        root_screen:    Option<wire::LogicalRect>
    }
}
impl LayoutSnapshot
{
    pub(super) fn with_root_state(mut self, state: State) -> Result<Self, Status>
    {
        let Self::Plan { plan,
                         .. } = &mut self
        else
        {
            return Err(BAD_ABI);
        };
        let root = plan.first_mut().ok_or(BAD_ABI)?;
        root.state = state;
        root.outline_changed = true;
        Ok(self)
    }

    pub(super) async fn calculate(self, author: &super::super::effects::Author)
                                  -> Result<Prepared, Status>
    {
        let Self::Plan { mut plan,
                         constraints,
                         window,
                         assign,
                         requested_rect,
                         parent,
                         inherited_clip,
                         root_screen } = self
        else
        {
            let Self::Ready(ready) = self
            else
            {
                return Err(CORRUPT_DATA);
            };
            return Ok(ready);
        };
        // One intrinsic bottom-up pass; leaves reuse cached content requirements.
        for i in (0..plan.len()).rev()
        {
            if plan[i].children.is_empty()
            {
                let entry = plan[i].entry;
                let measured =
                    measure(&mut plan[i].state, entry, author, Default::default()).await?;
                plan[i].intrinsic = measured;
            }
            else
            {
                Manager::allocate_axes(&mut plan, i)?;
                if let Some(cached) = plan[i].state.layout.as_ref().and_then(|l| l.intrinsic)
                {
                    let l = plan[i].state.layout.as_ref().ok_or(CORRUPT_DATA)?;
                    plan[i].intrinsic =
                        layout::Measurement { intrinsic:   l.sizing.resolve(cached.intrinsic),
                                              constrained: l.sizing.resolve(cached.constrained) };
                    continue;
                }
                let x = Manager::axis(&mut plan,
                                      i,
                                      false,
                                      false,
                                      layout::AxisConstraint::Unbounded,
                                      None,
                                      author).await?;
                let y = Manager::axis(&mut plan,
                                      i,
                                      true,
                                      false,
                                      layout::AxisConstraint::Unbounded,
                                      None,
                                      author).await?;
                let raw = if let Some(l) =
                    plan[i].state.layout.as_ref().filter(|l| l.scroll.is_some())
                {
                    let creation = l.sizing.creation();
                    requirements(32,
                                 32,
                                 i64::from(creation.width().pixels()),
                                 i64::from(creation.height().pixels()))?
                }
                else
                {
                    requirements(x.0, y.0, x.1, y.1)?
                };
                let l = plan[i].state.layout.as_mut().ok_or(CORRUPT_DATA)?;
                l.sizing
                 .prepare_calculated(layout::SizePreferences { minimum:   Some(raw.minimum()),
                                                               preferred: Some(raw.preferred()) })
                 .commit();
                l.intrinsic = Some(layout::Measurement { intrinsic:   raw,
                                                         constrained: raw });
                let effective = l.sizing.resolve(raw);
                plan[i].intrinsic = layout::Measurement { intrinsic:   effective,
                                                          constrained: effective };
            }
        }
        let root_pref = plan[0].intrinsic.intrinsic.preferred().width().pixels();
        plan[0].width = match constraints.width
        {
            layout::AxisConstraint::Unbounded => root_pref,
            layout::AxisConstraint::AtMost(v) => root_pref.min(v.pixels()),
            layout::AxisConstraint::Exact(v) => v.pixels()
        };
        if let Some((viewport, anchors, border)) = window
        {
            let (start, extent) = super::window::axis(viewport.right,
                                                      border.left(),
                                                      border.right(),
                                                      root_pref,
                                                      anchors.left,
                                                      anchors.right);
            plan[0].width = extent;
            plan[0].rect.left = start;
            plan[0].rect.right = start + extent;
        }
        // Resolve widths top-down, including anchors, before any row calculation.
        for i in 0..plan.len()
        {
            if plan[i].children.is_empty()
            {
                let entry = plan[i].entry;
                let query = layout::Constraints { width:  exact(plan[i].width)?,
                                                  height: layout::AxisConstraint::Unbounded };
                plan[i].constrained = measure(&mut plan[i].state, entry, author, query).await?;
            }
            else
            {
                let can_reuse =
                    plan[i].state
                           .layout
                           .as_ref()
                           .is_some_and(|l| !l.arrangement_pending && l.outlines.is_some())
                    && (plan[i].state.layout.as_ref().is_some_and(|l| {
                                                         l.scroll.is_some()
                                                         && !l.grid
                                                              .as_ref()
                                                              .is_some_and(|c| c.percent_axis(0))
                                                     })
                        || i32::from(plan[i].before.bounds().right)
                           - i32::from(plan[i].before.bounds().left)
                           == i32::from(plan[i].width));
                plan[i].columns_cached = can_reuse;
                if can_reuse
                {
                    if let Some(d) = plan[i].state
                                            .layout
                                            .as_ref()
                                            .and_then(|l| l.outlines.as_ref())
                                            .and_then(|g| g.dividers.as_ref())
                    {
                        if d.tracks[0].len() != plan[i].columns.tracks.len()
                        {
                            return Err(CORRUPT_DATA);
                        }
                        let tracks = d.tracks[0].clone();
                        plan[i].columns.tracks.copy_from_slice(&tracks);
                    }
                    for (offset, child) in plan[i].children.clone().enumerate()
                    {
                        let old = plan[child].state.layout.as_ref().ok_or(CORRUPT_DATA)?.logical;
                        plan[i].columns.items[offset].position = old.left;
                        plan[i].columns.items[offset].extent = old.right - old.left;
                    }
                }
                else
                {
                    let width = if plan[i].state.layout.as_ref().is_some_and(|l| l.scroll.is_some())
                    {
                        layout::AxisConstraint::Unbounded
                    }
                    else
                    {
                        exact(plan[i].width)?
                    };
                    let basis = Some(i64::from(plan[i].width));
                    Manager::axis(&mut plan, i, false, false, width, basis, author).await?;
                }
                for (offset, child) in plan[i].children.clone().enumerate()
                {
                    plan[child].width =
                        i16::try_from(plan[i].columns.items[offset].extent).map_err(|_| {
                                                                               BAD_ARGUMENT
                                                                           })?;
                }
            }
        }
        // Heights can depend on resolved widths and nested aggregate requirements.
        for i in (0..plan.len()).rev()
        {
            if !plan[i].children.is_empty()
            {
                let query = layout::Constraints { width:  exact(plan[i].width)?,
                                                  height: layout::AxisConstraint::Unbounded };
                if plan[i].columns_cached
                   && let Some(l) = &plan[i].state.layout
                   && let Some((key, raw)) = l.measured
                   && key == query
                {
                    plan[i].constrained =
                        layout::Measurement { intrinsic:   l.sizing.resolve(raw.intrinsic),
                                              constrained: l.sizing.resolve(raw.constrained) };
                    continue;
                }
                let y = Manager::axis(&mut plan,
                                      i,
                                      true,
                                      true,
                                      layout::AxisConstraint::Unbounded,
                                      None,
                                      author).await?;
                let intrinsic = plan[i].intrinsic.intrinsic;
                let raw_intrinsic = plan[i].state
                                           .layout
                                           .as_ref()
                                           .and_then(|l| l.intrinsic)
                                           .ok_or(CORRUPT_DATA)?
                                           .intrinsic;
                let raw = if plan[i].state.layout.as_ref().is_some_and(|l| l.scroll.is_some())
                {
                    raw_intrinsic
                }
                else
                {
                    requirements(i64::from(raw_intrinsic.minimum().width().pixels()),
                                 y.0,
                                 i64::from(raw_intrinsic.preferred().width().pixels()),
                                 y.1)?
                };
                let query = layout::Constraints { width:  exact(plan[i].width)?,
                                                  height: layout::AxisConstraint::Unbounded };
                let l = plan[i].state.layout.as_mut().ok_or(CORRUPT_DATA)?;
                let raw_intrinsic = l.intrinsic.ok_or(CORRUPT_DATA)?.intrinsic;
                l.measured = Some((query,
                                   layout::Measurement { intrinsic:   raw_intrinsic,
                                                         constrained: raw }));
                plan[i].constrained = layout::Measurement { intrinsic,
                                                            constrained: l.sizing.resolve(raw) };
            }
        }
        let answer = plan[0].constrained;
        if !assign
        {
            let l = plan[0].state.layout.as_mut().ok_or(CORRUPT_DATA)?;
            let (_, raw) = l.measured.ok_or(CORRUPT_DATA)?;
            l.measured = Some((constraints, raw));
        }
        if assign
        {
            if let Some((viewport, anchors, border)) = window
            {
                let preferred = plan[0].constrained.constrained.preferred().height().pixels();
                let (start, extent) = super::window::axis(viewport.bottom,
                                                          border.top(),
                                                          border.bottom(),
                                                          preferred,
                                                          anchors.top,
                                                          anchors.bottom);
                plan[0].rect.top = start;
                plan[0].rect.bottom = start + extent;
            }
            else
            {
                plan[0].rect = requested_rect;
            }
            plan[0].screen = root_screen.unwrap_or_else(|| scrolling::logical(plan[0].rect));
            plan[0].clip = inherited_clip;
            for i in 0..plan.len()
            {
                let (rect, hidden) = scrolling::project(plan[i].screen, plan[i].clip)?;
                plan[i].rect = rect;
                plan[i].state.layout.as_mut().ok_or(CORRUPT_DATA)?.hidden = hidden;
                let height = layout::LayoutRect::try_from(rect).map_err(|e| e.status())?
                                                               .size()
                                                               .height()
                                                               .pixels();
                let scrolling = plan[i].state.layout.as_ref().is_some_and(|l| l.scroll.is_some());
                if !plan[i].children.is_empty()
                {
                    plan[i].rows_cached = plan[i].columns_cached
                                          && ((scrolling
                                               && !plan[i].state
                                                          .layout
                                                          .as_ref()
                                                          .and_then(|l| l.grid.as_ref())
                                                          .is_some_and(|c| c.percent_axis(1)))
                                              || i32::from(plan[i].before.bounds().bottom)
                                                 - i32::from(plan[i].before.bounds().top)
                                                 == i32::from(height));
                    if plan[i].rows_cached
                    {
                        if let Some(d) = plan[i].state
                                                .layout
                                                .as_ref()
                                                .and_then(|l| l.outlines.as_ref())
                                                .and_then(|g| g.dividers.as_ref())
                        {
                            if d.tracks[1].len() != plan[i].rows.tracks.len()
                            {
                                return Err(CORRUPT_DATA);
                            }
                            let tracks = d.tracks[1].clone();
                            plan[i].rows.tracks.copy_from_slice(&tracks);
                        }
                        for (offset, child) in plan[i].children.clone().enumerate()
                        {
                            let old =
                                plan[child].state.layout.as_ref().ok_or(CORRUPT_DATA)?.logical;
                            plan[i].rows.items[offset].position = old.top;
                            plan[i].rows.items[offset].extent = old.bottom - old.top;
                        }
                    }
                    else
                    {
                        let constraint = if scrolling
                        {
                            layout::AxisConstraint::Unbounded
                        }
                        else
                        {
                            exact(height)?
                        };
                        Manager::axis(&mut plan,
                                      i,
                                      true,
                                      true,
                                      constraint,
                                      Some(i64::from(height)),
                                      author).await?;
                    }
                }
                if let Some(old) = plan[i].state.layout.as_ref().and_then(|l| l.scroll.as_ref())
                {
                    let mut next = old.as_ref().clone();
                    let width = if plan[i].columns_cached
                    {
                        old.data.width
                    }
                    else
                    {
                        plan[i].columns.tracks.last().map_or(Ok(0), |t| {
                                                          t.position
                                                           .checked_add(t.assigned)
                                                           .ok_or(BAD_ARGUMENT)
                                                      })?
                    };
                    let height = if plan[i].rows_cached
                    {
                        old.data.height
                    }
                    else
                    {
                        plan[i].rows.tracks.last().map_or(Ok(0), |t| {
                                                       t.position
                                                        .checked_add(t.assigned)
                                                        .ok_or(BAD_ARGUMENT)
                                                   })?
                    };
                    next.arrange(rect, width, height)?;
                    if next.data != old.data
                    {
                        plan[i].state.layout.as_mut().ok_or(CORRUPT_DATA)?.scroll =
                            Some(Arc::try_new(next).map_err(|_| NO_MEMORY)?);
                        plan[i].outline_changed = true;
                    }
                }
                let l = plan[i].state.layout.as_ref().ok_or(CORRUPT_DATA)?;
                let (sx, sy) = l.scroll.as_ref().map_or((0, 0), |s| (s.data.x, s.data.y));
                let clip = tree::clip(tree::clip(plan[i].clip, l.clip()), Some(l.viewport(rect)));
                for (offset, child) in plan[i].children.clone().enumerate()
                {
                    let x = plan[i].columns.items[offset];
                    let y = plan[i].rows.items[offset];
                    let logical = wire::LogicalRect { left:   x.position,
                                                      top:    y.position,
                                                      right:  x.position
                                                               .checked_add(x.extent)
                                                               .ok_or(BAD_ARGUMENT)?,
                                                      bottom: y.position
                                                               .checked_add(y.extent)
                                                               .ok_or(BAD_ARGUMENT)? };
                    plan[child].state.layout.as_mut().ok_or(CORRUPT_DATA)?.logical = logical;
                    plan[child].screen = Manager::translate(logical, plan[i].screen.left,
                                                            plan[i].screen.top, sx, sy)?;
                    plan[child].clip = clip;
                }
            }
        }
        let mut drawings = Vec::new();
        if assign
        {
            for i in 0..plan.len()
            {
                if !plan[i].table
                {
                    continue;
                }
                let mut cells = Vec::new();
                let mut logical_cells = Vec::new();
                cells.try_reserve_exact(plan[i].children.len()).map_err(|_| NO_MEMORY)?;
                logical_cells.try_reserve_exact(plan[i].children.len()).map_err(|_| NO_MEMORY)?;
                for (offset, child) in plan[i].children.clone().enumerate()
                {
                    let n = plan[child].node;
                    let (x0, x1) =
                        Manager::cell_axis(&plan[i], offset, n.cell.column, n.span.columns, false)?;
                    let (y0, y1) =
                        Manager::cell_axis(&plan[i], offset, n.cell.row, n.span.rows, true)?;
                    let logical = wire::LogicalRect { left:   x0,
                                                      top:    y0,
                                                      right:  x1,
                                                      bottom: y1 };
                    let (sx, sy) = plan[i].state
                                          .layout
                                          .as_ref()
                                          .and_then(|l| l.scroll.as_ref())
                                          .map_or((0, 0), |s| (s.data.x, s.data.y));
                    let screen = Manager::translate(logical, plan[i].screen.left,
                                                    plan[i].screen.top, sx, sy)?;
                    let viewport =
                        plan[i].state.layout.as_ref().ok_or(CORRUPT_DATA)?.viewport(plan[i].rect);
                    cells.push(Manager::outline(screen, viewport));
                    logical_cells.push(logical);
                }
                let dividers = if plan[i].state
                                         .layout
                                         .as_ref()
                                         .and_then(|l| l.grid.as_ref())
                                         .is_some_and(|c| !c.boundaries.is_empty())
                {
                    let clone_tracks =
                        |source: &[wire::GridTrack]| -> Result<Arc<Vec<wire::GridTrack>>, Status> {
                            let mut v = Vec::new();
                            v.try_reserve_exact(source.len()).map_err(|_| NO_MEMORY)?;
                            v.extend_from_slice(source);
                            Arc::try_new(v).map_err(|_| NO_MEMORY)
                        };
                    let tracks = [clone_tracks(&plan[i].columns.tracks)?,
                                  clone_tracks(&plan[i].rows.tracks)?];
                    Some(Manager::divider_snapshot(&plan[i], &plan, &logical_cells, tracks)?)
                }
                else
                {
                    None
                };
                let l = plan[i].state.layout.as_mut().ok_or(CORRUPT_DATA)?;
                if dividers.is_some()
                   || l.outlines.as_ref().is_some_and(|g| g.dividers.is_some())
                   || l.outlines.as_ref().map_or(&[][..], |g| g.cells.as_slice())
                      != cells.as_slice()
                   || l.outlines.as_ref().map_or(&[][..], |g| g.logical.as_slice())
                      != logical_cells.as_slice()
                {
                    l.outlines =
                        Some(Arc::try_new(super::super::snapshot::Grid { cells, logical: logical_cells, dividers }).map_err(|_| {
                                                                                     NO_MEMORY
                                                                                 })?);
                    plan[i].outline_changed = true;
                }
            }

            drawings.try_reserve_exact(plan.len()).map_err(|_| NO_MEMORY)?;
            for i in 0..plan.len()
            {
                let entry = plan[i].entry;
                let mut payload = wire::Payload { message: wire::ASSIGN_RECT,
                                                  rect: plan[i].rect,
                                                  ..Default::default() };
                plan[i].state.reflow_with(&mut payload, entry, author).await?;
                if plan[i].before != plan[i].state.spec
                   || plan[i].outline_changed
                   || plan[i].before_hidden != plan[i].state.layout.as_ref().map(|l| l.hidden)
                {
                    let state = {
                        let mut wire = plan[i].state.spec.draw();
                        wire.reflow = 1;
                        super::super::effects::DetachedCall::capture(&wire)?
                    };
                    let (_, drawing) =
                        author.prepare(state, entry, plan[i].state.title.as_ref()).await?;
                    let mut drawing = drawing.ok_or(BAD_ABI)?;
                    drawing.id = plan[i].id;
                    drawing.grid = plan[i].state.layout.as_ref().and_then(|l| l.outlines.clone());
                    let clip = tree::clip(plan[i].clip,
                                          plan[i].state.layout.as_ref().and_then(Layout::clip));
                    drawing.set_visible(clip);
                    Manager::scroll_drawing(&plan[i].state, &mut drawing)?;
                    drawings.push(drawing);
                }
            }
        }
        if let Some((viewport, _, _)) = window
        {
            plan[0].state.layout.as_mut().ok_or(CORRUPT_DATA)?.window_extent =
                Some((viewport.right, viewport.bottom));
        }
        Ok(Prepared { request: None,
                      retired_binding: None,
                      metadata: None,
                      items: plan,
                      drawings,
                      measurement: answer.into(),
                      limited_damage: None,
                      feedback: None,
                      parent,
                      assign })
    }
}

/// Detached actual scrolling observations; no Manager/Canvas/CPU executor is
/// borrowed by the calculation, and the original source is never reselected.
pub(super) enum ScrollSnapshot
{
    Ready(Prepared, super::super::effects::DetachedCall),
    Plan
    {
        initial:        State,
        changing:       bool,
        layout:         LayoutSnapshot,
        fallback:       Option<Prepared>,
        call:           super::super::effects::DetachedCall,
        clip:           Option<Rect>,
        screen:         wire::LogicalRect,
        inherited_clip: Option<Rect>,
        reveal:         Option<wire::LogicalRect>,
        other_tracking: bool
    }
}
impl ScrollSnapshot
{
    pub(super) async fn calculate(
        self, author: &super::super::effects::Author)
        -> Result<(Prepared, super::super::effects::DetachedCall), Status>
    {
        let Self::Plan { initial,
                         changing,
                         layout,
                         fallback,
                         call,
                         clip,
                         screen,
                         inherited_clip,
                         reveal,
                         other_tracking } = self
        else
        {
            let Self::Ready(prepared, call) = self
            else
            {
                return Err(BAD_ABI);
            };
            return Ok((prepared, call));
        };
        let mut call = call;
        let current = initial.layout.as_ref().ok_or(NOT_SUPPORTED)?;
        let root_entry = match &layout
        {
            LayoutSnapshot::Plan { plan,
                                   .. } => plan.first().ok_or(BAD_ABI)?.entry,
            LayoutSnapshot::Ready(_) =>
            {
                fallback.as_ref().and_then(|p| p.items.first()).ok_or(BAD_ABI)?.entry
            }
        };
        let mut capture = None;
        if matches!(call.operation, BEGIN_TRACKING | POLL_TRACKING)
           && (!current.arrangement_pending || initial.tracking.is_some())
        {
            let mut state = initial.clone();
            let old = current.scroll.as_ref().ok_or(NOT_SUPPORTED)?.as_ref().clone();
            let mut scroll = old.clone();
            let changed = state.scroll_tracking_with(&mut call, &mut scroll, clip, author).await?;
            if scroll.data == old.data
            {
                let mut prepared = Prepared::metadata(call.id, state.clone());
                if changed
                {
                    let entry = root_entry;
                    let request = {
                        let mut wire = state.spec.draw();
                        wire.reflow = 1;
                        super::super::effects::DetachedCall::capture(&wire)?
                    };
                    let (_, drawing) = author.prepare(request, entry, state.title.as_ref()).await?;
                    let mut drawing = drawing.ok_or(BAD_ABI)?;
                    drawing.id = call.id;
                    drawing.grid = state.layout.as_ref().and_then(|l| l.outlines.clone());
                    Manager::scroll_drawing(&state, &mut drawing)?;
                    drawing.set_visible(clip);
                    prepared.drawings.try_reserve_exact(1).map_err(|_| NO_MEMORY)?;
                    let axis = initial.scroll_axis.or(state.scroll_axis).ok_or(CORRUPT_DATA)?;
                    let bar = scroll.bar(state.spec.bounds(), axis, 0, true).ok_or(CORRUPT_DATA)?;
                    prepared.limited_damage = Some(clip.map_or(bar.bounds, |clip| {
                                                           super::super::intersection(bar.bounds,
                                                                                      clip)
                                                       }));
                    prepared.drawings.push(drawing);
                }
                return Ok((prepared, call));
            }
            capture = Some((state, scroll));
        }
        if call.operation == REFLOW_OBJECT
           && !current.arrangement_pending
           && !changing
           && let Some(old) = &current.scroll
        {
            let mut next = old.as_ref().clone();
            if call.layout.message == wire::SCROLL_TO
            {
                next.position(call.layout.scroll.x, call.layout.scroll.y)?;
            }
            else if call.layout.message == wire::REVEAL_CHILD
            {
                next.reveal(reveal.ok_or(BAD_ARGUMENT)?)?;
            }
            if next.data == old.data
            {
                return Ok((Prepared::empty(), call));
            }
        }
        let mut prepared = layout.calculate(author).await?;
        if let Some(l) = prepared.items.first().and_then(|i| i.state.layout.as_ref())
        {
            call.layout.invalidation = l.pending();
            if call.relation.parent == 0 && !l.arrangement_pending
            {
                call.layout.status = OK;
            }
        }
        if call.layout.message == wire::SET_SCROLLING
        {
            return Ok((prepared, call));
        }
        if prepared.items.is_empty()
        {
            prepared = fallback.ok_or(BAD_ABI)?;
        }
        if prepared.items.iter().any(|i| {
                                    i.state.tracking.is_some()
                                    && !(i.id == call.id && call.operation == POLL_TRACKING)
                                })
        {
            return Err(SERVICE_BUSY);
        }
        if call.operation == BEGIN_TRACKING && other_tracking
        {
            return Err(SERVICE_BUSY);
        }
        let old = prepared.items[0].state
                                   .layout
                                   .as_ref()
                                   .and_then(|l| l.scroll.as_ref())
                                   .ok_or(NOT_SUPPORTED)?
                                   .as_ref()
                                   .clone();
        let mut next = old.clone();
        let tracking_changed = if let Some((state, scroll)) = capture
        {
            prepared.items[0].state = state;
            next = scroll;
            true
        }
        else if matches!(call.operation, BEGIN_TRACKING | POLL_TRACKING)
        {
            prepared.items[0].state.scroll_tracking_with(&mut call, &mut next, clip, author).await?
        }
        else
        {
            false
        };
        match call.layout.message
        {
            _ if matches!(call.operation, BEGIN_TRACKING | POLL_TRACKING) => (),
            wire::SCROLL_TO => next.position(call.layout.scroll.x, call.layout.scroll.y)?,
            wire::REVEAL_CHILD =>
            {
                let child =
                    prepared.items.iter().find(|i| i.id == call.layout.child).ok_or(NOT_FOUND)?;
                next.reveal(child.state.layout.as_ref().ok_or(NOT_SUPPORTED)?.logical)?;
            }
            _ => return Err(NOT_SUPPORTED)
        }
        if next.data == old.data && !tracking_changed
        {
            return Ok((prepared, call));
        }
        prepared.items[0].state.layout.as_mut().ok_or(CORRUPT_DATA)?.scroll =
            Some(Arc::try_new(next).map_err(|_| NO_MEMORY)?);
        Manager::translate_plan_with(&mut prepared, screen, inherited_clip, author).await?;
        // A translation preserves pending/cached content requirements. It must
        // not invalidate ancestors as if their established sizes had changed.
        prepared.assign = false;
        Ok((prepared, call))
    }
}
