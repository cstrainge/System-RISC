//! Target-only scenarios through public SDK, real loaded definitions and ECALLs.
use mac_toolbox::{controls::{self, Anchors, Cell, Reflow, Span, StandardKind, StandardState,
                             TrackSize},
                  *};

use super::conformance::Failure;
fn require(value: bool, message: &'static str) -> Result<(), Failure>
{
    if value { Ok(()) } else { Err(Failure::Mismatch(message)) }
}
fn size(w: i16, h: i16) -> controls::Size
{
    abi::Dimensions { width:  w,
                      height: h }.try_into()
                                 .unwrap()
}
fn rect(r: Rect) -> controls::LayoutRect
{
    r.try_into().unwrap()
}
fn button<'a>(s: &System<'a>, window: WindowId, title: &str, minimum: (i16, i16),
              preferred: (i16, i16))
              -> Result<controls::Control<'a>, Failure>
{
    let owned = s.controls().create_standard(window,
                                              StandardKind::Button,
                                              StandardState::new(Rect::new(4, 4, 104, 32),
                                                                 title),
                                              Reflow::Yes)?;
    owned.control().set_size_preferences(controls::SizePreferences { minimum: Some(size(minimum.0, minimum.1)), preferred: Some(size(preferred.0, preferred.1)) })?;
    Ok(owned)
}
fn adopt<'a>(t: controls::TableRef<'a>, cell: Cell, span: Span, child: controls::Control<'a>)
             -> Result<controls::ControlRef<'a>, Failure>
{
    t.set_child_spanning(cell, span, child).map_err(|e| Failure::Service(e.cause.status()))
}
fn bounds(c: controls::ControlRef<'_>) -> Result<Rect, Failure>
{
    Ok(c.info()?.standard.bounds)
}
fn ledger(s: &System<'_>) -> Result<inspection::Pool, Failure>
{
    let snapshot = s.inspection().snapshot(inspection::Scope::Own).map_err(|e| e.cause)?;
    let ledger = snapshot.ledger(0).map_err(|e| e.cause)?;
    for i in 0..ledger.len()
    {
        let p = ledger.at(i).map_err(|e| e.cause)?;
        if p.kind == *b"CTRL"
        {
            return Ok(p);
        }
    }
    Err(Failure::Mismatch("grid CTRL ledger missing"))
}
fn usage(s: &System<'_>) -> Result<(u64, u64, u64, u64), Failure>
{
    let domain = s.inspection()
                  .snapshot(inspection::Scope::Own)
                  .map_err(|e| e.cause)?
                  .at(0)
                  .map_err(|e| e.cause)?;
    match domain.accounting
    {
        inspection::MemoryAccounting::ExecutionAndResidentHeap { kernel,
                                                                 application,
                                                                 .. } =>
        {
            Ok((kernel.reserved, kernel.committed, application.reserved, application.committed))
        }
        _ => Err(Failure::Mismatch("grid resident heap accounting unavailable"))
    }
}
pub fn run(s: &System<'_>) -> Result<(), Failure>
{
    let before = ledger(s)?;
    let window =
        s.windows()
         .create_owned(windows::WindowSpec::new(Rect::new(40, 60, 740, 520), "Table grid"))?;
    window.select()?;
    let t = s.controls().create_table(window.id(), Rect::new(10, 10, 310, 70), Reflow::Yes)?;
    let table = t.table();
    let a = adopt(table,
                  Cell { row:    0,
                         column: 0 },
                  Span::ONE,
                  button(s, window.id(), "Fit", (120, 20), (200, 28))?)?;
    let b = adopt(table,
                  Cell { row:    0,
                         column: 1 },
                  Span::ONE,
                  button(s, window.id(), "Fill", (150, 20), (200, 28))?)?;
    a.set_anchors(Anchors::ALL)?;
    b.set_anchors(Anchors::ALL)?;
    table.set_column(1, TrackSize::Fill)?;
    table.set_row(0, TrackSize::Fill)?;
    require(table.column(0)? == TrackSize::Fit && table.column(1)? == TrackSize::Fill,
            "track configuration not retained")?;
    let req = t.control().ask_size(Default::default())?;
    require(req.intrinsic.minimum() == size(270, 20)
            && req.intrinsic.preferred() == size(400, 28),
            "grid intrinsic aggregation incorrect")?;
    t.control().assign_rect(rect(Rect::new(10, 10, 310, 70)))?;
    require(bounds(a)? == Rect::new(10, 10, 160, 70) && bounds(b)? == Rect::new(160, 10, 310, 70),
            "300 budget did not allocate 150/150")?;
    t.control().assign_rect(rect(Rect::new(10, 10, 510, 70)))?;
    require(bounds(a)? == Rect::new(10, 10, 210, 70) && bounds(b)? == Rect::new(210, 10, 510, 70),
            "500 budget did not allocate 200/300")?;
    let old = bounds(b)?;
    b.set_anchors(Anchors::NONE)?;
    require(bounds(b)? == old && t.control().info()?.arrangement_pending,
            "anchor setter assigned eagerly or lost parent dirtiness")?;
    t.control().assign_rect(rect(Rect::new(10, 10, 510, 70)))?;
    require(bounds(b)? == Rect::new(260, 26, 460, 54), "unanchored child did not center")?;
    b.set_anchors(Anchors { right: true,
                            bottom: true,
                            ..Anchors::NONE })?;
    t.control().assign_rect(rect(Rect::new(10, 10, 510, 70)))?;
    require(bounds(b)? == Rect::new(310, 42, 510, 70), "trailing anchors did not pin")?;
    b.set_anchors(Anchors { left: true,
                            top: true,
                            ..Anchors::NONE })?;
    t.control().assign_rect(rect(Rect::new(10, 10, 510, 70)))?;
    require(bounds(b)? == Rect::new(210, 10, 410, 38), "leading anchors did not pin")?;
    b.set_anchors(Anchors::ALL)?;
    t.control().assign_rect(rect(Rect::new(10, 10, 260, 20)))?;
    require(bounds(a)? == Rect::new(10, 10, 121, 20) && bounds(b)? == Rect::new(121, 10, 260, 20),
            "forced shortage did not shrink deterministically")?;
    require(t.control().ask_size(Default::default())?.intrinsic.minimum() == size(270, 20),
            "forced shrink concealed true minima")?;
    t.control().assign_rect(rect(Rect::new(10, 10, 10, 10)))?;
    require(bounds(a)?.is_empty()
            && bounds(b)?.is_empty()
            && table.hit_test(Point { h: 10,
                                      v: 10 })?
                    .is_none(),
            "zero assignment retained invisible hit")?;
    t.control().assign_rect(rect(Rect::new(10, 10, 510, 70)))?;
    require(!t.control().info()?.arrangement_pending, "grid assignment left work pending")?;
    // Identity assignment must not cancel an active child capture.
    require(a.begin_tracking(Point { h: 30,
                                     v: 30 },
                             100)?
             .tracking,
            "grid capture failed")?;
    t.control().assign_rect(rect(Rect::new(10, 10, 510, 70)))?;
    require(a.info()?.tracking, "identity assignment cancelled capture")?;
    let old_a = bounds(a)?;
    let old_b = bounds(b)?;
    require(t.control().assign_rect(rect(Rect::new(20, 20, 520, 80))) == Err(Error::ServiceBusy),
            "subtree layout crossed capture")?;
    require(bounds(a)? == old_a && bounds(b)? == old_b,
            "capture refusal published partial geometry")?;
    a.cancel()?;
    t.control().assign_rect(rect(Rect::new(20, 20, 520, 80)))?;
    // Override clearing must expose recalculated aggregate values, not a cache
    // containing the previous explicit override.
    t.control().set_preferred(size(600, 100))?;
    require(t.control().ask_size(Default::default())?.intrinsic.preferred() == size(600, 100),
            "Table preferred override ignored")?;
    t.control().clear_preferred()?;
    require(t.control().ask_size(Default::default())?.intrinsic.preferred() == size(400, 28),
            "Table override poisoned intrinsic cache")?;
    b.set_minimum(size(80, 20))?;
    require(t.control().ask_size(Default::default())?.intrinsic.minimum() == size(200, 20),
            "changed child minimum did not invalidate parent")?;
    let constrained=controls::Constraints { width:controls::AxisConstraint::Exact(controls::Extent::new(300).unwrap()), height:controls::AxisConstraint::Unbounded };
    t.control().set_preferred(size(600, 100))?;
    require(t.control().ask_size(constrained)?.constrained.preferred() == size(600, 100),
            "constrained Table override ignored")?;
    t.control().clear_preferred()?;
    require(t.control().ask_size(constrained)?.constrained.preferred() == size(400, 28),
            "cleared override survived in constrained cache")?;
    table.set_column(0, TrackSize::Percent(6000))?;
    t.control().assign_rect(rect(Rect::new(10, 10, 510, 70)))?;
    require(bounds(a)? == Rect::new(10, 10, 310, 70) && bounds(b)? == Rect::new(310, 10, 510, 70),
            "percent target did not precede Fill allocation")?;
    table.set_column(0, TrackSize::Fit)?;
    table.set_column(1, TrackSize::Fit)?;
    t.control().assign_rect(rect(Rect::new(10, 10, 310, 70)))?;
    require(bounds(a)? == Rect::new(10, 10, 170, 70) && bounds(b)? == Rect::new(170, 10, 310, 70),
            "multiple Fit tracks did not share their preferred deficits")?;
    let mut invalid = abi::controls::ControlCall { id:a.id().0, operation:abi::controls::REFLOW_OBJECT,
        layout:abi::controls::reflow::Payload { message:abi::controls::reflow::SET_ANCHORS, anchors:16, ..Default::default() }, ..Default::default() };
    require(s.client().control(&mut invalid) == Err(Error::BadArgument)
            && a.info()?.anchors == Anchors::ALL,
            "malformed anchors changed state")?;
    s.log("REFLOW_GRID fit-fill=150/150,200/300 anchors=center,edges,stretch shortage=111/139 \
           zero=no-hit capture=rollback-retry cache=override-clear\n")?;
    drop(t);

    let grid = s.controls().create_table(window.id(), Rect::new(10, 100, 610, 400), Reflow::Yes)?;
    let g = grid.table();
    for col in 0..3
    {
        g.set_column(col, TrackSize::Fill)?;
    }
    for row in 0..3
    {
        g.set_row(row, TrackSize::Fill)?;
    }
    let spanning = adopt(g,
                         Cell { row:    0,
                                column: 0 },
                         Span::new(2, 2).unwrap(),
                         button(s, window.id(), "Merged 2 x 2", (80, 40), (160, 60))?)?;
    let right = adopt(g,
                      Cell { row:    0,
                             column: 2 },
                      Span::new(2, 1).unwrap(),
                      button(s, window.id(), "Two rows", (40, 40), (80, 60))?)?;
    let bottom = adopt(g,
                       Cell { row:    2,
                              column: 0 },
                       Span::new(1, 3).unwrap(),
                       button(s, window.id(), "Three columns", (120, 20), (240, 28))?)?;
    spanning.set_anchors(Anchors::ALL)?;
    right.set_anchors(Anchors::ALL)?;
    let blocked = g.set_child(Cell { row:    1,
                                     column: 1 },
                              button(s, window.id(), "Overlap", (0, 0), (20, 20))?)
                   .err()
                   .ok_or(Failure::Mismatch("span overlap accepted"))?;
    require(blocked.cause == Error::AlreadyExists
            && blocked.child.control().info()?.parent.is_none(),
            "span refusal lost owner")?;
    let mut message = super::LogBuffer::new();
    core::fmt::write(&mut message, format_args!("{blocked}")).map_err(|_| Failure::Mismatch("adoption diagnostic truncated"))?;
    require(message.text().contains("overlap") && message.text().contains("ownership retained"),
            "span refusal lacks cause or ownership diagnostic")?;
    drop(blocked.child);
    require(g.reparent_spanning(right,
                                g,
                                Cell { row:    1,
                                       column: 1 },
                                Span::ONE)
            == Err(Error::AlreadyExists),
            "span reparent overlap accepted")?;
    require(right.info()?.parent == Some(grid.control().id()),
            "span reparent refusal lost parent")?;
    grid.control().assign_rect(rect(Rect::new(10, 100, 610, 400)))?;
    let sb = bounds(spanning)?;
    let rb = bounds(right)?;
    let bb = bounds(bottom)?;
    require(sb.left == 10
            && sb.top == 100
            && sb.right == rb.left
            && rb.right == 610
            && sb.bottom == rb.bottom,
            "merged columns or rows misplaced")?;
    require(bb.left == 190 && bb.right == 430 && bb.top > sb.bottom && bb.bottom < 400,
            "merged-cell centering incorrect")?;
    require(g.children().next().ok_or(Failure::Mismatch("span cursor empty"))??.span
            == Span::new(2, 2).unwrap(),
            "span enumeration lost dimensions")?;
    g.reparent_spanning(spanning,
                        g,
                        Cell { row:    0,
                               column: 0 },
                        Span::new(1, 2).unwrap())?;
    let released = adopt(g,
                         Cell { row:    1,
                                column: 0 },
                         Span::new(1, 2).unwrap(),
                         button(s, window.id(), "Freed row", (80, 20), (160, 28))?)?;
    grid.control().assign_rect(rect(Rect::new(10, 100, 610, 400)))?;
    require(bounds(released)?.top >= bounds(spanning)?.bottom,
            "span reduction did not free the vacated row")?;
    drop(g.remove_child(released)?);
    g.reparent_spanning(spanning,
                        g,
                        Cell { row:    0,
                               column: 0 },
                        Span::new(2, 2).unwrap())?;
    // A nested table participates in the same subtree transaction and receives
    // its parent's resolved width before its own row and anchor allocation.
    drop(g.remove_child(right)?);
    let nested = s.controls().create_table(window.id(), Rect::new(0, 0, 100, 50), Reflow::Yes)?;
    nested.table().set_column(0, TrackSize::Fill)?;
    nested.table().set_row(0, TrackSize::Fill)?;
    let deep = adopt(nested.table(),
                     Cell { row:    0,
                            column: 0 },
                     Span::ONE,
                     button(s, window.id(), "Nested", (40, 20), (80, 28))?)?;
    deep.set_anchors(Anchors::ALL)?;
    let nested_ref = adopt(g,
                           Cell { row:    0,
                                  column: 2 },
                           Span::new(2, 1).unwrap(),
                           nested.into_control())?;
    nested_ref.set_anchors(Anchors::ALL)?;
    grid.control().assign_rect(rect(Rect::new(10, 100, 610, 400)))?;
    require(bounds(deep)? == bounds(nested_ref)?, "nested grid was not arranged with outer grid")?;
    require(g.hit_test(Point { h: bounds(deep)?.left + 5,
                               v: bounds(deep)?.top + 5 })?
             .is_some_and(|h| h.control.id() == deep.id()),
            "nested assigned hit did not follow pixels")?;
    let arranged = bounds(deep)?;
    deep.assign_rect(rect(Rect::new(arranged.left,
                                    arranged.top,
                                    arranged.left + 20,
                                    arranged.top + 20)))?;
    require(grid.control().info()?.arrangement_pending,
            "manual child assignment did not invalidate outer arrangement")?;
    grid.control().assign_rect(rect(Rect::new(10, 100, 610, 400)))?;
    require(bounds(deep)? == arranged, "unchanged outer rectangle did not restore child layout")?;
    deep.invalidate_layout(controls::Invalidation::Arrangement)?;
    require(grid.control().info()?.arrangement_pending,
            "child arrangement invalidation did not reach root")?;
    grid.control().assign_rect(rect(Rect::new(10, 100, 610, 400)))?;
    require(!deep.info()?.arrangement_pending,
            "subtree assignment left explicit invalidation pending")?;
    grid.control().assign_rect(rect(Rect::new(20, 110, 620, 410)))?;
    require(bounds(deep)?
            == Rect::new(arranged.left + 10,
                         arranged.top + 10,
                         arranged.right + 10,
                         arranged.bottom + 10),
            "cached nested translation lost offset")?;
    grid.control().assign_rect(rect(Rect::new(10, 100, 610, 400)))?;
    s.log("REFLOW_GRID_PAINT_READY spans=2x2,1x3 nested=assigned\n")?;
    s.client().wait_event(0, 120)?;
    // Span changes preserve stable child IDs and reject zero/overflow wire input.
    let mut call=abi::controls::ControlCall { id:grid.control().id().0,operation:abi::controls::REFLOW_OBJECT,
        layout:abi::controls::reflow::Payload { message:abi::controls::reflow::REPARENT,child:spanning.id().0,parent:grid.control().id().0,
            cell:abi::controls::reflow::Cell{row:u32::MAX,column:0},span:abi::controls::reflow::Span{rows:2,columns:1},..Default::default() },..Default::default() };
    require(s.client().control(&mut call) == Err(Error::BadArgument),
            "span endpoint overflow accepted")?;
    let sb = bounds(spanning)?;
    call.layout.cell.row = 0;
    call.layout.span.rows = 0;
    require(s.client().control(&mut call) == Err(Error::BadArgument) && bounds(spanning)? == sb,
            "zero span altered layout")?;
    g.set_column(0, TrackSize::Percent(6000))?;
    require(g.set_column(1, TrackSize::Percent(5000)) == Err(Error::BadArgument)
            && g.column(1)? == TrackSize::Fill,
            "percent overcommit not atomic")?;
    g.set_column(0, TrackSize::Fill)?;
    // A clean identity assignment crosses no publication boundary.
    grid.control().assign_rect(rect(Rect::new(10, 100, 610, 400)))?;
    // Public window paint transaction excludes subtree publication.
    s.windows().begin_update(window.id())?;
    grid.control().assign_rect(rect(Rect::new(10, 100, 610, 400)))?;
    require(grid.control().assign_rect(rect(Rect::new(20, 100, 620, 400)))
            == Err(Error::ServiceBusy),
            "grid crossed pending paint")?;
    s.windows().end_update(window.id())?;
    grid.control().assign_rect(rect(Rect::new(20, 100, 620, 400)))?;
    // Fill the real retained-command limit, then require late admission failure
    // to preserve every descendant and permit retry after clearing the scene.
    s.windows().begin_update(window.id())?;
    let port = s.quickdraw().port(window.id());
    let mut refused = false;
    for _ in 0..513
    {
        match port.paint_rect(Rect::new(0, 0, 1, 1), 0xffffff)
        {
            Ok(()) => (),
            Err(Error::NoMemory) =>
            {
                refused = true;
                break;
            }
            Err(e) => return Err(e.into())
        }
    }
    require(refused, "grid admission fixture did not fill scene")?;
    s.windows().end_update(window.id())?;
    let root_old = bounds(grid.control())?;
    let deep_old = bounds(deep)?;
    let merged_old = bounds(spanning)?;
    let admission_usage = usage(s)?;
    require(grid.control().assign_rect(rect(Rect::new(30, 110, 630, 410)))
            == Err(Error::NoMemory),
            "grid did not refuse full retained scene")?;
    require(bounds(grid.control())? == root_old
            && bounds(deep)? == deep_old
            && bounds(spanning)? == merged_old,
            "grid admission published partial subtree")?;
    require(usage(s)? == admission_usage, "refused subtree retained resident heap charges")?;
    s.windows().begin_update(window.id())?;
    s.windows().end_update(window.id())?;
    grid.control().assign_rect(rect(Rect::new(30, 110, 630, 410)))?;
    require(bounds(deep)? != deep_old, "grid admission retry did not commit")?;
    // Geometry/track staging itself can be refused by the actual allocation
    // budget, without invoking an alternate allocator or fault-injection path.
    g.set_column(1_000_000_000, TrackSize::Fill)?;
    let prior = bounds(grid.control())?;
    let staging_usage = usage(s)?;
    require(grid.control().assign_rect(rect(Rect::new(40, 110, 640, 410)))
            == Err(Error::NoMemory),
            "huge grid staging did not return NoMemory")?;
    require(bounds(grid.control())? == prior, "staging failure changed geometry")?;
    require(usage(s)? == staging_usage, "failed grid allocation retained resident heap charges")?;
    g.set_column(1_000_000_000, TrackSize::Fit)?;
    grid.control().assign_rect(rect(Rect::new(40, 110, 640, 410)))?;
    drop(grid);
    require(matches!(deep.info(), Err(Error::NotFound)), "nested grid ref survived disposal")?;
    drop(window);
    let after = ledger(s)?;
    require((after.live, after.pending) == (before.live, before.pending)
            && after.births - before.births == after.deaths - before.deaths,
            "grid cleanup leaked CTRL ownership")?;
    s.log("REFLOW_GRID_NATIVE_DONE spans=overlap,zero,overflow-refused nested=atomic \
           retained=NoMemory-rollback-retry staging=NoMemory-rollback-retry \
           resident-charges=refunded ledger=balanced\n")?;
    Ok(())
}
