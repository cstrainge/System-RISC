//! Bounded native scrolling scenarios through the actual public CTRL transport.
use mac_toolbox::{controls::{self, Anchors, Cell, Reflow, ScrollPosition, Scrolling, SizeBounds,
                             StandardKind, StandardState},
                  *};

use super::conformance::Failure;
fn require(ok: bool, why: &'static str) -> Result<(), Failure>
{
    if ok { Ok(()) } else { Err(Failure::Mismatch(why)) }
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
    Err(Failure::Mismatch("scroll CTRL ledger missing"))
}
fn button<'a>(s: &System<'a>, window: WindowId, bounds: Rect, text: &str)
              -> Result<controls::Control<'a>, Failure>
{
    Ok(s.controls().create_standard(window,
                                     StandardKind::Button,
                                     StandardState::new(bounds, text),
                                     Reflow::Yes)?)
}
fn add<'a>(t: controls::TableRef<'a>, row: u32, column: u32, c: controls::Control<'a>)
           -> Result<controls::ControlRef<'a>, Failure>
{
    Ok(t.set_child(Cell { row,
                          column },
                   c)
        .map_err(|e| Failure::Service(e.cause.status()))?)
}
const BOTH: Scrolling = Scrolling::Fixed { horizontal: true,
                                           vertical:   true };
fn position(x: i64, y: i64) -> ScrollPosition
{
    ScrollPosition { x,
                     y }
}
fn assign(c: controls::ControlRef<'_>, r: Rect) -> Result<(), Failure>
{
    Ok(c.assign_rect(r.try_into().unwrap())?)
}
fn pressure(s: &System<'_>, window: WindowId) -> Result<(), Failure>
{
    s.windows().begin_update(window)?;
    let mut full = false;
    for _ in 0..513
    {
        match s.quickdraw().port(window).paint_rect(Rect::new(0, 0, 1, 1), 0xffffff)
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
    s.windows().end_update(window)?;
    require(full, "scroll pressure did not reach retained admission limit")
}
fn gesture(s: &System<'_>, t: controls::TableRef<'_>, marker: &str) -> Result<(), Failure>
{
    s.events().flush(events::EventMask::ALL, events::EventMask::NONE)?;
    s.log(marker)?;
    let start = s.events().uptime_ticks()?;
    while s.events().uptime_ticks()?.saturating_sub(start) < 240
    {
        let event = s.client().wait_event(0xffff, 5)?;
        if event.kind == abi::events::EventKind::MouseDown
        {
            // The fixture window has a fixed content origin (50,70).
            let local = Point { h: event.where_.h - 50,
                                v: event.where_.v - 70 };
            let hit = t.hit_test(local)?
                       .ok_or(Failure::Mismatch("real scroll gesture missed Table bar"))?;
            require(hit.control.id() == t.control().id(), "bar hit returned a content child")?;
            require(t.control().track(local, |_| Ok(()))?,
                    "real Table scrollbar capture was unhandled")?;
            return Ok(());
        }
    }
    Err(Failure::Mismatch("real Table scrollbar input timed out"))
}
pub fn run(s: &System<'_>) -> Result<(), Failure>
{
    let before = ledger(s)?;
    let mut pixels = [0xff00ff; 128 * 128];
    s.controls().draw_standard_pixels(&mut pixels,
                                       128,
                                       128,
                                       StandardKind::Button,
                                       StandardState::new(Rect::new(-10000, -10000, 10000,
                                                                    10000),
                                                          "Clipped"))?;
    require(pixels.iter().all(|p| *p != 0xff00ff),
            "large clipped button left its central surface unpainted")?;
    s.log("TABLE_SCROLL_LARGE_PAINT visible-region=painted\n")?;
    let w =
        s.windows()
         .create_owned(windows::WindowSpec::new(Rect::new(50, 70, 650, 490), "Scrolling Table"))?;
    w.select()?;
    let table = s.controls().create_table(w.id(), Rect::new(10, 10, 330, 190), Reflow::Yes)?;
    let t = table.table();
    let c = table.control();
    require(t.scroll_info()?.scrolling == Scrolling::Disabled, "scrolling enabled by default")?;
    require(t.scroll_to(position(0, 0)) == Err(Error::NotSupported),
            "disabled Table accepted scroll")?;
    t.set_scrolling(BOTH)?;
    let a = add(t, 0, 0, button(s, w.id(), Rect::new(0, 0, 20000, 20000), "Wide first")?)?;
    let b = add(t, 1, 1, button(s, w.id(), Rect::new(0, 0, 20000, 20000), "Wide last")?)?;
    a.set_anchors(Anchors::ALL)?;
    b.set_anchors(Anchors::ALL)?;
    t.set_column(0, SizeBounds::FILL)?;
    t.set_column(1, SizeBounds::FILL)?;
    assign(c, Rect::new(10, 10, 330, 190))?;
    let info = t.scroll_info()?;
    require(info.content_width == 40000
            && info.content_height == 40000
            && info.horizontal_bar
            && info.vertical_bar
            && info.viewport == Rect::new(10, 10, 314, 174),
            "wide fixed content or coupled viewport wrong")?;
    let requirements = c.ask_size(Default::default())?;
    require(requirements.intrinsic.minimum().width().pixels() == 32
            && requirements.intrinsic.preferred().width().pixels() == 320,
            "content extent leaked into viewport requirement")?;
    let logical = t.content_rect(b)?;
    require(logical.left == 20000 && logical.top == 20000 && logical.right == 40000,
            "wide child logical rectangle narrowed")?;
    t.scroll_to(position(i64::MAX, i64::MAX))?;
    let end = t.scroll_info()?;
    require(end.position == position(39696, 39836) && t.content_rect(b)? == logical,
            "scroll limit or logical stability wrong")?;
    require(b.info()?.standard.bounds == Rect::new(-19686, -19826, 314, 174),
            "wide scroll projection wrong")?;
    require(!c.info()?.arrangement_pending && !c.info()?.measurement_pending,
            "clean scroll invalidated layout caches")?;
    let hit = t.hit_test(Point { h: 30,
                                 v: 30 })?
               .ok_or(Failure::Mismatch("scrolled child not hit"))?;
    require(hit.control.id() == b.id(), "hidden first child stole scrolled hit")?;
    require(t.scroll_to(position(-1, 0)) == Err(Error::BadArgument) && t.scroll_info()? == end,
            "negative scroll changed position")?;
    t.reveal(a)?;
    require(t.scroll_info()?.position == position(0, 0),
            "oversize reveal did not align leading edge")?;
    let busy = a.begin_tracking(Point { h: 30,
                                        v: 30 },
                                0)?;
    require(busy.tracking && t.scroll_to(position(16, 16)) == Err(Error::ServiceBusy),
            "child capture did not protect geometry")?;
    a.cancel()?;
    t.scroll_to(position(16, 16))?;
    s.windows().begin_update(w.id())?;
    let old = t.scroll_info()?;
    require(t.scroll_to(position(32, 32)) == Err(Error::ServiceBusy) && t.scroll_info()? == old,
            "pending paint allowed scroll publication")?;
    s.windows().end_update(w.id())?;
    t.scroll_to(position(32, 32))?;
    // Disabling fixed layout cannot represent this aggregate's requirements.
    let old = t.scroll_info()?;
    require(t.set_scrolling(Scrolling::Disabled) == Err(Error::BadArgument)
            && t.scroll_info()? == old,
            "refused fitting transition changed scrolling mode")?;
    s.log("TABLE_SCROLL_WIDE content=40000x40000 logical=stable hidden-hit=excluded \
           refusal=unchanged\n")?;
    let spanning = button(s, w.id(), Rect::new(0, 0, 100, 20), "Wide span")?;
    t.set_child_spanning(Cell { row:    2,
                                column: 0 },
                         controls::Span::new(1, 2).unwrap(),
                         spanning)
     .map_err(|e| Failure::Service(e.cause.status()))?;
    t.scroll_to(position(20000, i64::MAX))?;
    s.log("TABLE_SCROLL_WIDE_SPAN_READY\n")?;
    s.client().wait_event(0, 60)?;
    drop(table);

    let outer = s.controls().create_table(w.id(), Rect::new(10, 10, 210, 110), Reflow::Yes)?;
    outer.table().set_scrolling(BOTH)?;
    let front =
        add(outer.table(), 0, 0, button(s, w.id(), Rect::new(0, 0, 500, 200), "Visible row")?)?;
    front.set_anchors(Anchors::ALL)?;
    let inner = s.controls().create_table(w.id(), Rect::new(0, 0, 260, 160), Reflow::Yes)?;
    inner.table().set_scrolling(BOTH)?;
    let inside =
        add(inner.table(), 0, 0, button(s, w.id(), Rect::new(0, 0, 600, 300), "Nested content")?)?;
    inside.set_anchors(Anchors::ALL)?;
    let inner_ref = inner.table();
    outer.table()
         .set_child_spanning(Cell { row:    1,
                                    column: 0 },
                             controls::Span::new(1, 2).unwrap(),
                             inner.into_control())
         .map_err(|e| Failure::Service(e.cause.status()))?;
    assign(outer.control(), Rect::new(10, 10, 210, 110))?;
    let logical = inner_ref.content_rect(inside)?;
    inner_ref.scroll_to(position(100, 50))?;
    let hit = outer.table()
                   .hit_test(Point { h: 30,
                                     v: 30 })?
                   .ok_or(Failure::Mismatch("nested hidden scroll lost front child"))?;
    require(hit.control.id() == front.id(), "scrolling a hidden nested Table exposed its child")?;
    outer.table().scroll_to(position(120, 200))?;
    require(inner_ref.content_rect(inside)? == logical
            && inner_ref.scroll_info()?.position == position(100, 50),
            "outer translation rearranged nested scroll content")?;
    let hit = outer.table()
                   .hit_test(Point { h: 30,
                                     v: 30 })?
                   .ok_or(Failure::Mismatch("nested visible content missing"))?;
    require(hit.control.id() == inside.id(), "nested translated hit did not follow clipping")?;
    // Move an accepted nested viewport, then scroll it before either parent
    // consumes its pending layout. Its accepted position must not jump.
    let destination = s.controls().create_table(w.id(), Rect::new(0, 0, 300, 200), Reflow::Yes)?;
    destination.table().set_scrolling(BOTH)?;
    let before_bounds = inner_ref.control().info()?.standard.bounds;
    outer.table().reparent(inner_ref.control(),
                            destination.table(),
                            Cell { row:    0,
                                   column: 0 })?;
    inner_ref.scroll_to(position(110, 60))?;
    require(inner_ref.control().info()?.standard.bounds == before_bounds,
            "nested reparent scroll changed accepted screen origin before layout")?;
    drop(destination);
    // An offscreen leaf owner remains usable after removal and explicit placement.
    let hidden =
        add(outer.table(), 1, 0, button(s, w.id(), Rect::new(0, 0, 100, 20), "Hidden removal")?)?;
    outer.table().scroll_to(position(0, 0))?;
    let removed = outer.table().remove_child(hidden)?;
    assign(removed.control(), Rect::new(20, 20, 120, 40))?;
    require(removed.control()
                   .begin_tracking(Point { h: 30,
                                           v: 30 },
                                   0)?
                   .handled,
            "removed offscreen child stayed hidden after explicit assignment")?;
    removed.control().cancel()?;
    drop(removed);
    drop(outer);
    s.log("TABLE_SCROLL_NESTED span,hidden-scroll,ancestor-translation=observed\n")?;

    let table = s.controls().create_table(w.id(), Rect::new(10, 10, 330, 190), Reflow::Yes)?;
    let t = table.table();
    let c = table.control();
    t.set_scrolling(BOTH)?;
    let a = add(t, 0, 0, button(s, w.id(), Rect::new(0, 0, 310, 190), "Coupled overflow")?)?;
    a.set_anchors(Anchors::ALL)?;
    assign(c, Rect::new(10, 10, 330, 190))?;
    let both = t.scroll_info()?;
    require(both.horizontal_bar && both.vertical_bar,
            "vertical overflow failed to induce horizontal bar")?;
    assign(c, Rect::new(10, 10, 410, 270))?;
    require(!t.scroll_info()?.horizontal_bar && !t.scroll_info()?.vertical_bar,
            "bars persisted without overflow")?;
    require(t.content_rect(a)?.right == 310, "viewport growth stretched fixed content")?;
    assign(c, Rect::new(10, 10, 330, 190))?;
    // Small viewports degrade safely; they must not reinterpret a short vertical
    // bar as a horizontal active bar or allow offscreen child hits.
    assign(c, Rect::new(10, 10, 18, 18))?;
    require(t.scroll_info()?.viewport.is_empty()
            && t.hit_test(Point { h: 12,
                                  v: 12 })?
                .is_none(),
            "tiny viewport has active invalid geometry")?;
    assign(c, Rect::new(10, 10, 330, 190))?;
    // Replace content with a comfortably scrollable row for real pointer input.
    a.set_preferred(controls::Size::try_from(abi::Dimensions { width:  800,
                                                               height: 600 }).unwrap())?;
    let point = Point { h: 322,
                        v: 166 };
    let start = c.begin_tracking(point, 0)?;
    require(start.handled
            && start.tracking
            && start.window == w.id()
            && !start.arrangement_pending
            && !start.measurement_pending
            && t.scroll_info()?.position.y == 16,
            "bar arrow did not capture and step")?;
    let logical = t.content_rect(a)?;
    let repeat = c.poll_tracking(point, true, 20)?;
    require(repeat.action && t.scroll_info()?.position.y == 32, "bar arrow repeat did not step")?;
    c.cancel()?;
    require(!c.info()?.tracking, "bar cancellation left capture live")?;
    t.scroll_to(position(0, 0))?;
    s.windows().begin_update(w.id())?;
    c.draw()?;
    a.draw()?;
    s.windows().end_update(w.id())?;
    gesture(s, t, "TABLE_SCROLL_ARROW_READY\n")?;
    require(t.scroll_info()?.position.y > 0, "real arrow did not move content")?;
    t.scroll_to(position(0, 0))?;
    gesture(s, t, "TABLE_SCROLL_THUMB_READY\n")?;
    require(t.scroll_info()?.position.y > 100 && t.content_rect(a)? == logical,
            "real thumb did not translate fixed content")?;
    // A dirty child remains deferred during capture, and wakes on cancellation.
    let begin = c.begin_tracking(Point { h: 322,
                                         v: 166 },
                                 50)?;
    require(begin.tracking, "deferred-layout arrow capture failed")?;
    a.set_preferred(controls::Size::try_from(abi::Dimensions { width:  900,
                                                               height: 700 }).unwrap())?;
    s.client().next_event(0)?;
    require(c.info()?.arrangement_pending, "captured content change was not deferred")?;
    c.cancel()?;
    s.client().next_event(0)?;
    require(t.scroll_info()?.content_width == 900 && !c.info()?.arrangement_pending,
            "cancel did not wake content reflow")?;
    pressure(s, w.id())?;
    let old = t.scroll_info()?;
    let bounds = a.info()?.standard.bounds;
    require(t.scroll_to(position(0, 0)) == Err(Error::NoMemory)
            && t.scroll_info()? == old
            && a.info()?.standard.bounds == bounds,
            "retained refusal partially published scroll")?;
    s.windows().begin_update(w.id())?;
    s.windows().end_update(w.id())?;
    t.scroll_to(position(0, 0))?;
    require(t.scroll_info()?.position == position(0, 0), "scroll retry did not commit")?;
    t.set_scrolling(Scrolling::Disabled)?;
    require(t.scroll_info()?.scrolling == Scrolling::Disabled
            && a.info()?.standard.bounds.right <= 330,
            "fitting transition did not shrink content")?;
    t.set_scrolling(BOTH)?;
    s.log("TABLE_SCROLL_PAINT_READY\n")?;
    s.client().wait_event(0, 60)?;
    drop(table);
    require(t.scroll_info() == Err(Error::NotFound) && a.info().is_err(),
            "disposed scrolling subtree retained IDs")?;
    drop(w);
    s.client().next_event(0)?;
    let after = ledger(s)?;
    require((before.live, before.pending) == (after.live, after.pending)
            && after.births - before.births == after.deaths - before.deaths,
            "scroll CTRL ownership did not balance")?;
    s.log("TABLE_SCROLL_NATIVE_DONE \
           arrow,repeat,thumb,cancel,pressure,retry,overflow,mode=observed CTRL=balanced\n")?;
    Ok(())
}
