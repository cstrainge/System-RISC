//! Native decoration toggles on the real resident Table and loaded CDEF.
use mac_toolbox::{controls::{self, Cell, GridLines, Reflow, Span, StandardKind, StandardState,
                             TrackSize},
                  *};

use super::conformance::Failure;
fn require(ok: bool, text: &'static str) -> Result<(), Failure>
{
    if ok { Ok(()) } else { Err(Failure::Mismatch(text)) }
}
fn rect(r: Rect) -> controls::LayoutRect
{
    r.try_into().unwrap()
}
fn pause(s: &System<'_>, marker: &str) -> Result<(), Failure>
{
    s.log(marker)?;
    let start = s.events().uptime_ticks()?;
    loop
    {
        let elapsed = s.events().uptime_ticks()?.saturating_sub(start);
        if elapsed >= 60
        {
            break;
        }
        s.client().wait_event(0, (60 - elapsed) as u32)?;
    }
    Ok(())
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
    Err(Failure::Mismatch("Table lines CTRL ledger missing"))
}
pub fn run(s: &System<'_>) -> Result<(), Failure>
{
    let before = ledger(s)?;
    let w =
        s.windows().create_owned(windows::WindowSpec::new(Rect::new(20, 60, 780, 500),
                                                           "Table line options"))?;
    w.select()?;
    let classic = s.controls().create_table(w.id(), Rect::new(10, 10, 180, 80), Reflow::No)?;
    require(classic.table().grid_lines()? == GridLines::ALL, "classic Table line default changed")?;
    classic.table().set_grid_lines(GridLines::NONE)?;
    require(classic.table().grid_lines()? == GridLines::NONE, "classic Table border switch lost")?;
    drop(classic);
    s.log("TABLE_LINES_CLASSIC_DONE\n")?;
    let t = s.controls().create_table(w.id(), Rect::new(10, 10, 180, 80), Reflow::Yes)?;
    let button = s.controls().create_standard(w.id(),
                                               StandardKind::Button,
                                               StandardState::new(Rect::new(0, 0, 60, 20),
                                                                  "Capture"),
                                               Reflow::Yes)?;
    let c = t.table()
             .set_child(Cell { row:    0,
                               column: 0 },
                        button)
             .map_err(|e| Failure::Service(e.cause.status()))?;
    t.control().assign_rect(rect(Rect::new(10, 10, 180, 80)))?;
    let old = c.info()?.standard.bounds;
    let root_old = t.control().info()?.standard.bounds;
    let size = t.control().ask_size(Default::default())?;
    require(c.begin_tracking(Point { h: old.left + 3,
                                     v: old.top + 3 },
                             20)?
             .tracking,
            "Table line capture missing")?;
    t.table().set_grid_lines(GridLines::NONE)?;
    require(c.info()?.tracking
            && c.info()?.standard.bounds == old
            && t.control().info()?.standard.bounds == root_old
            && !t.control().info()?.arrangement_pending
            && t.control().ask_size(Default::default())? == size,
            "line change altered capture, geometry or measurement")?;
    c.cancel()?;
    s.log("TABLE_LINES_CAPTURE_DONE\n")?;
    let mut invalid = abi::controls::ControlCall { id: t.control().id().0,
                                                   operation:
                                                       abi::controls::SET_TABLE_LINES_OBJECT,
                                                   ..Default::default() };
    invalid.standard.flags = 8;
    require(s.client().control(&mut invalid) == Err(Error::BadArgument)
            && t.table().grid_lines()? == GridLines::NONE,
            "unknown Table line bits mutated state")?;
    invalid.id = c.id().0;
    invalid.standard.flags = 0;
    require(s.client().control(&mut invalid) == Err(Error::NotSupported),
            "non-Table accepted line switches")?;
    s.log("TABLE_LINES_REFUSALS_DONE\n")?;
    t.table().set_grid_lines(GridLines::ALL)?;
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
    require(full, "Table line pressure did not fill retained scene")?;
    // The exact same setting must not try to insert a new retained command.
    t.table().set_grid_lines(GridLines::ALL)?;
    require(t.table().set_grid_lines(GridLines::NONE) == Err(Error::NoMemory)
            && t.table().grid_lines()? == GridLines::ALL
            && c.info()?.standard.bounds == old,
            "Table line admission refusal changed settings or geometry")?;
    s.windows().begin_update(w.id())?;
    s.windows().end_update(w.id())?;
    t.table().set_grid_lines(GridLines::NONE)?;
    require(t.table().grid_lines()? == GridLines::NONE, "Table line retry did not commit")?;
    s.log("TABLE_LINES_PRESSURE_DONE\n")?;
    drop(t);
    require(matches!(c.info(), Err(Error::NotFound)), "Table line child survived disposal")?;
    let mut tables: [Option<controls::Table<'_>>; 8] = core::array::from_fn(|_| None);
    for (i, slot) in tables.iter_mut().enumerate()
    {
        let x = 15 + (i as i16 % 4) * 185;
        let y = 20 + (i as i16 / 4) * 205;
        let area = Rect::new(x, y, x + 160, y + 160);
        let table = s.controls().create_table(w.id(), area, Reflow::Yes)?;
        for (row, column, span, label) in [(0, 0, Span::new(1, 2).unwrap(), "Merged"),
                                           (1, 0, Span::ONE, "Left"),
                                           (1, 1, Span::ONE, "Right")]
        {
            let child =
                s.controls()
                 .create_standard(w.id(),
                                  StandardKind::StaticText,
                                  StandardState { minimum: 0,
                                                  maximum: 0,
                                                  ..StandardState::new(Rect::new(0, 0, 44,
                                                                                 16),
                                                                       label) },
                                  Reflow::Yes)?;
            table.table()
                 .set_child_spanning(Cell { row,
                                            column },
                                     span,
                                     child)
                 .map_err(|e| Failure::Service(e.cause.status()))?;
        }
        for index in [0, 1]
        {
            table.table().set_column(index, TrackSize::Fill)?;
            table.table().set_row(index, TrackSize::Fill)?;
        }
        table.control().assign_rect(rect(area))?;
        *slot = Some(table);
    }
    pause(s, "TABLE_LINES_BEFORE_READY\n")?;
    for (i, t) in tables.iter().enumerate()
    {
        let table = t.as_ref().unwrap();
        let old = table.control().info()?.standard.bounds;
        let size = table.control().ask_size(Default::default())?;
        let lines = GridLines { rows:    i & 1 != 0,
                                columns: i & 2 != 0,
                                border:  i & 4 != 0 };
        table.table().set_grid_lines(lines)?;
        require(table.table().grid_lines()? == lines
                && table.control().info()?.standard.bounds == old
                && !table.control().info()?.arrangement_pending
                && table.control().ask_size(Default::default())? == size,
                "line matrix changed layout or lost flags")?;
    }
    pause(s, "TABLE_LINES_MATRIX_READY\n")?;
    for t in tables.iter().flatten()
    {
        t.table().set_grid_lines(GridLines::NONE)?;
    }
    pause(s, "TABLE_LINES_HIDDEN_READY\n")?;
    for t in tables.iter().flatten()
    {
        t.table().set_grid_lines(GridLines::ALL)?;
    }
    pause(s, "TABLE_LINES_RESTORED_READY\n")?;
    drop(tables);
    drop(w);
    let after = ledger(s)?;
    require((before.live, before.pending) == (after.live, after.pending)
            && after.births - before.births == after.deaths - before.deaths,
            "Table line ownership leaked")?;
    s.log("TABLE_LINES_NATIVE_DONE modes=8 geometry,measurement,capture=preserved \
           malformed,wrong-family,pressure=refused retry=usable ledger=balanced\n")?;
    Ok(())
}
