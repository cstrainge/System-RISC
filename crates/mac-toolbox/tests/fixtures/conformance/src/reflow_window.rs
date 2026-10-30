//! Real window-root layout through public SDK calls and optional physical-input
//! replay. No host implementation substitutes for Window Manager/CDEF execution.
use mac_toolbox::{controls::{self, Anchors, BorderOffsets, Cell, Reflow, StandardKind,
                             StandardState, TrackSize},
                  *};

use super::conformance::Failure;
fn require(ok: bool, message: &'static str) -> Result<(), Failure>
{
    if ok { Ok(()) } else { Err(Failure::Mismatch(message)) }
}
fn border(l: i16, r: i16, t: i16, b: i16) -> BorderOffsets
{
    BorderOffsets::new(l, r, t, b).unwrap()
}
fn bounds(c: controls::ControlRef<'_>) -> Result<Rect, Failure>
{
    Ok(c.info()?.standard.bounds)
}
fn rect(r: Rect) -> controls::LayoutRect
{
    r.try_into().unwrap()
}
fn usage(s: &System<'_>) -> Result<(u64, u64, u64, u64), Failure>
{
    let d = s.inspection()
             .snapshot(inspection::Scope::Own)
             .map_err(|e| e.cause)?
             .at(0)
             .map_err(|e| e.cause)?;
    match d.accounting
    {
        inspection::MemoryAccounting::ExecutionAndResidentHeap { kernel,
                                                                 application,
                                                                 .. } =>
        {
            Ok((kernel.reserved, kernel.committed, application.reserved, application.committed))
        }
        _ => Err(Failure::Mismatch("window-root resident accounting unavailable"))
    }
}
fn ledger(s: &System<'_>) -> Result<inspection::Pool, Failure>
{
    let snap = s.inspection().snapshot(inspection::Scope::Own).map_err(|e| e.cause)?;
    let ledger = snap.ledger(0).map_err(|e| e.cause)?;
    for i in 0..ledger.len()
    {
        let p = ledger.at(i).map_err(|e| e.cause)?;
        if p.kind == *b"CTRL"
        {
            return Ok(p);
        }
    }
    Err(Failure::Mismatch("window-root CTRL ledger missing"))
}
fn fill_scene(s: &System<'_>, window: WindowId) -> Result<(), Failure>
{
    s.windows().begin_update(window)?;
    let mut refused = false;
    for _ in 0..513
    {
        match s.quickdraw().port(window).paint_rect(Rect::new(0, 0, 1, 1), 0xffffff)
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
    s.windows().end_update(window)?;
    require(refused, "window-root retained pressure did not reach limit")
}
fn clear_scene(s: &System<'_>, window: WindowId) -> Result<(), Failure>
{
    s.windows().begin_update(window)?;
    s.windows().end_update(window)?;
    Ok(())
}
#[cfg(feature = "window-anchors")]
fn checkpoint(s: &System<'_>, marker: &str) -> Result<(), Failure>
{
    s.log(marker)?;
    let start = s.events().uptime_ticks()?;
    loop
    {
        let elapsed = s.events().uptime_ticks()?.saturating_sub(start);
        if elapsed >= 120
        {
            break;
        }
        s.client().wait_event(0, (120 - elapsed) as u32)?;
    }
    Ok(())
}
pub fn run(s: &System<'_>) -> Result<(), Failure>
{
    let before = ledger(s)?;
    let w=s.windows().create_owned(windows::WindowSpec::new(Rect::new(60,60,380,300),"Window anchors").minimum(1,1).procedure(8))?;
    w.select()?;
    let table = s.controls().create_table(w.id(), Rect::new(20, 20, 180, 120), Reflow::Yes)?;
    let child = s.controls().create_standard(w.id(),
                                              StandardKind::Button,
                                              StandardState::new(Rect::new(0, 0, 60, 20),
                                                                 "Table child"),
                                              Reflow::Yes)?;
    let child = table.table()
                     .set_child(Cell { row:    0,
                                       column: 0 },
                                child)
                     .map_err(|e| Failure::Service(e.cause.status()))?;
    child.set_anchors(Anchors::ALL)?;
    table.table().set_column(0, TrackSize::Fill)?;
    table.table().set_row(0, TrackSize::Fill)?;
    let t = table.control();
    t.set_anchors(Anchors::ALL)?;
    require(bounds(t)? == Rect::new(0, 0, 320, 240) && bounds(child)? == bounds(t)?,
            "direct window borders not assigned")?;
    t.set_window_border(border(12, 12, 12, 50))?;
    require(bounds(t)? == Rect::new(12, 12, 308, 190) && bounds(child)? == bounds(t)?,
            "root border or nested arrangement incorrect")?;
    require(child.set_window_border(BorderOffsets::ZERO) == Err(Error::BadArgument),
            "Table child accepted window border property")?;
    let owned = s.controls().create_standard(w.id(),
                                              StandardKind::Button,
                                              StandardState::new(Rect::new(5, 5, 65, 25),
                                                                 "Pinned"),
                                              Reflow::Yes)?;
    let c = owned.control();
    let trailing = Anchors { right: true,
                             bottom: true,
                             ..Anchors::NONE };
    c.set_anchors(trailing)?;
    require(bounds(c)? == Rect::new(260, 220, 320, 240), "trailing window edges did not align")?;
    c.set_window_border(border(5, 12, 7, 9))?;
    require(bounds(c)? == Rect::new(248, 211, 308, 231), "asymmetric border offsets lost")?;
    c.set_anchors(Anchors { left: true,
                            ..Anchors::NONE })?;
    require(bounds(c)? == Rect::new(5, 109, 65, 129), "unanchored window axis did not center")?;
    c.set_anchors(Anchors { left: true,
                            right: true,
                            bottom: true,
                            ..Anchors::NONE })?;
    require(bounds(c)? == Rect::new(5, 211, 308, 231), "opposing window anchors did not stretch")?;
    c.set_window_border(border(300, 300, 200, 200))?;
    require(bounds(c)? == Rect::new(300, 200, 300, 200),
            "oversized borders did not produce zero extent")?;
    c.set_window_border(border(0, 12, 0, 12))?;
    c.set_anchors(trailing)?;
    let old = bounds(c)?;
    let mut malformed=abi::controls::ControlCall { id:c.id().0,operation:abi::controls::REFLOW_OBJECT,
        layout:abi::controls::reflow::Payload{message:abi::controls::reflow::SET_WINDOW_BORDER,
            window_border:abi::controls::reflow::BorderOffsets{left:-1,..Default::default()},..Default::default()},..Default::default()};
    require(s.client().control(&mut malformed) == Err(Error::BadArgument) && bounds(c)? == old,
            "negative window border mutated live state")?;
    let plain = s.controls().create_standard(w.id(),
                                              StandardKind::Button,
                                              StandardState::new(Rect::new(10, 10, 50, 30),
                                                                 "Fixed"),
                                              Reflow::No)?;
    require(plain.control().set_anchors(Anchors::ALL) == Err(Error::NotSupported),
            "classic control accepted window anchors")?;
    drop(plain);
    // Border changes are an atomic transaction, including capture/paint refusal.
    require(c.begin_tracking(Point { h: 260,
                                     v: 220 },
                             100)?
             .tracking,
            "window anchor capture missing")?;
    c.set_anchors(trailing)?;
    require(c.info()?.tracking, "identical window anchor call cancelled capture")?;
    require(c.set_window_border(border(0, 20, 0, 12)) == Err(Error::ServiceBusy)
            && bounds(c)? == old
            && c.info()?.window_border == border(0, 12, 0, 12),
            "capture refusal lost old border or geometry")?;
    c.cancel()?;
    s.windows().begin_update(w.id())?;
    require(c.set_window_border(border(0, 20, 0, 12)) == Err(Error::ServiceBusy)
            && bounds(c)? == old,
            "root crossed pending paint")?;
    s.windows().end_update(w.id())?;
    fill_scene(s, w.id())?;
    let charge = usage(s)?;
    require(c.set_window_border(border(0, 20, 0, 12)) == Err(Error::NoMemory)
            && bounds(c)? == old
            && c.info()?.window_border == border(0, 12, 0, 12),
            "root retained refusal changed settings or geometry")?;
    require(usage(s)? == charge, "root retained refusal leaked resident charges")?;
    clear_scene(s, w.id())?;
    c.set_window_border(border(0, 20, 0, 12))?;
    c.set_window_border(border(0, 12, 0, 12))?;
    // Clearing window anchors preserves geometry, and re-enabling recomputes it.
    c.set_anchors(Anchors::NONE)?;
    c.assign_rect(rect(Rect::new(1, 2, 61, 22)))?;
    require(bounds(c)? == Rect::new(1, 2, 61, 22), "disabled root moved")?;
    c.set_anchors(trailing)?;
    require(bounds(c)? == old, "root anchor re-enable did not arrange")?;
    s.log("WINDOW_ANCHOR_STATIC_DONE borders=direct,offset,clamped axes=center,pin,stretch \
           capture,paint,retained=refused-retried resident-charges=refunded\n")?;
    #[cfg(feature = "window-anchors")]
    {
        let window_old = w.info()?.bounds;
        let table_old = bounds(t)?;
        let child_old = bounds(child)?;
        let button_old = bounds(c)?;
        require(c.begin_tracking(Point { h: 260,
                                         v: 220 },
                                 200)?
                 .tracking,
                "resize capture missing")?;
        checkpoint(s, "WINDOW_ANCHOR_CAPTURE_READY\n")?;
        require(w.info()?.bounds == window_old
                && bounds(t)? == table_old
                && bounds(child)? == child_old
                && bounds(c)? == button_old
                && c.info()?.tracking,
                "captured resize partially committed")?;
        c.cancel()?;
        s.windows().begin_update(w.id())?;
        checkpoint(s, "WINDOW_ANCHOR_PAINT_READY\n")?;
        require(w.info()?.bounds == window_old
                && bounds(t)? == table_old
                && bounds(c)? == button_old,
                "pending-paint resize partially committed")?;
        s.windows().end_update(w.id())?;
        fill_scene(s, w.id())?;
        checkpoint(s, "WINDOW_ANCHOR_PRESSURE_READY\n")?;
        require(w.info()?.bounds == window_old
                && bounds(t)? == table_old
                && bounds(child)? == child_old
                && bounds(c)? == button_old,
                "retained resize partially committed")?;
        clear_scene(s, w.id())?;
        checkpoint(s, "WINDOW_ANCHOR_GROW_READY\n")?;
        require(w.info()?.bounds == Rect::new(60, 60, 480, 360)
                && bounds(t)? == Rect::new(12, 12, 408, 250)
                && bounds(child)? == bounds(t)?
                && bounds(c)? == Rect::new(348, 268, 408, 288),
                "accepted grow did not arrange all roots")?;
        w.set_standard_bounds(Some(Rect::new(60, 60, 560, 420)))?;
        s.windows().begin_update(w.id())?;
        checkpoint(s, "WINDOW_ANCHOR_ZOOM_PAINT_READY\n")?;
        require(!w.info()?.zoomed
                && w.info()?.bounds == Rect::new(60, 60, 480, 360)
                && bounds(t)? == Rect::new(12, 12, 408, 250)
                && bounds(c)? == Rect::new(348, 268, 408, 288),
                "refused zoom changed window, restore state or control geometry")?;
        s.windows().end_update(w.id())?;
        checkpoint(s, "WINDOW_ANCHOR_ZOOM_READY\n")?;
        {
            use core::fmt::Write;
            let mut log = super::LogBuffer::new();
            let _ = writeln!(&mut log,
                             "WINDOW_ANCHOR_ZOOM_OBSERVED window={:?} zoomed={} table={:?} \
                              button={:?} input={:?}",
                             w.info()?.bounds,
                             w.info()?.zoomed,
                             bounds(t)?,
                             bounds(c)?,
                             s.events().input_state()?.mouse);
            s.log(log.text())?;
        }
        require(w.info()?.zoomed
                && w.info()?.bounds == Rect::new(60, 60, 560, 420)
                && bounds(t)? == Rect::new(12, 12, 488, 310)
                && bounds(c)? == Rect::new(428, 328, 488, 348),
                "zoom did not arrange roots")?;
        checkpoint(s, "WINDOW_ANCHOR_ZOOM_CANCEL_READY\n")?;
        require(w.info()?.zoomed
                && w.info()?.bounds == Rect::new(60, 60, 560, 420)
                && bounds(t)? == Rect::new(12, 12, 488, 310)
                && bounds(c)? == Rect::new(428, 328, 488, 348),
                "cancelled zoom changed geometry")?;
        checkpoint(s, "WINDOW_ANCHOR_RESTORE_READY\n")?;
        require(!w.info()?.zoomed
                && w.info()?.bounds == Rect::new(60, 60, 480, 360)
                && bounds(t)? == Rect::new(12, 12, 408, 250)
                && bounds(c)? == Rect::new(348, 268, 408, 288),
                "zoom restore lost root geometry")?;
        let previous = bounds(c)?;
        c.set_anchors(Anchors::NONE)?;
        checkpoint(s, "WINDOW_ANCHOR_DISABLED_READY\n")?;
        require(w.info()?.bounds == Rect::new(60, 60, 520, 380)
                && bounds(t)? == Rect::new(12, 12, 448, 270)
                && bounds(c)? == previous,
                "disabled root followed resize or table failed")?;
        c.set_anchors(trailing)?;
        let root_before = bounds(t)?;
        let button_before = bounds(c)?;
        checkpoint(s, "WINDOW_ANCHOR_DRAG_READY\n")?;
        require(w.info()?.bounds == Rect::new(90, 80, 550, 400)
                && bounds(t)? == root_before
                && bounds(c)? == button_before,
                "window drag changed local control geometry")?;
        checkpoint(s, "WINDOW_ANCHOR_FINAL_PAINT_READY\n")?;
        s.log("WINDOW_ANCHOR_INTERACTIVE_DONE grow,zoom,restore,drag=observed \
               capture,paint,retained=atomic-refusal retry=usable disabled=stationary\n")?;
    }
    drop(owned);
    drop(table);
    require(matches!(child.info(), Err(Error::NotFound)),
            "window table child survived subtree disposal")?;
    drop(w);
    let after = ledger(s)?;
    require((before.live, before.pending) == (after.live, after.pending)
            && after.births - before.births == after.deaths - before.deaths,
            "window root control ownership leaked")?;
    s.log("WINDOW_ANCHOR_NATIVE_DONE ledger=balanced\n")?;
    Ok(())
}
