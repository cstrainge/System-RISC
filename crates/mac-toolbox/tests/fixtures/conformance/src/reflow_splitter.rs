//! Bounded native divider scenarios using loaded CDEFs and the public SDK.
use mac_toolbox::{controls::{self, Anchors, Cell, Reflow, SizeBounds, SplitterOrientation,
                             SplitterSpec, StandardKind, StandardState},
                  *};

use super::conformance::Failure;
fn require(ok: bool, why: &'static str) -> Result<(), Failure>
{
    if ok { Ok(()) } else { Err(Failure::Mismatch(why)) }
}
fn pause(s: &System<'_>, ticks: u64) -> Result<(), Failure>
{
    let start = s.events().uptime_ticks()?;
    while s.events().uptime_ticks()?.saturating_sub(start) < ticks
    {
        s.client().wait_event(0, 2)?;
    }
    Ok(())
}
fn ledger(s: &System<'_>) -> Result<inspection::Pool, Failure>
{
    let snapshot = s.inspection().snapshot(inspection::Scope::Own).map_err(|e| e.cause)?;
    let ledger = snapshot.ledger(0).map_err(|e| e.cause)?;
    for index in 0..ledger.len()
    {
        let p = ledger.at(index).map_err(|e| e.cause)?;
        if p.kind == *b"CTRL"
        {
            return Ok(p);
        }
    }
    Err(Failure::Mismatch("splitter CTRL ledger missing"))
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
    require(full, "splitter pressure did not fill retained admission")
}
fn gesture(s: &System<'_>, t: controls::TableRef<'_>, marker: &str) -> Result<(), Failure>
{
    s.events().flush(events::EventMask::ALL, events::EventMask::NONE)?;
    s.log(marker)?;
    let start = s.events().uptime_ticks()?;
    while s.events().uptime_ticks()?.saturating_sub(start) < 300
    {
        let e = s.client().wait_event(0xffff, 5)?;
        if e.kind == abi::events::EventKind::MouseDown
        {
            let point = Point { h: e.where_.h - 50,
                                v: e.where_.v - 70 };
            let hit = t.hit_test(point)?
                       .ok_or(Failure::Mismatch("splitter real input missed boundary"))?;
            require(hit.control.id() == t.control().id() && hit.part == controls::Part::Splitter,
                    "splitter hit routed to wrong control/part")?;
            require(hit.control.track(point, |_| Ok(()))?, "splitter real capture unhandled")?;
            return Ok(());
        }
    }
    Err(Failure::Mismatch("splitter input timeout"))
}
pub fn run(s: &System<'_>) -> Result<(), Failure>
{
    let before = ledger(s)?;
    let mut w = s.windows()
                 .create_owned(windows::WindowSpec::new(Rect::new(50, 70, 650, 490), "Splitter"))?;
    w.select()?;
    let spec = SplitterSpec { bounds:        Rect::new(150, 70, 154, 110),
                              orientation:   SplitterOrientation::Vertical,
                              range:         Some((40, 280)),
                              parent_bounds: Some(Rect::new(20, 20, 320, 220)),
                              enabled:       true,
                              reflow:        Reflow::Yes };
    let divider = s.controls().create_splitter(w.id(), spec)?;
    let d = divider.control();
    s.windows().begin_update(w.id())?;
    d.draw()?;
    s.windows().end_update(w.id())?;
    s.log("SPLITTER_PARENT_BASE_READY\n")?;
    pause(s, 20)?;
    let started = d.begin_tracking(Point { h: 151,
                                           v: 90 },
                                   0)?;
    require(started.tracking && started.handled && !started.action,
            "splitter begin did not acquire capture")?;
    let moved = d.poll_tracking(Point { h: 201,
                                        v: 90 },
                                true,
                                1)?;
    require(moved.standard.value == 200 && moved.standard.bounds == spec.bounds && !moved.action,
            "splitter motion changed bounds or reported release")?;
    require(d.info()?.standard.bounds == spec.bounds,
            "splitter moved resting geometry during drag")?;
    s.log("SPLITTER_PARENT_VERTICAL_READY\n")?;
    pause(s, 25)?;
    d.cancel()?;
    require(!d.info()?.tracking, "splitter cancel retained capture")?;
    s.log("SPLITTER_PARENT_CANCEL_READY\n")?;
    pause(s, 20)?;
    require(divider.splitter().configure(SplitterSpec { range: Some((20, 10)),
                                                        ..spec })
            == Err(Error::BadArgument),
            "splitter inverted range accepted")?;
    let bad = StandardState { bounds: spec.bounds,
                              flags: 4,
                              splitter_parent: Rect::new(20, 20, 10, 10),
                              ..Default::default() };
    require(s.controls().standard_hit(StandardKind::Splitter,
                                      bad,
                                      Point { h: 151,
                                              v: 90 })
            == Err(Error::BadArgument),
            "splitter malformed parent accepted by native definition")?;
    let horizontal = SplitterSpec { bounds: Rect::new(70, 150, 110, 154),
                                    orientation: SplitterOrientation::Horizontal,
                                    range: Some((40, 200)),
                                    ..spec };
    divider.splitter().configure(horizontal)?;
    d.begin_tracking(Point { h: 90,
                             v: 151 },
                     0)?;
    d.poll_tracking(Point { h: 90,
                            v: 181 },
                    true,
                    1)?;
    s.log("SPLITTER_PARENT_HORIZONTAL_READY\n")?;
    pause(s, 25)?;
    let end = d.poll_tracking(Point { h: 90,
                                      v: 181 },
                              false,
                              2)?;
    require(end.action
            && !end.tracking
            && end.standard.value == 180
            && end.standard.bounds == horizontal.bounds,
            "splitter release did not report unchanged-bounds position")?;
    d.begin_tracking(Point { h: 90,
                             v: 151 },
                     3)?;
    require(d.poll_tracking(Point { h: 90,
                                    v: 32767 },
                            true,
                            4)?
             .standard
             .value
            == 200,
            "splitter parent range not clamped")?;
    d.cancel()?;
    s.windows().begin_update(w.id())?;
    require(matches!(d.begin_tracking(Point { h: 90,
                                              v: 151 },
                                      5),
                     Err(Error::ServiceBusy)),
            "splitter begin during paint accepted")?;
    s.windows().end_update(w.id())?;
    d.begin_tracking(Point { h: 90,
                             v: 151 },
                     6)?;
    // A parent geometry change cancels the old outline only after accepted configuration.
    divider.splitter()
           .configure(SplitterSpec { parent_bounds: Some(Rect::new(30, 30, 300, 210)),
                                     ..horizontal })?;
    require(!d.info()?.tracking, "splitter parent-bound change left old capture")?;
    drop(divider);

    let table = s.controls().create_table(w.id(), Rect::new(10, 10, 330, 230), Reflow::Yes)?;
    let t = table.table();
    let c = table.control();
    for n in 0..2
    {
        t.set_column(n, SizeBounds::FILL)?;
        t.set_row(n, SizeBounds::FILL)?;
    }
    require(!t.column_splitter(0)? && !t.row_splitter(0)?, "Table splitters enabled by default")?;
    t.set_column_splitter(0, true)?;
    t.set_row_splitter(0, true)?;
    require(t.column_splitter(0)? && t.row_splitter(0)? && !t.column_splitter(1)?,
            "Table boundary settings not independent")?;
    let mut children = [c; 4];
    for (n, text) in ["One", "Two", "Three", "Four"].into_iter().enumerate()
    {
        let child = s.controls().create_standard(w.id(),
                                                  StandardKind::Button,
                                                  StandardState::new(Rect::new(0, 0, 80, 40),
                                                                     text),
                                                  Reflow::Yes)?;
        child.control().set_anchors(Anchors::ALL)?;
        children[n] = t.set_child(Cell { row:    (n / 2) as u32,
                                         column: (n % 2) as u32 },
                                  child)
                       .map_err(|e| Failure::Service(e.cause.status()))?;
    }
    c.assign_rect(Rect::new(10, 10, 330, 230).try_into().unwrap())?;
    let old = [children[0].info()?.standard.bounds,
               children[1].info()?.standard.bounds,
               children[2].info()?.standard.bounds,
               children[3].info()?.standard.bounds];
    require(old[0] == Rect::new(10, 10, 168, 118) && old[3] == Rect::new(172, 122, 330, 230),
            "Table did not reserve selected splitter gutters")?;
    s.windows().begin_update(w.id())?;
    c.draw()?;
    for child in children
    {
        child.draw()?;
    }
    s.windows().end_update(w.id())?;
    s.log("TABLE_SPLITTER_GRID_READY\n")?;
    pause(s, 20)?;
    let point = Point { h: 170,
                        v: 50 };
    require(c.begin_tracking(point, 0)?.tracking, "Table divider did not capture")?;
    require(!c.poll_tracking(Point { h: 210,
                                     v: 50 },
                             true,
                             1)?
              .action,
            "Table divider resized during drag")?;
    for (child, old) in children.into_iter().zip(old)
    {
        require(child.info()?.standard.bounds == old, "Table pane moved before release")?;
    }
    require(t.set_column_splitter(0, false) == Err(Error::ServiceBusy),
            "Table captured boundary disable accepted")?;
    require(matches!(children[0].begin_tracking(Point { h: 30,
                                                        v: 30 },
                                                2),
                     Err(Error::ServiceBusy)),
            "Table divider permitted second capture")?;
    c.cancel()?;
    for (child, old) in children.into_iter().zip(old)
    {
        require(child.info()?.standard.bounds == old, "Table cancel changed pane geometry")?;
    }
    gesture(s, t, "TABLE_SPLITTER_COLUMN_READY\n")?;
    require(children[0].info()?.standard.bounds.right == 208
            && children[1].info()?.standard.bounds.left == 212,
            "Table column release did not apply pair sizes")?;
    gesture(s, t, "TABLE_SPLITTER_ROW_READY\n")?;
    require(children[0].info()?.standard.bounds.bottom == 158
            && children[2].info()?.standard.bounds.top == 162,
            "Table row release did not apply pair sizes")?;
    s.log("TABLE_SPLITTER_RELEASED_READY\n")?;
    pause(s, 20)?;
    let accepted = [children[0].info()?.standard.bounds,
                    children[1].info()?.standard.bounds,
                    children[2].info()?.standard.bounds,
                    children[3].info()?.standard.bounds];
    let mut pressure_window =
        s.windows()
         .create_owned(windows::WindowSpec::new(Rect::new(450, 300, 580, 390), "Pressure"))?;
    w.select()?;
    pressure(s, pressure_window.id())?;
    require(c.begin_tracking(Point { h: 210,
                                     v: 50 },
                             3)?
             .tracking,
            "Table capture required retained drawing allocation")?;
    c.poll_tracking(Point { h: 190,
                            v: 50 },
                    true,
                    4)?;
    require(matches!(c.poll_tracking(Point { h: 190,
                                             v: 50 },
                                     false,
                                     5),
                     Err(Error::NoMemory)),
            "Table release pressure did not refuse")?;
    require(c.info()?.tracking, "Table release refusal lost cancellable capture")?;
    for (child, old) in children.into_iter().zip(accepted)
    {
        require(child.info()?.standard.bounds == old, "Table release refusal changed panes")?;
    }
    c.cancel()?;
    pressure_window.close()?;
    c.begin_tracking(Point { h: 210,
                             v: 50 },
                     6)?;
    c.poll_tracking(Point { h: 190,
                            v: 50 },
                    false,
                    7)?;
    require(children[0].info()?.standard.bounds.right == 188, "Table retry did not commit")?;
    t.reset_column_split_sizes()?;
    t.reset_row_split_sizes()?;
    c.assign_rect(Rect::new(10, 10, 330, 230).try_into().unwrap())?;
    require(children[0].info()?.standard.bounds == old[0], "Table manual size reset failed")?;
    t.set_row_splitter(0, false)?;
    c.assign_rect(Rect::new(10, 10, 330, 230).try_into().unwrap())?;
    require(t.column_splitter(0)?
            && !t.row_splitter(0)?
            && children[2].info()?.standard.bounds.top == 120,
            "Table boundary disable affected wrong axis")?;
    bounds(s, t, children)?;
    drop(table);
    w.close()?;
    let after = ledger(s)?;
    require(after.live == before.live
            && after.pending == before.pending
            && after.births - before.births == after.deaths - before.deaths,
            "splitter CTRL ledger unbalanced")?;
    s.log("SPLITTER_NATIVE_DONE \
           parent-outline,standalone,selected-boundaries,gestures,cancel,pressure,retry,\
           reset=observed CTRL=balanced\n")?;
    Ok(())
}

pub fn control_suite(s: &System<'_>) -> Result<(), Failure>
{
    let current = s.processes().current()?;
    let entry = s.files().lookup(current.volume, Cnid::ROOT, "Applications/Control Suite")?;
    let process = s.processes().launch(current.volume, entry.id)?;
    s.processes().activate(process.id)?;
    pause(s, 90)?;
    s.log("CONTROL_SUITE_SPLITTER_READY\n")?;
    let start = s.events().uptime_ticks()?;
    while s.events().uptime_ticks()?.saturating_sub(start) < 600
    {
        if !s.processes().is_running(current.volume, entry.id)?
        {
            s.log("CONTROL_SUITE_SPLITTER_CLOSED\n")?;
            return Ok(());
        }
        s.client().wait_event(0, 5)?;
    }
    Err(Failure::Mismatch("Control Suite did not exit after native splitter demonstration"))
}

fn bounds(s: &System<'_>, t: controls::TableRef<'_>, children: [controls::ControlRef<'_>; 4])
          -> Result<(), Failure>
{
    use controls::{Fill, SizeBounds as B, SizeUnit as U};
    let c = t.control();
    let setting = |min, max, fill| B::new(min, max, fill).unwrap();
    let assign = || c.assign_rect(Rect::new(10, 10, 330, 230).try_into().unwrap());
    let right = || children[0].info().map(|i| i.standard.bounds.right);
    t.set_column(0, setting(Some(U::Pixels(100)), Some(U::Percentage(60.0)), Fill::None))?;
    assign()?;
    require(right()? == 110, "no-fill minimum was not applied before distributing remainder")?;
    t.set_column(0, setting(None, Some(U::Pixels(60)), Fill::None))?;
    assign()?;
    require(right()? == 70,
            "no-fill ideal was not clamped below content minimum by explicit maximum")?;
    t.set_column(0, setting(Some(U::Pixels(140)), Some(U::Pixels(180)), Fill::Fill))?;
    assign()?;
    let x = right()?;
    c.begin_tracking(Point { h: x + 1,
                             v: 30 },
                     0)?;
    let limit = c.poll_tracking(Point { h: 400,
                                        v: 30 },
                                true,
                                1)?;
    require(limit.standard.value == 190, "Table splitter ignored first-track maximum")?;
    c.poll_tracking(Point { h: 400,
                            v: 30 },
                    false,
                    2)?;
    require(right()? == 190, "maximum-constrained release assigned wrong size")?;
    c.begin_tracking(Point { h: 191,
                             v: 30 },
                     3)?;
    c.poll_tracking(Point { h: -100,
                            v: 30 },
                    false,
                    4)?;
    require(right()? == 150, "minimum-constrained release assigned wrong size")?;
    t.set_column(0, setting(Some(U::Percentage(25.0)), Some(U::Percentage(50.0)), Fill::Fill))?;
    assign()?;
    require(right()? == 168, "percentage max did not constrain Fill")?;
    c.assign_rect(Rect::new(10, 10, 250, 230).try_into().unwrap())?;
    require(right()? == 128, "percentage bounds did not recompute on viewport width change")?;
    let percent = setting(Some(U::Percentage(33.3333)), Some(U::Percentage(33.3333)), Fill::None);
    t.set_column(0, percent)?;
    assign()?;
    require(right()? == 115 && t.column(0)? == percent,
            "fractional percentage did not round down or round-trip")?;
    t.set_column(0, setting(Some(U::Pixels(0)), Some(U::Pixels(0)), Fill::None))?;
    assign()?;
    require(children[0].info()?.standard.bounds.is_empty(),
            "zero maximum did not collapse track safely")?;
    let large = setting(Some(U::Pixels(250)), None, Fill::Fill);
    t.set_column(0, large)?;
    t.set_column(1, large)?;
    assign()?;
    require(right()? == 168 && children[1].info()?.standard.bounds.left == 172,
            "forced minimum shortage did not shrink deterministically")?;
    let capped = setting(None, Some(U::Pixels(90)), Fill::Fill);
    t.set_column(0, capped)?;
    t.set_column(1, capped)?;
    assign()?;
    require(right()? == 100 && children[1].info()?.standard.bounds.right == 194,
            "capped Fill distributed beyond maxima")?;
    let previous = children[0].info()?.standard.bounds;
    t.set_column(0, setting(Some(U::Pixels(200)), Some(U::Percentage(10.0)), Fill::None))?;
    require(assign() == Err(Error::BadArgument)
            && children[0].info()?.standard.bounds == previous,
            "resolved mixed-unit contradiction changed accepted geometry")?;
    t.set_column(0, B::FILL)?;
    t.set_column(1, B::FILL)?;
    assign()?;
    let old = t.column(0)?;
    for (kind, value, fill) in [(2, f32::NAN.to_bits(), 0),
                                (2, f32::INFINITY.to_bits(), 0),
                                (2, (-1.0f32).to_bits(), 0),
                                (1, 0xffff, 0),
                                (99, 0, 0),
                                (0, 0, 2)]
    {
        let mut call=abi::controls::ControlCall {id:c.id().0,operation:abi::controls::REFLOW_OBJECT,
            layout:abi::controls::reflow::Payload {message:abi::controls::reflow::SET_GRID_TRACK,
                track:abi::controls::reflow::TrackSetting {axis:0,index:0,bounds:abi::controls::reflow::SizeBounds {
                    min:abi::controls::reflow::SizeUnit{kind,value},fill,..Default::default()}},..Default::default()},..Default::default()};
        require(s.client().control(&mut call) == Err(Error::BadArgument) && t.column(0)? == old,
                "malformed native bounds changed track settings")?;
    }
    t.set_column(0, setting(Some(U::Percentage(f32::MAX)), None, Fill::None))?;
    require(assign() == Err(Error::BadArgument), "unrepresentable percentage did not refuse")?;
    t.set_column(0, B::FILL)?;
    assign()?;
    t.set_row(0, setting(None, Some(U::Percentage(25.0)), Fill::Fill))?;
    assign()?;
    require(children[0].info()?.standard.bounds.bottom == 65,
            "row percentage maximum was not applied")?;
    c.assign_rect(Rect::new(10, 10, 330, 310).try_into().unwrap())?;
    require(children[0].info()?.standard.bounds.bottom == 85,
            "row percentage did not follow new height")?;
    t.set_row(0, B::FILL)?;
    assign()?;
    s.log("TABLE_BOUNDS_NATIVE_DONE \
           pixels,percentages,fractional,none,fill,maxima,zero,shortage,split-limits=observed \
           malformed,mixed,overflow=refused retry=committed\n")?;
    Ok(())
}
