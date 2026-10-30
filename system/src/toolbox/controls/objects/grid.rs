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
    tracks: Vec<wire::TrackSetting>
}
#[derive(Default)]
struct Axis
{
    tracks: Vec<wire::GridTrack>,
    items:  Vec<wire::GridItem>
}
struct Item
{
    id:              u64,
    children:        Range<usize>,
    node:            tree::Node,
    state:           State,
    before:          Spec,
    entry:           DefinitionEntry,
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
    metadata:                  Option<(u64, State)>,
    items:                     Vec<Item>,
    pub(crate) drawings:       Vec<super::super::Drawing>,
    pub(crate) measurement:    wire::Measurement,
    pub(crate) limited_damage: Option<Rect>,
    parent:                    u64,
    assign:                    bool
}
impl Prepared
{
    pub(super) fn empty() -> Self
    {
        Self { metadata:       None,
               items:          Vec::new(),
               drawings:       Vec::new(),
               measurement:    Default::default(),
               limited_damage: None,
               parent:         0,
               assign:         false }
    }

    pub(super) fn metadata(id: u64, state: State) -> Self
    {
        Self { metadata: Some((id, state)),
               ..Self::empty() }
    }

    pub(super) fn extend(&mut self, mut next: Self) -> Result<(), Status>
    {
        if self.items.is_empty() && self.drawings.is_empty()
        {
            *self = next;
            return Ok(());
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
fn measure(state: &mut State, entry: DefinitionEntry, gate: TrapGate,
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
    state.reflow(&mut payload, entry, gate)?;
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
        layout::TrackSize::decode(requested.mode, requested.percent).map_err(|e| e.status())?;
        if requested == previous
        {
            return Ok(());
        }
        let old_tracks = old.map_or(&[][..], |c| c.tracks.as_slice());
        let mut percent = requested.percent;
        for t in
            old_tracks.iter().filter(|t| t.axis == requested.axis && t.index != requested.index)
        {
            percent = percent.checked_add(t.percent).ok_or(BAD_ARGUMENT)?;
        }
        if percent > 10_000
        {
            return Err(BAD_ARGUMENT);
        }
        let mut tracks = Vec::new();
        tracks.try_reserve_exact(old_tracks.len().checked_add(1).ok_or(NO_MEMORY)?)
              .map_err(|_| NO_MEMORY)?;
        tracks.extend(old_tracks.iter()
                                .filter(|t| t.axis != requested.axis || t.index != requested.index)
                                .copied());
        if requested.mode != wire::TRACK_FIT
        {
            tracks.push(requested);
        }
        tracks.sort_unstable_by_key(|t| (t.axis, t.index));
        let next = if tracks.is_empty()
        {
            None
        }
        else
        {
            Some(Arc::try_new(Configuration { tracks }).map_err(|_| NO_MEMORY)?)
        };
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

    pub(crate) fn is_table(&self, id: u64, owner: u64) -> Result<bool, Status>
    {
        let o = self.get(id, owner)?;
        Ok(o.state.layout.is_some()
           && matches!(o.state.spec,
                       Spec::Standard { procedure: TABLE_PROCEDURE,
                                        .. }))
    }

    pub(crate) fn prepare_layout(&self, call: &ControlCall, owner: u64, paint_pending: bool,
                                 gate: TrapGate,
                                 resolve: impl FnMut(i16) -> Result<DefinitionEntry, Status>,
                                 window: Option<(Rect, layout::Anchors, layout::BorderOffsets)>)
                                 -> Result<Prepared, Status>
    {
        self.prepare_layout_change(call, owner, paint_pending, gate, resolve, window, None)
    }

    /// Prepare with cloned state only. The caller admits all retained output
    /// before committing; multiple roots can share one window transaction.
    fn prepare_layout_change(&self, call: &ControlCall, owner: u64, paint_pending: bool,
                             gate: TrapGate,
                             mut resolve: impl FnMut(i16) -> Result<DefinitionEntry, Status>,
                             window: Option<(Rect, layout::Anchors, layout::BorderOffsets)>,
                             scroll_change: Option<Option<Arc<scrolling::Scroll>>>)
                             -> Result<Prepared, Status>
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
           && assign
           && root.state.spec.bounds() == call.layout.rect
           && !root_layout.arrangement_pending
        {
            return Ok(Prepared::empty());
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
                return Ok(prepared);
            }
        }
        let mut plan = Vec::new();
        self.append_plan(&mut plan, call.id, owner, &mut resolve)?;
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
                self.append_plan(&mut plan, id, owner, &mut resolve)?;
                id = next;
            }
            plan[index].children = start..plan.len();
            if assign && plan[index].state.tracking.is_some()
            {
                return Err(SERVICE_BUSY);
            }
            index += 1;
        }
        // One intrinsic bottom-up pass; leaves reuse cached content requirements.
        for i in (0..plan.len()).rev()
        {
            if plan[i].children.is_empty()
            {
                let entry = plan[i].entry;
                let measured = measure(&mut plan[i].state, entry, gate, Default::default())?;
                plan[i].intrinsic = measured;
            }
            else
            {
                Self::allocate_axes(&mut plan, i)?;
                if let Some(cached) = plan[i].state.layout.as_ref().and_then(|l| l.intrinsic)
                {
                    let l = plan[i].state.layout.as_ref().ok_or(CORRUPT_DATA)?;
                    plan[i].intrinsic =
                        layout::Measurement { intrinsic:   l.sizing.resolve(cached.intrinsic),
                                              constrained: l.sizing.resolve(cached.constrained) };
                    continue;
                }
                let x = Self::axis(&mut plan,
                                   i,
                                   false,
                                   false,
                                   layout::AxisConstraint::Unbounded,
                                   gate)?;
                let y =
                    Self::axis(&mut plan, i, true, false, layout::AxisConstraint::Unbounded, gate)?;
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
                plan[i].constrained = measure(&mut plan[i].state, entry, gate, query)?;
            }
            else
            {
                let can_reuse =
                    plan[i].state
                           .layout
                           .as_ref()
                           .is_some_and(|l| !l.arrangement_pending && l.outlines.is_some())
                    && (plan[i].state.layout.as_ref().is_some_and(|l| l.scroll.is_some())
                        || i32::from(plan[i].before.bounds().right)
                           - i32::from(plan[i].before.bounds().left)
                           == i32::from(plan[i].width));
                plan[i].columns_cached = can_reuse;
                if can_reuse
                {
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
                    Self::axis(&mut plan, i, false, false, width, gate)?;
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
                let y =
                    Self::axis(&mut plan, i, true, true, layout::AxisConstraint::Unbounded, gate)?;
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
                plan[0].rect = call.layout.rect;
            }
            plan[0].screen = if root.tree.parent != 0
                                && call.layout.rect == root.state.spec.bounds()
            {
                self.screen_rect(call.id, owner)?
            }
            else
            {
                scrolling::logical(plan[0].rect)
            };
            plan[0].clip = self.inherited_clip(call.id);
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
                                          && (scrolling
                                              || i32::from(plan[i].before.bounds().bottom)
                                                 - i32::from(plan[i].before.bounds().top)
                                                 == i32::from(height));
                    if plan[i].rows_cached
                    {
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
                        Self::axis(&mut plan, i, true, true, constraint, gate)?;
                    }
                }
                if let Some(old) = plan[i].state.layout.as_ref().and_then(|l| l.scroll.as_ref())
                {
                    let mut next = **old;
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
                    plan[child].screen =
                        Self::translate(logical, plan[i].screen.left, plan[i].screen.top, sx, sy)?;
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
                        Self::cell_axis(&plan[i], offset, n.cell.column, n.span.columns, false)?;
                    let (y0, y1) =
                        Self::cell_axis(&plan[i], offset, n.cell.row, n.span.rows, true)?;
                    let logical = wire::LogicalRect { left:   x0,
                                                      top:    y0,
                                                      right:  x1,
                                                      bottom: y1 };
                    let (sx, sy) = plan[i].state
                                          .layout
                                          .as_ref()
                                          .and_then(|l| l.scroll.as_ref())
                                          .map_or((0, 0), |s| (s.data.x, s.data.y));
                    let screen =
                        Self::translate(logical, plan[i].screen.left, plan[i].screen.top, sx, sy)?;
                    let viewport =
                        plan[i].state.layout.as_ref().ok_or(CORRUPT_DATA)?.viewport(plan[i].rect);
                    cells.push(Self::outline(screen, viewport));
                    logical_cells.push(logical);
                }
                let l = plan[i].state.layout.as_mut().ok_or(CORRUPT_DATA)?;
                if l.outlines.as_ref().map_or(&[][..], |g| g.cells.as_slice()) != cells.as_slice()
                   || l.outlines.as_ref().map_or(&[][..], |g| g.logical.as_slice())
                      != logical_cells.as_slice()
                {
                    l.outlines =
                        Some(Arc::try_new(super::super::snapshot::Grid { cells, logical: logical_cells }).map_err(|_| {
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
                plan[i].state.reflow(&mut payload, entry, gate)?;
                if plan[i].before != plan[i].state.spec
                   || plan[i].outline_changed
                   || self.get(plan[i].id, owner)?.state.layout.as_ref().map(|l| l.hidden)
                      != plan[i].state.layout.as_ref().map(|l| l.hidden)
                {
                    let mut state = plan[i].state.spec.draw();
                    state.reflow = 1;
                    let mut drawing = super::super::prepare_with_title(&mut state, entry, gate, plan[i].state.title.as_ref())?.ok_or(BAD_ABI)?;
                    drawing.id = plan[i].id;
                    drawing.grid = plan[i].state.layout.as_ref().and_then(|l| l.outlines.clone());
                    let clip = tree::clip(plan[i].clip,
                                          plan[i].state.layout.as_ref().and_then(Layout::clip));
                    drawing.set_visible(clip);
                    Self::scroll_drawing(&plan[i].state, &mut drawing)?;
                    drawings.push(drawing);
                }
            }
        }
        if let Some((viewport, _, _)) = window
        {
            plan[0].state.layout.as_mut().ok_or(CORRUPT_DATA)?.window_extent =
                Some((viewport.right, viewport.bottom));
        }
        let parent = self.get(call.id, owner)?.tree.parent;
        Ok(Prepared { metadata: None,
                      items: plan,
                      drawings,
                      measurement: answer.into(),
                      limited_damage: None,
                      parent,
                      assign })
    }

    pub(crate) fn prepare_scroll(&self, call: &mut ControlCall, owner: u64, paint_pending: bool,
                                 gate: TrapGate,
                                 mut resolve: impl FnMut(i16) -> Result<DefinitionEntry, Status>)
                                 -> Result<Prepared, Status>
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
                return Ok(Prepared::empty());
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
        if call.layout.message == wire::REVEAL_CHILD
        {
            if self.get(call.layout.child, owner)?.tree.parent != call.id
            {
                return Err(BAD_ARGUMENT);
            }
        }
        if call.operation == BEGIN_TRACKING
           && self.objects.iter().any(|(id, o)| {
                                     o.recipient == owner
                                     && o.state.tracking.is_some()
                                     && (id != call.id || root.state.tracking.is_some())
                                 })
        {
            return Err(SERVICE_BUSY);
        }
        let mut capture = None;
        if matches!(call.operation, BEGIN_TRACKING | POLL_TRACKING)
           && (!current.arrangement_pending || root.state.tracking.is_some())
        {
            let mut state = root.state.clone();
            let old = **current.scroll.as_ref().ok_or(NOT_SUPPORTED)?;
            let mut scroll = old;
            let changed =
                state.scroll_tracking(call, &mut scroll, self.visible_clip(call.id), gate)?;
            if scroll.data == old.data
            {
                let mut prepared = Prepared::metadata(call.id, state.clone());
                if changed
                {
                    let entry = resolve(super::super::definition_id(&state.spec.draw())?)?;
                    let mut request = state.spec.draw();
                    request.reflow = 1;
                    let mut drawing =
                        super::super::prepare_with_title(&mut request,
                                                         entry,
                                                         gate,
                                                         state.title.as_ref())?.ok_or(BAD_ABI)?;
                    drawing.id = call.id;
                    drawing.grid = state.layout.as_ref().and_then(|l| l.outlines.clone());
                    Self::scroll_drawing(&state, &mut drawing)?;
                    drawing.set_visible(self.visible_clip(call.id));
                    prepared.drawings.try_reserve_exact(1).map_err(|_| NO_MEMORY)?;
                    let axis = root.state.scroll_axis.or(state.scroll_axis).ok_or(CORRUPT_DATA)?;
                    let bar = scroll.bar(state.spec.bounds(), axis, 0, true).ok_or(CORRUPT_DATA)?;
                    prepared.limited_damage =
                        Some(self.visible_clip(call.id)
                                 .map_or(bar.bounds, |clip| {
                                     super::super::intersection(bar.bounds, clip)
                                 }));
                    prepared.drawings.push(drawing);
                }
                return Ok(prepared);
            }
            capture = Some((state, scroll));
        }
        if call.operation == REFLOW_OBJECT
           && !current.arrangement_pending
           && change.is_none()
           && let Some(old) = &current.scroll
        {
            let mut next = **old;
            if call.layout.message == wire::SCROLL_TO
            {
                next.position(call.layout.scroll.x, call.layout.scroll.y)?;
            }
            else if call.layout.message == wire::REVEAL_CHILD
            {
                next.reveal(self.get(call.layout.child, owner)?
                                .state
                                .layout
                                .as_ref()
                                .ok_or(NOT_SUPPORTED)?
                                .logical)?;
            }
            if next.data == old.data
            {
                return Ok(Prepared::empty());
            }
        }
        let assignment = ControlCall { id: call.id,
                                       operation: REFLOW_OBJECT,
                                       layout: wire::Payload { message: wire::ASSIGN_RECT,
                                                               rect: root.state.spec.bounds(),
                                                               ..Default::default() },
                                       ..Default::default() };
        let mut prepared = if call.operation == POLL_TRACKING && root.state.tracking.is_some()
        {
            Prepared::empty()
        }
        else
        {
            self.prepare_layout_change(&assignment, owner, false, gate, &mut resolve, None, change)?
        };
        if let Some(l) = prepared.items.first().and_then(|i| i.state.layout.as_ref())
        {
            call.layout.invalidation = l.pending();
            if root.tree.parent == 0 && !l.arrangement_pending
            {
                call.layout.status = OK;
            }
        }
        if call.layout.message == wire::SET_SCROLLING
        {
            return Ok(prepared);
        }
        if prepared.items.is_empty()
        {
            self.append_plan(&mut prepared.items, call.id, owner, &mut resolve)?;
            let mut index = 0;
            while index < prepared.items.len()
            {
                let start = prepared.items.len();
                let mut id = prepared.items[index].node.first;
                while id != 0
                {
                    self.append_plan(&mut prepared.items, id, owner, &mut resolve)?;
                    id = self.get(id, owner)?.tree.next;
                }
                prepared.items[index].children = start..prepared.items.len();
                index += 1;
            }
            prepared.parent = root.tree.parent;
        }
        if prepared.items.iter().any(|i| {
                                    i.state.tracking.is_some()
                                    && !(i.id == call.id && call.operation == POLL_TRACKING)
                                })
        {
            return Err(SERVICE_BUSY);
        }
        if call.operation == BEGIN_TRACKING
           && self.objects.iter().any(|(id, o)| {
                                     id != call.id
                                     && o.recipient == owner
                                     && o.state.tracking.is_some()
                                 })
        {
            return Err(SERVICE_BUSY);
        }
        let old = **prepared.items[0].state
                                     .layout
                                     .as_ref()
                                     .and_then(|l| l.scroll.as_ref())
                                     .ok_or(NOT_SUPPORTED)?;
        let mut next = old;
        let tracking_changed = if let Some((state, scroll)) = capture
        {
            prepared.items[0].state = state;
            next = scroll;
            true
        }
        else if matches!(call.operation, BEGIN_TRACKING | POLL_TRACKING)
        {
            let clip = self.visible_clip(call.id);
            prepared.items[0].state.scroll_tracking(call, &mut next, clip, gate)?
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
            return Ok(prepared);
        }
        prepared.items[0].state.layout.as_mut().ok_or(CORRUPT_DATA)?.scroll =
            Some(Arc::try_new(next).map_err(|_| NO_MEMORY)?);
        self.translate_plan(&mut prepared, call.id, owner, gate)?;
        // A translation preserves pending/cached content requirements. It must
        // not invalidate ancestors as if their established sizes had changed.
        prepared.assign = false;
        Ok(prepared)
    }

    fn translate_plan(&self, prepared: &mut Prepared, root: u64, owner: u64, gate: TrapGate)
                      -> Result<(), Status>
    {
        let plan = &mut prepared.items;
        plan[0].screen = self.screen_rect(root, owner)?;
        plan[0].clip = self.inherited_clip(root);
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
                let mut next = **old;
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
                    l.outlines = Some(Arc::try_new(super::super::snapshot::Grid { cells, logical }).map_err(|_| NO_MEMORY)?);
                }
            }
            let local_clip = l.clip();
            let viewport = l.viewport(rect);
            let clip = tree::clip(tree::clip(plan[i].clip, local_clip), Some(viewport));
            for child in plan[i].children.clone()
            {
                let logical = plan[child].state.layout.as_ref().ok_or(CORRUPT_DATA)?.logical;
                plan[child].screen = Self::translate(logical, screen.left, screen.top, sx, sy)?;
                plan[child].clip = clip;
            }
            let visible = tree::clip(plan[i].clip, local_clip);
            let before = self.get(plan[i].id, owner)?;
            let old_clip = self.visible_clip(plan[i].id);
            let same_grid = match (before.state.layout.as_ref().and_then(|l| l.outlines.as_ref()),
                                   plan[i].state.layout.as_ref().and_then(|l| l.outlines.as_ref()))
            {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false
            };
            if i != 0 && before.state.spec == plan[i].state.spec && old_clip == visible && same_grid
            {
                continue;
            }
            let mut state = plan[i].state.spec.draw();
            state.reflow = 1;
            let mut drawing =
                super::super::prepare_with_title(&mut state,
                                                 plan[i].entry,
                                                 gate,
                                                 plan[i].state.title.as_ref())?.ok_or(BAD_ABI)?;
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
    pub(crate) fn commit_layout(&mut self, prepared: Prepared)
    {
        if let Some((id, state)) = prepared.metadata
        {
            let released =
                self.objects
                    .get(id)
                    .is_some_and(|o| o.state.tracking.is_some() && state.tracking.is_none());
            if let Some(object) = self.objects.get_mut(id)
            {
                object.state = state;
            }
            self.layout_committed(id);
            if released
            {
                self.wake_capture(id);
            }
        }
        for staged in prepared.items
        {
            let released =
                self.objects
                    .get(staged.id)
                    .is_some_and(|o| o.state.tracking.is_some() && staged.state.tracking.is_none());
            if let Some(object) = self.objects.get_mut(staged.id)
            {
                object.state = staged.state;
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

    fn append_plan(&self, plan: &mut Vec<Item>, id: u64, owner: u64,
                   resolve: &mut impl FnMut(i16) -> Result<DefinitionEntry, Status>)
                   -> Result<(), Status>
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
        let entry = resolve(super::super::definition_id(&o.state.spec.draw())?)?;
        plan.try_reserve(1).map_err(|_| NO_MEMORY)?;
        let zero = layout::Requirements::default();
        let empty = layout::Measurement { intrinsic:   zero,
                                          constrained: zero };
        plan.push(Item { id,
                         children: 0..0,
                         node: o.tree,
                         state: o.state.clone(),
                         before: o.state.spec,
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

    fn axis(plan: &mut [Item], index: usize, vertical: bool, constrained: bool,
            constraint: layout::AxisConstraint, gate: TrapGate)
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
                track.mode = t.mode;
                track.percent = t.percent;
            }
        }
        super::super::reflow::grid_axis(entry,
                                        gate,
                                        constraint.into(),
                                        &mut axis.tracks,
                                        &mut axis.items)
    }
}
