//! Event-boundary layout through real services and disk-loaded definitions.
use mac_toolbox::{controls::{self, Anchors, Cell, Reflow, SizeBounds, StandardKind, StandardState},
                  *};

use super::conformance::Failure;

fn require(ok: bool, text: &'static str) -> Result<(), Failure>
{
    if ok { Ok(()) } else { Err(Failure::Mismatch(text)) }
}
fn size(w: i16, h: i16) -> controls::Size
{
    controls::Size::new(controls::Extent::new(w).unwrap(), controls::Extent::new(h).unwrap())
}
fn bounds(c: controls::ControlRef<'_>) -> Result<Rect, Failure>
{
    Ok(c.info()?.standard.bounds)
}
fn width(c: controls::ControlRef<'_>) -> Result<i16, Failure>
{
    let b = bounds(c)?;
    Ok(b.right - b.left)
}
fn poll(s: &System<'_>) -> Result<(), Failure>
{
    s.client().next_event(0)?;
    Ok(())
}
fn button<'a>(s: &System<'a>, w: WindowId, text: &str, preferred: i16)
              -> Result<controls::Control<'a>, Failure>
{
    let c = s.controls().create_standard(w,
                                          StandardKind::Button,
                                          StandardState::new(Rect::new(0, 0, 60, 24), text),
                                          Reflow::Yes)?;
    c.control()
     .set_size_preferences(controls::SizePreferences { minimum:   Some(size(20, 18)),
                                                       preferred: Some(size(preferred, 24)) })?;
    Ok(c)
}
fn ledger(s: &System<'_>) -> Result<inspection::Pool, Failure>
{
    let snap = s.inspection().snapshot(inspection::Scope::Own).map_err(|e| e.cause)?;
    let l = snap.ledger(0).map_err(|e| e.cause)?;
    for i in 0..l.len()
    {
        let p = l.at(i).map_err(|e| e.cause)?;
        if p.kind == *b"CTRL"
        {
            return Ok(p);
        }
    }
    Err(Failure::Mismatch("automatic layout CTRL ledger missing"))
}
fn redraw(s: &System<'_>, w: WindowId, controls: &[controls::ControlRef<'_>])
          -> Result<(), Failure>
{
    s.windows().begin_update(w)?;
    for c in controls
    {
        c.draw()?;
    }
    s.windows().end_update(w)?;
    Ok(())
}
pub fn run(s: &System<'_>) -> Result<(), Failure>
{
    let before = ledger(s)?;
    let w = s.windows().create_owned(windows::WindowSpec::new(Rect::new(30, 70, 680, 460),
                                                               "Automatic Table reflow"))?;
    w.select()?;
    let table = s.controls().create_table(w.id(), Rect::new(10, 10, 310, 150), Reflow::Yes)?;
    let a = table.table()
                 .set_child(Cell { row:    0,
                                   column: 0 },
                            button(s, w.id(), "A", 60)?)
                 .map_err(|e| Failure::Service(e.cause.status()))?;
    let nested = s.controls().create_table(w.id(), Rect::new(0, 0, 120, 60), Reflow::Yes)?;
    let b = nested.table()
                  .set_child(Cell { row:    0,
                                    column: 0 },
                             button(s, w.id(), "Nested", 60)?)
                  .map_err(|e| Failure::Service(e.cause.status()))?;
    let n = table.table()
                 .set_child(Cell { row:    0,
                                   column: 1 },
                            nested.into_control())
                 .map_err(|e| Failure::Service(e.cause.status()))?;
    a.set_anchors(Anchors::ALL)?;
    n.set_anchors(Anchors::ALL)?;
    table.table().set_column(1, SizeBounds::FILL)?;
    let other = s.controls().create_table(w.id(), Rect::new(330, 10, 620, 150), Reflow::Yes)?;
    let c = other.table()
                 .set_child(Cell { row:    0,
                                   column: 0 },
                            button(s, w.id(), "Other", 70)?)
                 .map_err(|e| Failure::Service(e.cause.status()))?;
    let roots = [table.control(), a, n, b, other.control(), c];
    // No explicit assignment or root anchors are needed for a positioned Table.
    poll(s)?;
    require(width(a)? == 60 && width(n)? == 240 && width(c)? == 70,
            "automatic initial Table assignment missing")?;
    let old = bounds(a)?;
    for x in [80, 100, 120]
    {
        a.set_preferred(size(x, 24))?;
    }
    require(bounds(a)? == old && table.control().info()?.arrangement_pending,
            "setters did not coalesce until event boundary")?;
    poll(s)?;
    require(width(a)? == 120 && width(n)? == 180 && !table.control().info()?.arrangement_pending,
            "automatic coalesced preferences not arranged")?;
    b.set_preferred(size(110, 24))?;
    poll(s)?;
    require(width(b)? == 110 && width(a)? == 120, "nested automatic reflow lost leaf preference")?;
    // Real content, not only size overrides, invalidates measurement.
    a.clear_preferred()?;
    poll(s)?;
    let mut state = a.info()?.standard;
    let area = state.bounds;
    state = StandardState::new(area, "Content changed to a longer button");
    a.configure(StandardKind::Button, state)?;
    require(table.control().info()?.measurement_pending
            && table.control().info()?.arrangement_pending,
            "content change did not invalidate root measurement")?;
    let wanted = a.ask_size(Default::default())?.intrinsic.preferred().width().pixels();
    poll(s)?;
    require(width(a)? == wanted && wanted == 60 && !table.control().info()?.arrangement_pending,
            "content invalidation did not consume the fixed leaf measurement")?;
    a.set_preferred(size(100, 24))?;
    poll(s)?;
    // An unrelated captured root neither moves nor blocks another root.
    let cb = bounds(c)?;
    require(c.begin_tracking(Point { h: cb.left + 3,
                                     v: cb.top + 3 },
                             20)?
             .tracking,
            "automatic capture setup failed")?;
    a.set_preferred(size(130, 24))?;
    poll(s)?;
    require(width(a)? == 130 && c.info()?.tracking && bounds(c)? == cb,
            "automatic work disturbed an unrelated capture")?;
    c.set_preferred(size(100, 24))?;
    poll(s)?;
    require(other.control().info()?.layout_error == Some(Error::ServiceBusy)
            && c.info()?.tracking
            && bounds(c)? == cb,
            "captured automatic root was not deferred")?;
    for _ in 0..4
    {
        poll(s)?;
    }
    require(bounds(c)? == cb && c.info()?.tracking, "deferred polling disturbed capture")?;
    c.cancel()?;
    poll(s)?;
    require(width(c)? == 100 && other.control().info()?.layout_error.is_none(),
            "capture release did not wake automatic layout")?;
    // Paint publication and layout publication stay separate transactions.
    let old = bounds(a)?;
    s.windows().begin_update(w.id())?;
    a.set_preferred(size(150, 24))?;
    poll(s)?;
    require(bounds(a)? == old && table.control().info()?.layout_error == Some(Error::ServiceBusy),
            "automatic layout crossed open painting")?;
    for control in roots
    {
        control.draw()?;
    }
    s.windows().end_update(w.id())?;
    require(bounds(a)? == old, "EndUpdate unexpectedly arranged before event boundary")?;
    poll(s)?;
    require(width(a)? == 150 && table.control().info()?.layout_error.is_none(),
            "EndUpdate did not wake deferred layout")?;
    s.log("AUTO_REFLOW_COALESCED content,nested,capture,paint=observed\n")?;
    // Actual retained-command capacity refuses publication; no replacement
    // allocator or test-only failure hook is involved.
    s.windows().begin_update(w.id())?;
    let mut full = false;
    for _ in 0..513
    {
        match s.quickdraw().port(w.id()).paint_rect(Rect::new(0, 0, 1, 1), 0xffffff)
        {
            Ok(()) => (),
            Err(Error::NoMemory) =>
            {
                full = true;
                break;
            }
            Err(e) => return Err(e.into())
        }
    }
    s.windows().end_update(w.id())?;
    require(full, "automatic layout pressure did not fill retained scene")?;
    a.set_preferred(size(170, 24))?;
    poll(s)?;
    require(width(a)? == 150
            && table.control().info()?.arrangement_pending
            && table.control().info()?.layout_error == Some(Error::NoMemory),
            "automatic admission failure published geometry or lost cause")?;
    let start = s.events().uptime_ticks()?;
    s.client().wait_event(0, 600)?;
    let elapsed = s.events().uptime_ticks()?.saturating_sub(start);
    require(elapsed <= 120 && width(a)? == 150,
            "automatic retry was not bounded or changed refused geometry")?;
    redraw(s, w.id(), &roots)?;
    poll(s)?;
    require(width(a)? == 170 && table.control().info()?.layout_error.is_none(),
            "released retained pressure did not resume automatic layout")?;
    // Scratch admission failure and correction use the same public track setter.
    table.table().set_column(1_000_000_000, SizeBounds::FILL)?;
    poll(s)?;
    require(table.control().info()?.layout_error == Some(Error::NoMemory) && width(a)? == 170,
            "automatic scratch refusal changed geometry")?;
    table.table().set_column(1_000_000_000, SizeBounds::CONTENT)?;
    poll(s)?;
    require(table.control().info()?.layout_error.is_none(), "automatic scratch retry failed")?;
    // Removing/reparenting pending controls consumes only their current tree.
    a.set_anchors(Anchors::NONE)?;
    a.set_preferred(size(90, 24))?;
    table.table().reparent(a,
                            other.table(),
                            Cell { row:    1,
                                   column: 0 })?;
    poll(s)?;
    require(a.info()?.parent == Some(other.control().id()) && width(a)? == 90,
            "automatic pending reparent used the old root")?;
    let detached = other.table().remove_child(a)?;
    drop(detached);
    poll(s)?;
    require(matches!(a.info(), Err(Error::NotFound)), "automatic detached child was not stale")?;
    // A standalone anchored leaf follows content preference changes too.
    let leaf = button(s, w.id(), "Pinned", 80)?;
    leaf.control().set_anchors(Anchors { right: true,
                                          bottom: true,
                                          ..Anchors::NONE })?;
    let before_leaf = bounds(leaf.control())?;
    leaf.control().set_preferred(size(130, 24))?;
    poll(s)?;
    let after_leaf = bounds(leaf.control())?;
    require(after_leaf.right == before_leaf.right
            && after_leaf.bottom == before_leaf.bottom
            && after_leaf.right - after_leaf.left == 130,
            "anchored leaf did not follow automatic content sizing")?;
    // Dispose a queued root before the poll, then reuse storage for a new root.
    let temporary = s.controls().create_table(w.id(), Rect::new(10, 200, 200, 260), Reflow::Yes)?;
    let old_id = temporary.control().id();
    temporary.table().set_column(0, SizeBounds::FILL)?;
    drop(temporary);
    let replacement =
        s.controls().create_table(w.id(), Rect::new(10, 200, 200, 260), Reflow::Yes)?;
    poll(s)?;
    require(replacement.control().id() != old_id
            && matches!(s.controls().control(old_id).info(), Err(Error::NotFound)),
            "queued root disposal transferred work to reused identity")?;
    drop(replacement);
    drop(leaf);
    drop(other);
    drop(table);
    drop(w);
    poll(s)?;
    let after = ledger(s)?;
    require((before.live, before.pending) == (after.live, after.pending)
            && after.births - before.births == after.deaths - before.deaths,
            "automatic layout did not reclaim control ownership")?;
    s.log("AUTO_REFLOW_NATIVE_DONE coalesced,nested,content,root=arranged \
           capture,paint=deferred retained,scratch=refused-retried \
           reparent,dispose,reuse=observed ledger=balanced\n")?;
    Ok(())
}
