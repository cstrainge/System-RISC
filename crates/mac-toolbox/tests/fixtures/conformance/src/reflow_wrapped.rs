//! Native font-derived sizing and text publication through the public SDK.
use mac_toolbox::{controls::{self, Anchors, Cell, Reflow, SizeBounds, Span, StaticTextSpec,
                             TextAlignment, TextLayout},
                  *};

use super::conformance::Failure;
fn require(ok: bool, why: &'static str) -> Result<(), Failure>
{
    if ok { Ok(()) } else { Err(Failure::Mismatch(why)) }
}
fn query(width: i16) -> controls::Constraints
{
    controls::Constraints { width: controls::AxisConstraint::Exact(controls::Extent::new(width).unwrap()),
                            height: controls::AxisConstraint::Unbounded }
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
    Err(Failure::Mismatch("wrapped text CTRL ledger missing"))
}
fn font_width(s: &System<'_>, text: &str) -> Result<i16, Failure>
{
    Ok(s.client().font(abi::quickdraw::FONT_WIDTH, Default::default(), text, 0)?.width)
}
fn pixels(s: &System<'_>) -> Result<(), Failure>
{
    // Compare the actual loaded CDEF output to independently positioned complete
    // lines through the same public drawing transport. A split cluster, wrong
    // baseline, missing ellipsis or clip leakage changes the pixel arrays.
    let font = abi::quickdraw::FontSelection::default();
    let m = s.client().font(abi::quickdraw::FONT_METRICS, font, "", 0)?.metrics;
    let h = (m.ascent + m.descent + m.leading).max(1);
    let mut actual = [0xffffff; 128 * 128];
    let mut expected = [0xffffff; 128 * 128];
    for (text, cluster, rows) in
        [("A A A", "A", 3), ("e\u{301}e\u{301}", "e\u{301}", 2), ("👩‍🚀👩‍🚀", "👩‍🚀", 2)]
    {
        actual.fill(0xffffff);
        expected.fill(0xffffff);
        let width = font_width(s, cluster)?.max(1);
        require(width < 110, "pixel fixture cluster wider than surface")?;
        let state =
            controls::StandardState { minimum: 0,
                                      maximum: 0,
                                      font,
                                      flags: abi::controls::TEXT_WRAP,
                                      ..controls::StandardState::new(Rect::new(4,
                                                                               4,
                                                                               4 + width,
                                                                               4 + rows * h),
                                                                     text) };
        s.controls().draw_standard_pixels(&mut actual,
                                           128,
                                           128,
                                           controls::StandardKind::StaticText,
                                           state)?;
        for row in 0..rows
        {
            let state =
                controls::StandardState { minimum: 0,
                                          maximum: 0,
                                          font,
                                          ..controls::StandardState::new(Rect::new(4,
                                                                                   4 + row * h,
                                                                                   4 + width,
                                                                                   4
                                                                                   + (row + 1)
                                                                                     * h),
                                                                         cluster) };
            s.controls().draw_standard_pixels(&mut expected,
                                               128,
                                               128,
                                               controls::StandardKind::StaticText,
                                               state)?;
        }
        require(actual == expected, "wrapped text pixels differ from intact grapheme lines")?;
        require(actual.iter().any(|p| *p != 0xffffff), "wrapped pixel scenario drew no ink")?;
    }
    actual.fill(0xffffff);
    expected.fill(0xffffff);
    let width = font_width(s, "A")?;
    let state = controls::StandardState { minimum: 0,
                                          maximum: 0,
                                          font,
                                          flags: abi::controls::TEXT_WRAP,
                                          ..controls::StandardState::new(Rect::new(4,
                                                                                   4,
                                                                                   4 + width,
                                                                                   4 + 2 * h),
                                                                         "A \u{301}") };
    s.controls().draw_standard_pixels(&mut actual,
                                       128,
                                       128,
                                       controls::StandardKind::StaticText,
                                       state)?;
    for (row, line) in ["A", " \u{301}"].into_iter().enumerate()
    {
        let y = 4 + row as i16 * h;
        let state = controls::StandardState { minimum: 0,
                                              maximum: 0,
                                              font,
                                              flags: abi::controls::TEXT_WRAP,
                                              ..controls::StandardState::new(Rect::new(4,
                                                                                       y,
                                                                                       4
                                                                                       + width,
                                                                                       y + h),
                                                                             line) };
        s.controls().draw_standard_pixels(&mut expected,
                                           128,
                                           128,
                                           controls::StandardKind::StaticText,
                                           state)?;
    }
    require(actual == expected, "wrap trim split a space-plus-combining grapheme")?;
    actual.fill(0xffffff);
    expected.fill(0xffffff);
    let width = font_width(s, "AA…")?;
    for (buffer, text) in [(&mut actual, "AAAAAAAAAAAA"), (&mut expected, "AA…")]
    {
        let state = controls::StandardState { minimum: 0,
                                              maximum: 0,
                                              font,
                                              ..controls::StandardState::new(Rect::new(4,
                                                                                       4,
                                                                                       4
                                                                                       + width,
                                                                                       4 + h),
                                                                             text) };
        s.controls().draw_standard_pixels(buffer,
                                           128,
                                           128,
                                           controls::StandardKind::StaticText,
                                           state)?;
    }
    require(actual == expected, "ellipsis pixels differ from the fitted prefix")?;
    s.log("WRAPPED_TEXT_PIXELS words,combining,emoji=whole-lines ellipsis=matched\n")?;
    Ok(())
}
pub fn run(s: &System<'_>) -> Result<(), Failure>
{
    let before = ledger(s)?;
    pixels(s)?;
    let w =
        s.windows().create_owned(windows::WindowSpec::new(Rect::new(50, 70, 650, 490),
                                                           "Wrapped Table text"))?;
    w.select()?;
    let table = s.controls().create_table(w.id(), Rect::new(10, 10, 130, 300), Reflow::Yes)?;
    table.table().set_column(0, SizeBounds::FILL)?;
    table.table().set_column(1, SizeBounds::FILL)?;
    let text = "Alpha beta gamma delta epsilon zeta eta theta.";
    let owned =
        s.controls().create_static_text_with(w.id(),
                                              StaticTextSpec { bounds: Rect::new(0, 0, 300,
                                                                                 20),
                                                               text,
                                                               font: Default::default(),
                                                               alignment: TextAlignment::Left,
                                                               layout: TextLayout::Wrap,
                                                               reflow: Reflow::Yes })?;
    let c = table.table()
                 .set_child_spanning(Cell { row:    0,
                                            column: 0 },
                                     Span::new(1, 2).unwrap(),
                                     owned.into_control())
                 .map_err(|e| Failure::Service(e.cause.status()))?;
    c.set_anchors(Anchors { left:   true,
                            right:  true,
                            top:    true,
                            bottom: false })?;
    let nested = s.controls().create_table(w.id(), Rect::new(0, 0, 100, 30), Reflow::Yes)?;
    nested.table().set_column(0, SizeBounds::FILL)?;
    let nested_text =
        s.controls().create_static_text_with(w.id(),
                                              StaticTextSpec { bounds: Rect::new(0, 0, 250,
                                                                                 20),
                                                               text,
                                                               font: Default::default(),
                                                               alignment: TextAlignment::Left,
                                                               layout: TextLayout::Wrap,
                                                               reflow: Reflow::Yes })?;
    let nc = nested.table()
                   .set_child(Cell { row:    0,
                                     column: 0 },
                              nested_text.into_control())
                   .map_err(|e| Failure::Service(e.cause.status()))?;
    nc.set_anchors(Anchors { left:   true,
                             right:  true,
                             top:    true,
                             bottom: false })?;
    let n = table.table()
                 .set_child_spanning(Cell { row:    1,
                                            column: 0 },
                                     Span::new(1, 2).unwrap(),
                                     nested.into_control())
                 .map_err(|e| Failure::Service(e.cause.status()))?;
    n.set_anchors(Anchors { left:   true,
                            right:  true,
                            top:    true,
                            bottom: false })?;
    let native = c.ask_size(Default::default())?;
    let wide = c.ask_size(query(500))?;
    let narrow = c.ask_size(query(120))?;
    require(native.intrinsic == wide.intrinsic && native.intrinsic == narrow.intrinsic,
            "width query changed intrinsic text size")?;
    require(native.intrinsic.preferred().width().pixels() == font_width(s, text)?
            && narrow.constrained.preferred().height() > wide.constrained.preferred().height(),
            "native text height did not follow resolved width")?;
    require(c.ask_size(query(0))?.constrained.preferred().height()
            >= narrow.constrained.preferred().height(),
            "zero width did not make bounded progress")?;
    table.control().assign_rect(Rect::new(10, 10, 130, 300).try_into().unwrap())?;
    let first = c.info()?.standard.bounds;
    require(first.right - first.left == 120
            && first.bottom - first.top == narrow.constrained.preferred().height().pixels(),
            "spanned Table row ignored constrained text height")?;
    let nested_bounds = nc.info()?.standard.bounds;
    require(nested_bounds.right - nested_bounds.left == 120
            && nested_bounds.bottom - nested_bounds.top == first.bottom - first.top,
            "nested Table ignored resolved wrapping width")?;
    nc.static_text().set("Nested short")?;
    s.client().next_event(0)?;
    let nested_short = nc.info()?.standard.bounds;
    require(nested_short.bottom - nested_short.top < nested_bounds.bottom - nested_bounds.top,
            "nested content change did not propagate row requirements")?;
    c.static_text().set("Short")?;
    require(c.info()?.standard.bounds == first, "text update restored stale creation geometry")?;
    s.client().next_event(0)?;
    let short = c.info()?.standard.bounds;
    require(short.bottom - short.top < first.bottom - first.top
            && short.left == first.left
            && short.right == first.right,
            "content change did not reflow row at assigned width")?;
    c.static_text().set(text)?;
    s.client().next_event(0)?;
    require(c.info()?.standard.bounds == first, "long text reflow did not restore row")?;
    table.control().assign_rect(Rect::new(10, 10, 510, 300).try_into().unwrap())?;
    let expanded = c.info()?.standard.bounds;
    require(expanded.bottom - expanded.top == wide.constrained.preferred().height().pixels(),
            "wider Table did not reduce wrapped row height")?;
    c.static_text().set_font(abi::quickdraw::FontSelection { size: 18,
                                                              ..Default::default() })?;
    s.client().next_event(0)?;
    require(c.ask_size(Default::default())?.intrinsic.preferred().height()
            > native.intrinsic.preferred().height(),
            "font change reused stale metrics")?;
    c.static_text().set_font(Default::default())?;
    c.static_text().set("A\r\n\rB\n")?;
    let h = wide.intrinsic.preferred().height().pixels();
    require(c.ask_size(query(500))?.constrained.preferred().height().pixels() == h * 4,
            "CR LF blank or trailing lines were lost")?;
    c.static_text().set("A\r")?;
    require(c.ask_size(query(500))?.constrained.preferred().height().pixels() == h * 2,
            "standalone trailing CR was lost")?;
    c.static_text().set(text)?;
    c.static_text().set_layout(TextLayout::Ellipsis)?;
    require(c.ask_size(query(20))?.constrained.preferred().height().pixels() == h,
            "ellipsis mode unexpectedly wrapped")?;
    c.static_text().set_layout(TextLayout::Wrap)?;
    let old = c.info()?.standard;
    let mut malformed =
        abi::controls::ControlCall { id: c.id().0,
                                     operation: abi::controls::SET_STATIC_TEXT_OBJECT,
                                     flags: abi::controls::TEXT_LAYOUT,
                                     standard: controls::StandardState { flags: 2,
                                                                         ..Default::default() },
                                     ..Default::default() };
    require(s.client().control(&mut malformed) == Err(Error::BadArgument)
            && c.info()?.standard == old,
            "unknown text layout mutated resident state")?;
    malformed.flags = 32;
    require(s.client().control(&mut malformed) == Err(Error::BadArgument),
            "unknown property mask accepted")?;
    require(table.control().static_text().set("wrong family") == Err(Error::NotSupported),
            "Table accepted static text update")?;
    let too_long = [b'A'; abi::INLINE_STRING_BYTES + 1];
    require(c.static_text().set(core::str::from_utf8(&too_long).unwrap())
            == Err(Error::BadArgument),
            "oversize text silently truncated")?;
    let large = [b'W'; abi::INLINE_STRING_BYTES];
    let large_text = core::str::from_utf8(&large).unwrap();
    let large_font = abi::quickdraw::FontSelection { size: 512,
                                                     ..Default::default() };
    require(s.client().font(abi::quickdraw::FONT_WIDTH, large_font, large_text, 0)?.width
            == i16::MAX,
            "classic font width saturation changed")?;
    require(matches!(s.client()
                      .font(abi::quickdraw::FONT_WIDTH_CHECKED, large_font, large_text, 0),
                     Err(Error::BadArgument)),
            "checked font width concealed overflow")?;
    let oversized =
        s.controls().create_static_text_with(w.id(),
                                              StaticTextSpec { bounds:    Rect::new(0, 0, 10, 10),
                                                               text:      "W",
                                                               font:      large_font,
                                                               alignment: TextAlignment::Left,
                                                               layout:    TextLayout::Wrap,
                                                               reflow:    Reflow::Yes })?;
    let accepted = oversized.control().info()?.standard;
    require(oversized.text().set(large_text) == Err(Error::BadArgument)
            && oversized.control().info()?.standard == accepted,
            "oversize natural text changed accepted state")?;
    oversized.text().set_font(Default::default())?;
    oversized.text().set(large_text)?;
    require(oversized.control().ask_size(Default::default()).is_ok(),
            "font overflow correction did not recover")?;
    drop(oversized);
    // Valid widths can still produce an unrepresentable total height. Report a
    // typed measurement error, then accept corrected content on the same object.
    let breaks = [b'\n'; abi::INLINE_STRING_BYTES];
    let tall =
        s.controls()
         .create_static_text_with(w.id(),
                                  StaticTextSpec { bounds:    Rect::new(0, 0, 10, 10),
                                                   text:
                                                       core::str::from_utf8(&breaks).unwrap(),
                                                   font:      large_font,
                                                   alignment: TextAlignment::Left,
                                                   layout:    TextLayout::Wrap,
                                                   reflow:    Reflow::Yes })?;
    require(matches!(tall.control().ask_size(Default::default()), Err(Error::BadArgument)),
            "oversize text height silently narrowed")?;
    tall.text().set("W")?;
    require(tall.control().ask_size(Default::default()).is_ok(),
            "height overflow correction failed")?;
    drop(tall);
    s.log("WRAPPED_TEXT_MEASURE natural,narrow,wide,zero,font,newlines,span,dynamic=observed\n")?;
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
    require(full, "text pressure did not exhaust retained scene")?;
    c.static_text().set(text)?;
    let prior = c.info()?.standard;
    require(c.static_text().set("retry") == Err(Error::NoMemory) && c.info()?.standard == prior,
            "refused text update changed title font or geometry")?;
    s.windows().begin_update(w.id())?;
    s.windows().end_update(w.id())?;
    c.static_text().set("retry")?;
    s.client().next_event(0)?;
    let retry = c.info()?.standard;
    require(&retry.title[..retry.title_length as usize] == b"retry", "text retry did not commit")?;
    c.static_text().set(text)?;
    table.control().assign_rect(Rect::new(10, 10, 190, 300).try_into().unwrap())?;
    s.windows().begin_update(w.id())?;
    table.control().draw()?;
    c.draw()?;
    n.draw()?;
    nc.draw()?;
    s.windows().end_update(w.id())?;
    s.log("WRAPPED_TEXT_PAINT_READY\n")?;
    s.client().wait_event(0, 60)?;
    drop(table);
    require(c.static_text().set("stale") == Err(Error::NotFound),
            "disposed text reference accepted write")?;
    drop(w);
    s.client().next_event(0)?;
    let after = ledger(s)?;
    require((before.live, before.pending) == (after.live, after.pending)
            && after.births - before.births == after.deaths - before.deaths,
            "wrapped text control ownership did not balance")?;
    s.log("WRAPPED_TEXT_NATIVE_DONE refusal=unchanged retry=committed stale=refused \
           CTRL=balanced\n")?;
    Ok(())
}
