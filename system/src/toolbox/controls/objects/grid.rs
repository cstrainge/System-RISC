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
    parent:          Option<usize>,
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
    columns:         Axis,
    rows:            Axis
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

    /// All fallible allocation, CDEF preparation and retained admission precede
    /// publication. A no-op returns before resolving images or allocating a plan.
    pub(crate) fn request_grid(&mut self, call: &mut ControlCall, owner: u64, paint_pending: bool,
                               gate: TrapGate,
                               mut resolve: impl FnMut(i16) -> Result<DefinitionEntry, Status>,
                               mut publish: impl FnMut(&mut Vec<super::super::Drawing>)
                                     -> Result<(), Status>)
                               -> Result<bool, Status>
    {
        if call.layout.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        let root = self.table(call.id, owner)?;
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
        if assign
           && root.state.spec.bounds() == call.layout.rect
           && !root_layout.arrangement_pending
        {
            return Ok(false);
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
                call.layout.measurement = layout::Measurement { intrinsic: root_layout.sizing.resolve(measured.intrinsic), constrained: root_layout.sizing.resolve(measured.constrained) }.into();
                return Ok(false);
            }
        }
        let mut plan = Vec::new();
        self.append_plan(&mut plan, call.id, None, owner, &mut resolve)?;
        let mut index = 0;
        while index < plan.len()
        {
            let start = plan.len();
            let mut id = plan[index].node.first;
            while id != 0
            {
                let next = self.get(id, owner)?.tree.next;
                self.append_plan(&mut plan, id, Some(index), owner, &mut resolve)?;
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
                let raw = requirements(x.0, y.0, x.1, y.1)?;
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
                    && i32::from(plan[i].before.bounds().right)
                       - i32::from(plan[i].before.bounds().left)
                       == i32::from(plan[i].width);
                plan[i].columns_cached = can_reuse;
                if can_reuse
                {
                    for (offset, child) in plan[i].children.clone().enumerate()
                    {
                        let old = plan[child].before.bounds();
                        plan[i].columns.items[offset].position =
                            i64::from(old.left) - i64::from(plan[i].before.bounds().left);
                        plan[i].columns.items[offset].extent =
                            i64::from(old.right) - i64::from(old.left);
                    }
                }
                else
                {
                    let width = exact(plan[i].width)?;
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
                let raw = requirements(i64::from(raw_intrinsic.minimum().width().pixels()),
                                       y.0,
                                       i64::from(raw_intrinsic.preferred().width().pixels()),
                                       y.1)?;
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
            plan[0].rect = call.layout.rect;
            for i in 0..plan.len()
            {
                if !plan[i].children.is_empty()
                {
                    let height =
                        layout::LayoutRect::try_from(plan[i].rect).map_err(|e| e.status())?
                                                                  .size()
                                                                  .height()
                                                                  .pixels();
                    plan[i].rows_cached = plan[i].columns_cached
                                          && i32::from(plan[i].before.bounds().bottom)
                                             - i32::from(plan[i].before.bounds().top)
                                             == i32::from(height);
                    if plan[i].rows_cached
                    {
                        for (offset, child) in plan[i].children.clone().enumerate()
                        {
                            let old = plan[child].before.bounds();
                            plan[i].rows.items[offset].position =
                                i64::from(old.top) - i64::from(plan[i].before.bounds().top);
                            plan[i].rows.items[offset].extent =
                                i64::from(old.bottom) - i64::from(old.top);
                        }
                    }
                    else
                    {
                        Self::axis(&mut plan, i, true, true, exact(height)?, gate)?;
                    }
                    for (offset, child) in plan[i].children.clone().enumerate()
                    {
                        let x = plan[i].columns.items[offset];
                        let y = plan[i].rows.items[offset];
                        let left = i64::from(plan[i].rect.left).checked_add(x.position)
                                                               .ok_or(BAD_ARGUMENT)?;
                        let top = i64::from(plan[i].rect.top).checked_add(y.position)
                                                             .ok_or(BAD_ARGUMENT)?;
                        let right = left.checked_add(x.extent).ok_or(BAD_ARGUMENT)?;
                        let bottom = top.checked_add(y.extent).ok_or(BAD_ARGUMENT)?;
                        plan[child].rect =
                            Rect::new(i16::try_from(left).map_err(|_| BAD_ARGUMENT)?,
                                      i16::try_from(top).map_err(|_| BAD_ARGUMENT)?,
                                      i16::try_from(right).map_err(|_| BAD_ARGUMENT)?,
                                      i16::try_from(bottom).map_err(|_| BAD_ARGUMENT)?);
                    }
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
                cells.try_reserve_exact(plan[i].children.len()).map_err(|_| NO_MEMORY)?;
                for (offset, child) in plan[i].children.clone().enumerate()
                {
                    let n = plan[child].node;
                    let (x0, x1) =
                        Self::cell_axis(&plan[i], offset, n.cell.column, n.span.columns, false)?;
                    let (y0, y1) =
                        Self::cell_axis(&plan[i], offset, n.cell.row, n.span.rows, true)?;
                    let left = i64::from(plan[i].rect.left).checked_add(x0).ok_or(BAD_ARGUMENT)?;
                    let top = i64::from(plan[i].rect.top).checked_add(y0).ok_or(BAD_ARGUMENT)?;
                    let right = i64::from(plan[i].rect.left).checked_add(x1).ok_or(BAD_ARGUMENT)?;
                    let bottom = i64::from(plan[i].rect.top).checked_add(y1).ok_or(BAD_ARGUMENT)?;
                    cells.push(Rect::new(i16::try_from(left).map_err(|_| BAD_ARGUMENT)?,
                                         i16::try_from(top).map_err(|_| BAD_ARGUMENT)?,
                                         i16::try_from(right).map_err(|_| BAD_ARGUMENT)?,
                                         i16::try_from(bottom).map_err(|_| BAD_ARGUMENT)?));
                }
                let l = plan[i].state.layout.as_mut().ok_or(CORRUPT_DATA)?;
                if l.outlines.as_ref().map_or(&[][..], |g| g.cells.as_slice()) != cells.as_slice()
                {
                    l.outlines =
                        Some(Arc::try_new(super::super::snapshot::Grid { cells }).map_err(|_| {
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
                if plan[i].before != plan[i].state.spec || plan[i].outline_changed
                {
                    let mut state = plan[i].state.spec.draw();
                    state.reflow = 1;
                    let mut drawing = super::super::prepare_with_title(&mut state, entry, gate, plan[i].state.title.as_ref())?.ok_or(BAD_ABI)?;
                    drawing.id = plan[i].id;
                    drawing.grid = plan[i].state.layout.as_ref().and_then(|l| l.outlines.clone());
                    let mut clip = plan[i].state.layout.as_ref().and_then(|l| l.visible);
                    let mut parent = plan[i].parent;
                    while let Some(p) = parent
                    {
                        clip = tree::clip(clip, Some(plan[p].rect));
                        clip =
                            tree::clip(clip, plan[p].state.layout.as_ref().and_then(|l| l.visible));
                        parent = plan[p].parent;
                    }
                    clip = tree::clip(clip, self.inherited_clip(call.id));
                    drawing.set_visible(clip);
                    drawings.push(drawing);
                }
            }
        }
        // Admission closure may fail, but then no live state or scene has changed.
        let changed = !drawings.is_empty();
        let parent = self.get(call.id, owner)?.tree.parent;
        for item in &plan
        {
            self.get(item.id, owner)?;
        }
        publish(&mut drawings)?;
        for staged in plan
        {
            // The held desktop guard and nonreentrant invocation contract keep
            // these IDs live. No fallible operation follows scene publication.
            if let Some(object) = self.objects.get_mut(staged.id)
            {
                object.state = staged.state;
            }
        }
        if assign
        {
            self.invalidate_arrangement_ancestors(parent);
        }
        call.layout.measurement = answer.into();
        Ok(changed)
    }

    fn append_plan(&self, plan: &mut Vec<Item>, id: u64, parent: Option<usize>, owner: u64,
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
                         parent,
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
                          .and_then(|g| g.cells.get(offset))
                          .ok_or(CORRUPT_DATA)?;
            let base = item.before.bounds();
            return Ok(if vertical
                      {
                          (i64::from(old.top) - i64::from(base.top),
                           i64::from(old.bottom) - i64::from(base.top))
                      }
                      else
                      {
                          (i64::from(old.left) - i64::from(base.left),
                           i64::from(old.right) - i64::from(base.left))
                      });
        }
        let axis = if vertical { &item.rows } else { &item.columns };
        let first = axis.tracks.get(start as usize).ok_or(CORRUPT_DATA)?;
        let end = start.checked_add(span).and_then(|n| n.checked_sub(1)).ok_or(CORRUPT_DATA)?;
        let last = axis.tracks.get(end as usize).ok_or(CORRUPT_DATA)?;
        Ok((first.position, last.position.checked_add(last.assigned).ok_or(BAD_ARGUMENT)?))
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
