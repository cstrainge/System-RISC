//! Fixed content scrolling shares the canonical CTRL tree and layout admission.
//! Only viewport translation changes on a clean scroll; content remains cached.
use mac_abi::controls::reflow as wire;

use super::*;

#[derive(Clone, Copy)]
pub(super) struct Scroll
{
    pub data:  wire::Scroll,
    pub entry: DefinitionEntry
}
impl Scroll
{
    pub fn new(axes: u32, entry: DefinitionEntry) -> Result<Self, Status>
    {
        if axes == 0 || axes & !3 != 0
        {
            return Err(BAD_ARGUMENT);
        }
        Ok(Self { data: wire::Scroll { axes,
                                       ..Default::default() },
                  entry })
    }

    pub fn arrange(&mut self, bounds: Rect, width: i64, height: i64) -> Result<(), Status>
    {
        if width < 0 || height < 0
        {
            return Err(BAD_ARGUMENT);
        }
        self.data.width = width;
        self.data.height = height;
        // Least consistent set, in deterministic horizontal-before-vertical
        // order. Fixed content is independent of every candidate viewport.
        for bars in [0, 1, 2, 3]
        {
            if bars & !self.data.axes != 0
            {
                continue;
            }
            let vw = (i32::from(bounds.right)
                      - i32::from(bounds.left)
                      - if bars & 2 != 0 { 16 } else { 0 }).max(0);
            let vh = (i32::from(bounds.bottom)
                      - i32::from(bounds.top)
                      - if bars & 1 != 0 { 16 } else { 0 }).max(0);
            let required = (u32::from(width > i64::from(vw))
                            | u32::from(height > i64::from(vh)) << 1)
                           & self.data.axes;
            if required == bars
            {
                self.data.bars = bars;
                self.data.viewport =
                    Rect::new(bounds.left,
                              bounds.top,
                              i16::try_from(i32::from(bounds.left) + vw).map_err(|_| {
                                                                            BAD_ARGUMENT
                                                                        })?,
                              i16::try_from(i32::from(bounds.top) + vh).map_err(|_| BAD_ARGUMENT)?);
                self.position(self.data.x, self.data.y)?;
                return Ok(());
            }
        }
        Err(CORRUPT_DATA)
    }

    pub fn maximum(self, axis: usize) -> i64
    {
        let d = self.data;
        if d.axes & (1 << axis) == 0
        {
            return 0;
        }
        if axis == 0
        {
            (d.width - (i64::from(d.viewport.right) - i64::from(d.viewport.left))).max(0)
        }
        else
        {
            (d.height - (i64::from(d.viewport.bottom) - i64::from(d.viewport.top))).max(0)
        }
    }

    pub fn position(&mut self, x: i64, y: i64) -> Result<(), Status>
    {
        if x < 0 || y < 0
        {
            return Err(BAD_ARGUMENT);
        }
        self.data.x = x.min(self.maximum(0));
        self.data.y = y.min(self.maximum(1));
        Ok(())
    }

    pub fn bar(self, bounds: Rect, axis: usize, highlight: i16, enabled: bool)
               -> Option<ScrollbarState>
    {
        if self.data.bars & (1 << axis) == 0
        {
            return None;
        }
        let v = self.data.viewport;
        let rect = if axis == 0
        {
            Rect::new(bounds.left, v.bottom, v.right, bounds.bottom)
        }
        else
        {
            Rect::new(v.right, bounds.top, bounds.right, v.bottom)
        };
        let position = if axis == 0 { self.data.x } else { self.data.y };
        let max = self.maximum(axis);
        let value =
            if max == 0 { 0 } else { (i128::from(position) * 32767 / i128::from(max)) as i16 };
        Some(ScrollbarState { bounds: rect,
                              value,
                              maximum: if max == 0 { 0 } else { 32767 },
                              active: u32::from(enabled),
                              highlighted: highlight,
                              reserved: 0 })
    }

    pub fn reveal(&mut self, rect: wire::LogicalRect) -> Result<(), Status>
    {
        fn position(old: i64, start: i64, end: i64, extent: i64) -> i64
        {
            if start < old || end - start > extent
            {
                start
            }
            else if end > old.saturating_add(extent)
            {
                end - extent
            }
            else
            {
                old
            }
        }
        let v = self.data.viewport;
        self.position(position(self.data.x,
                               rect.left,
                               rect.right,
                               i64::from(v.right) - i64::from(v.left)),
                      position(self.data.y,
                               rect.top,
                               rect.bottom,
                               i64::from(v.bottom) - i64::from(v.top)))
    }
}

pub(super) fn logical(rect: Rect) -> wire::LogicalRect
{
    wire::LogicalRect { left:   rect.left.into(),
                        top:    rect.top.into(),
                        right:  rect.right.into(),
                        bottom: rect.bottom.into() }
}

/// Fully hidden controls retain finite local geometry but have an empty clip.
/// A partially visible unrepresentable CDEF rectangle refuses before commit.
pub(super) fn project(rect: wire::LogicalRect, clip: Option<Rect>) -> Result<(Rect, bool), Status>
{
    let hidden = clip.is_some_and(|c| {
                         rect.right <= i64::from(c.left)
                         || rect.left >= i64::from(c.right)
                         || rect.bottom <= i64::from(c.top)
                         || rect.top >= i64::from(c.bottom)
                         || c.is_empty()
                     });
    let rect = if hidden
    {
        wire::LogicalRect { right: rect.right.checked_sub(rect.left).ok_or(BAD_ARGUMENT)?,
                            bottom: rect.bottom.checked_sub(rect.top).ok_or(BAD_ARGUMENT)?,
                            ..Default::default() }
    }
    else
    {
        rect
    };
    Ok((Rect::new(i16::try_from(rect.left).map_err(|_| BAD_ARGUMENT)?,
                  i16::try_from(rect.top).map_err(|_| BAD_ARGUMENT)?,
                  i16::try_from(rect.right).map_err(|_| BAD_ARGUMENT)?,
                  i16::try_from(rect.bottom).map_err(|_| BAD_ARGUMENT)?),
        hidden))
}

impl Layout
{
    pub(super) fn clip(&self) -> Option<Rect>
    {
        if self.hidden { Some(Rect::ZERO) } else { self.visible }
    }

    pub(super) fn viewport(&self, bounds: Rect) -> Rect
    {
        self.scroll.as_ref().map_or(bounds, |s| s.data.viewport)
    }
}

impl Manager
{
    /// Recover the accepted screen origin through canonical content edges,
    /// including ancestors whose drawing rectangles are hidden placeholders.
    pub(super) fn screen_rect(&self, id: u64, owner: u64) -> Result<wire::LogicalRect, Status>
    {
        let object = self.get(id, owner)?;
        if object.tree.parent == 0
        {
            return Ok(logical(object.state.spec.bounds()));
        }
        let mut rect = object.state.layout.as_ref().ok_or(NOT_SUPPORTED)?.logical;
        let mut parent = object.tree.parent;
        while parent != 0
        {
            let object = self.get(parent, owner)?;
            let l = object.state.layout.as_ref().ok_or(CORRUPT_DATA)?;
            let base = if object.tree.parent == 0
            {
                logical(object.state.spec.bounds())
            }
            else
            {
                l.logical
            };
            let (sx, sy) = l.scroll.as_ref().map_or((0, 0), |s| (s.data.x, s.data.y));
            let x = base.left.checked_sub(sx).ok_or(BAD_ARGUMENT)?;
            let y = base.top.checked_sub(sy).ok_or(BAD_ARGUMENT)?;
            rect = wire::LogicalRect { left:   rect.left.checked_add(x).ok_or(BAD_ARGUMENT)?,
                                       right:  rect.right.checked_add(x).ok_or(BAD_ARGUMENT)?,
                                       top:    rect.top.checked_add(y).ok_or(BAD_ARGUMENT)?,
                                       bottom: rect.bottom.checked_add(y).ok_or(BAD_ARGUMENT)? };
            parent = object.tree.parent;
        }
        Ok(rect)
    }

    pub(crate) fn scrolling(&self, id: u64, owner: u64) -> Result<bool, Status>
    {
        Ok(self.get(id, owner)?.state.layout.as_ref().is_some_and(|l| l.scroll.is_some()))
    }

    pub(super) fn scroll_drawing(state: &State, drawing: &mut super::super::Drawing)
                                 -> Result<(), Status>
    {
        let Some(scroll) = state.layout.as_ref().and_then(|l| l.scroll.as_ref())
        else
        {
            return Ok(());
        };
        if scroll.data.bars == 0
        {
            return Ok(());
        }
        let bounds = state.spec.bounds();
        let enabled = state.spec.draw().standard.enabled != 0;
        let bars = core::array::from_fn(|axis| {
            scroll.bar(bounds,
                       axis,
                       if state.scroll_axis == Some(axis) { state.scroll_highlight } else { 0 },
                       enabled)
        });
        drawing.bars = Some(alloc::boxed::Box::try_new(super::super::snapshot::Bars {
            bars, entry: scroll.entry, viewport: scroll.data.viewport
        }).map_err(|_| NO_MEMORY)?);
        Ok(())
    }

    pub(crate) fn scroll_query(&self, call: &mut ControlCall, owner: u64) -> Result<(), Status>
    {
        let table = self.table(call.id, owner)?;
        if call.layout.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        let l = table.state.layout.as_ref().ok_or(NOT_SUPPORTED)?;
        match call.layout.message
        {
            wire::GET_SCROLLING =>
            {
                call.layout.scroll =
                    l.scroll.as_ref().map_or(wire::Scroll { viewport: table.state.spec.bounds(),
                                                            ..Default::default() },
                                             |s| s.data);
            }
            wire::GET_LOGICAL_RECT =>
            {
                let child = self.get(call.layout.child, owner)?;
                if child.tree.parent != call.id
                {
                    return Err(BAD_ARGUMENT);
                }
                call.layout.scroll.logical =
                    child.state.layout.as_ref().ok_or(NOT_SUPPORTED)?.logical;
            }
            _ => return Err(NOT_SUPPORTED)
        }
        Ok(())
    }
}

impl State
{
    pub(super) fn scroll_hit(&self, point: Point, gate: TrapGate)
                             -> Result<Option<(usize, i16)>, Status>
    {
        if self.layout.as_ref().and_then(Layout::clip).is_some_and(|c| !c.contains(point))
        {
            return Ok(None);
        }
        let Some(scroll) = self.layout.as_ref().and_then(|l| l.scroll.as_ref())
        else
        {
            return Ok(None);
        };
        for axis in 0..2
        {
            if let Some(scrollbar) =
                scroll.bar(self.spec.bounds(), axis, 0, self.spec.draw().standard.enabled != 0)
            {
                let mut call = ControlCall { operation: HIT_SCROLLBAR,
                                             reflow: 1,
                                             scrollbar,
                                             point,
                                             ..Default::default() };
                super::super::prepare(&mut call, scroll.entry, gate)?;
                if call.result != 0
                {
                    return Ok(Some((axis, call.result as i16)));
                }
            }
        }
        Ok(None)
    }

    pub(super) fn scroll_tracking(&mut self, call: &mut ControlCall, scroll: &mut Scroll,
                                  clip: Option<Rect>, gate: TrapGate)
                                  -> Result<bool, Status>
    {
        let before =
            (self.scroll_axis, self.scroll_highlight, self.tracking.is_some(), scroll.data);
        let mut action = Action::None;
        let mut part = 0;
        let mut handled = false;
        if call.operation == BEGIN_TRACKING
        {
            if self.tracking.is_some()
            {
                return Err(SERVICE_BUSY);
            }
            if !clip.is_some_and(|c| !c.contains(call.point))
               && let Some((axis, hit)) = self.scroll_hit(call.point, gate)?
            {
                part = hit;
                handled = true;
                let bar = scroll.bar(self.spec.bounds(), axis, 0, true).ok_or(CORRUPT_DATA)?;
                self.tracking = Some(Tracking::new(hit, call.point, bar.value, call.ticks));
                self.scroll_axis = Some(axis);
                self.scroll_highlight = hit;
                if hit != SCROLL_THUMB
                {
                    action = Action::Step(hit);
                }
            }
        }
        else if let Some(tracking) = self.tracking
        {
            part = tracking.part;
            handled = true;
            let axis = self.scroll_axis.ok_or(CORRUPT_DATA)?;
            let bar = scroll.bar(self.spec.bounds(), axis, self.scroll_highlight, true)
                            .ok_or(CORRUPT_DATA)?;
            let transition = tracking.poll(call.point,
                                           call.flags & BUTTON_DOWN != 0,
                                           call.ticks,
                                           Some(bar),
                                           1,
                                           scroll.entry,
                                           gate,
                                           |point| {
                                               if clip.is_some_and(|c| !c.contains(point))
                                               {
                                                   return Ok(0);
                                               }
                                               let mut hit = ControlCall { operation:
                                                                               HIT_SCROLLBAR,
                                                                           reflow: 1,
                                                                           scrollbar: bar,
                                                                           point,
                                                                           ..Default::default() };
                                               super::super::prepare(&mut hit, scroll.entry, gate)?;
                                               Ok(hit.result as i16)
                                           })?;
            action = transition.action;
            self.tracking = transition.tracking;
            self.scroll_highlight = transition.highlight;
        }
        let (old_x, old_y) = (scroll.data.x, scroll.data.y);
        if let Some(axis) = self.scroll_axis
        {
            let old = if axis == 0 { old_x } else { old_y };
            let v = scroll.data.viewport;
            let extent = if axis == 0
            {
                i64::from(v.right) - i64::from(v.left)
            }
            else
            {
                i64::from(v.bottom) - i64::from(v.top)
            };
            let next = match action
            {
                Action::Value(value) =>
                {
                    (i128::from(value) * i128::from(scroll.maximum(axis)) / 32767) as i64
                }
                Action::Step(part) =>
                {
                    let amount = if matches!(part, SCROLL_PAGE_UP | SCROLL_PAGE_DOWN)
                    {
                        (extent - 16).max(1)
                    }
                    else
                    {
                        16
                    };
                    if matches!(part, SCROLL_UP | SCROLL_PAGE_UP)
                    {
                        old.saturating_sub(amount).max(0)
                    }
                    else
                    {
                        old.saturating_add(amount)
                    }
                }
                _ => old
            };
            scroll.position(if axis == 0 { next } else { old_x },
                            if axis == 1 { next } else { old_y })?;
        }
        if self.tracking.is_none()
        {
            self.scroll_axis = None;
        }
        let changed =
            before
            != (self.scroll_axis, self.scroll_highlight, self.tracking.is_some(), scroll.data);
        call.result = part.into();
        call.flags = u32::from(handled) * HANDLED
                     | u32::from(self.tracking.is_some()) * TRACKING
                     | u32::from(changed) * CHANGED
                     | u32::from((old_x, old_y) != (scroll.data.x, scroll.data.y)) * ACTION;
        Ok(changed)
    }
}
