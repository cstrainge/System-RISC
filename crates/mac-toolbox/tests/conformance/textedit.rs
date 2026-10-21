//! Original System 7.6.1 mono TextEdit observations through the native SDK.
use mac_toolbox::{Error, Point, Rect, System,
                  abi::BAD_ARGUMENT,
                  textedit::{EditorId, MAX_BYTES}};

use super::{Failure, require};
pub struct LayoutCase
{
    pub name:   &'static str,
    pub text:   &'static str,
    pub width:  i16,
    pub starts: &'static [u32],
    pub points: &'static [(i16, i16)]
}
pub struct EditCase
{
    #[allow(dead_code)] // Host oracle identity; not needed by every shared guest profile.
    pub name: &'static str,
    pub start:     u32,
    pub end:       u32,
    pub key:       u32,
    pub text:      &'static str,
    pub selection: (u32, u32)
}
pub struct OffsetCase
{
    pub y:      i16,
    pub values: &'static [(i16, u32)]
}
#[path = "textedit_oracle.rs"]
mod textedit_oracle;
pub use textedit_oracle::{EDITS, LAYOUT, OFFSETS};
fn create(system: &System<'_>, width: i16) -> Result<EditorId, Failure>
{
    Ok(system.textedit().create(Rect::new(6, 4, 6 + width, 90), Rect::new(10, 8, 120, 80))?)
}
fn text(system: &System<'_>, id: EditorId, expected: &str) -> Result<(), Failure>
{
    let mut output = [0; 256];
    require(system.textedit().text(id, &mut output)? == expected, "original TextEdit edit bytes")
}
pub fn original(system: &System<'_>) -> Result<usize, Failure>
{
    let te = system.textedit();
    let mut count = 0;
    for case in LAYOUT
    {
        let id = create(system, case.width)?;
        let info = te.set_text(id, case.text)?;
        require(info.selection_start == case.text.len() as u32
                && info.selection_end == case.text.len() as u32,
                "TESetText selects text end")?;
        if info.line_count as usize + 1 != case.starts.len()
        {
            return Err(Failure::Observation { case:  case.name,
                                              field: "original nLines" });
        }
        require(info.line_height == 16 && info.ascent == 12, "original Chicago12 metrics")?;
        for (index, starts) in case.starts.windows(2).enumerate()
        {
            let line = te.line(id, index as u32)?;
            require(line.start == starts[0] && line.end == starts[1], "original lineStarts")?;
            count += 1;
        }
        for (index, &(h, v)) in case.points.iter().enumerate()
        {
            if te.point(id, index as u32)?
               != (Point { h,
                           v })
            {
                return Err(Failure::Observation { case:  case.name,
                                                  field: "original TEGetPoint" });
            }
            count += 1;
        }
        te.dispose(id)?;
    }
    for case in EDITS
    {
        let id = create(system, 100)?;
        te.set_text(id, "abc\rdef")?;
        te.select(id, case.start, case.end)?;
        let info = te.key(id, char::from_u32(case.key).ok_or(Failure::Mismatch("oracle key"))?)?;
        text(system, id, case.text)?;
        require((info.selection_start, info.selection_end) == case.selection,
                "original TEKey selection")?;
        te.dispose(id)?;
        count += 1;
    }
    let id = create(system, 100)?;
    te.set_text(id, "abcdef")?;
    te.select(id, 2, 4)?;
    let info = te.insert(id, "XY")?;
    text(system, id, "abXYcdef")?;
    require((info.selection_start, info.selection_end) == (4, 6),
            "TEInsert shifts selected range")?;
    te.set_text(id, "abcdef")?;
    te.select(id, 2, 2)?;
    te.delete(id)?;
    text(system, id, "abcdef")?;
    te.select(id, 2, 4)?;
    te.delete(id)?;
    text(system, id, "abef")?;
    te.select(id, 3, 1)?;
    let info = te.info(id)?;
    require((info.selection_start, info.selection_end) == (1, 1),
            "reversed TESetSelect collapses at end")?;
    te.dispose(id)?;
    count += 4;
    let id = create(system, 40)?;
    te.set_text(id, "abc\rdef ghij")?;
    for case in OFFSETS
    {
        for &(h, offset) in case.values
        {
            require(te.offset(id,
                              Point { h,
                                      v: case.y })?
                    == offset,
                    "original TEGetOffset")?;
            count += 1;
        }
    }
    te.dispose(id)?;
    Ok(count)
}
pub fn native(system: &System<'_>) -> Result<usize, Failure>
{
    let te = system.textedit();
    let id = create(system, 100)?;
    te.set_text(id, "Aé水🙂B")?;
    te.select(id, 1, 10)?;
    let mut output = [0; 64];
    require(te.selection_text(id, &mut output)? == "é水🙂", "native UTF8 selection")?;
    let before = te.info(id)?;
    require(te.select(id, 2, 3) == Err(Error::BadArgument), "reject split UTF8 selection")?;
    require(te.info(id)? == before, "bad selection preserves state")?;
    require(te.point(id, 2) == Err(Error::BadArgument), "reject split UTF8 point")?;
    let mut tiny = [0xaa; 1];
    require(te.text(id, &mut tiny) == Err(Error::NoMemory) && tiny == [0xaa],
            "text read atomic on short buffer")?;
    te.key(id, '界')?;
    text(system, id, "A界B")?;
    let before = te.info(id)?;
    require(te.set_rects(id, Rect::ZERO, Rect::new(0, 0, 100, 100)) == Err(Error::BadArgument),
            "reject invalid text rectangles")?;
    require(te.info(id)? == before, "bad rectangles preserve text state")?;
    te.activate(id, true)?;
    te.select(id, 0, 0)?;
    require(te.idle(id, 10)?.caret_visible, "active caret visible")?;
    require(!te.idle(id, 40)?.caret_visible, "30tick caret blink")?;
    te.dispose(id)?;
    let replacement = create(system, 100)?;
    require(replacement != id, "generation safe editor IDs")?;
    require(te.info(id) == Err(Error::NotFound), "stale editor rejected")?;
    te.dispose(replacement)?;
    let _ = MAX_BYTES;
    let id = create(system, 100)?;
    te.set_text(id, "abc")?;
    te.select(id, 1, 2)?;
    system.scrap().clear()?;
    system.scrap().put_text("previous")?;
    te.copy(id)?;
    require(system.scrap().text(&mut output)? == "b",
            "native Copy publishes selection to System scrap")?;
    text(system, id, "abc")?;
    te.cut(id)?;
    text(system, id, "ac")?;
    require(system.scrap().text(&mut output)? == "b", "native Cut publishes then deletes")?;
    te.paste(id)?;
    text(system, id, "abc")?;
    system.scrap().clear()?;
    let before = te.info(id)?;
    require(te.paste(id) == Err(Error::ScrapTypeUnavailable), "missing clipboard flavor")?;
    require(te.info(id)? == before, "missing Paste preserves editor")?;
    te.copy(id)?;
    require(system.scrap().info()?.flavors == 0, "empty Copy leaves clipboard")?;
    te.dispose(id)?;
    Ok(20)
}

/// Pascal string convenience traps keep native UTF-8 offsets. Original TEHandle
/// and counted MacRoman buffers are a separate, intentionally absent adapter.
#[allow(dead_code)] // Only profiles with a raw trap gate can run conversion checks.
pub fn legacy(system: &System<'_>, gate: mac_abi::TrapGate) -> Result<usize, Failure>
{
    use mac_abi::{textedit::*, traps::PSTRING_TEXTEDIT};
    fn call(gate: mac_abi::TrapGate, request: &mut TextEditCall) -> i64
    {
        // Live request and non-overlapping payload buffers belong to this call.
        unsafe {
            gate(PSTRING_TEXTEDIT,
                 core::ptr::from_mut(request).cast(),
                 core::mem::size_of::<TextEditCall>())
        }
    }
    let te = system.textedit();
    let id = create(system, 100)?;
    let pascal = b"\x04Caf\x8e";
    let mut request = TextEditCall { operation: SET_TEXT,
                                     id,
                                     input: pascal.as_ptr(),
                                     input_length: pascal.len(),
                                     ..Default::default() };
    require(call(gate, &mut request) == 0, "Pascal TESetText conversion")?;
    text(system, id, "Café")?;
    let mut output = [0xaa; 256];
    request = TextEditCall { operation: TEXT,
                             id,
                             output: output.as_mut_ptr(),
                             capacity: output.len(),
                             ..Default::default() };
    require(call(gate, &mut request) == 0
            && request.transferred == pascal.len()
            && output[..pascal.len()] == pascal[..],
            "Pascal TextEdit output roundtrip")?;
    require(output[pascal.len()..].iter().all(|&b| b == 0xaa), "Pascal output tail preserved")?;
    te.set_text(id, "水")?;
    output.fill(0xaa);
    require(call(gate, &mut request) == BAD_ARGUMENT as i64 && output.iter().all(|&b| b == 0xaa),
            "unmappable Pascal output rejected atomically")?;
    let malformed = b"\x05abc";
    request = TextEditCall { operation: INSERT,
                             id,
                             input: malformed.as_ptr(),
                             input_length: malformed.len(),
                             ..Default::default() };
    require(call(gate, &mut request) == BAD_ARGUMENT as i64, "malformed Pascal insert rejected")?;
    text(system, id, "水")?;
    te.dispose(id)?;
    Ok(6)
}

/// Native editor navigation is intentionally distinct from original TEKey.
pub fn navigation(system: &System<'_>) -> Result<usize, Failure>
{
    use mac_toolbox::textedit::CursorMovement::*;
    let te = system.textedit();
    let id = create(system, 100)?;
    let mut count = 0;
    macro_rules! movement {
        ($direction:expr,$extend:expr,$range:expr) => {{
            let info = te.move_cursor(id, $direction, $extend)?;
            require((info.selection_start, info.selection_end) == $range, "native cursor/anchor")?;
            count += 1;
        }};
    }
    te.set_text(id, "abc\rdef\rghi")?;
    te.select(id, 6, 6)?;
    movement!(LineStart, false, (4, 4));
    movement!(LineEnd, false, (7, 7));
    movement!(LineStart, true, (4, 7));
    movement!(Right, true, (5, 7));
    movement!(Right, true, (6, 7));
    movement!(Right, true, (7, 7));
    movement!(Right, true, (7, 8));
    movement!(Left, true, (7, 7));
    movement!(Left, true, (6, 7));
    movement!(DocumentStart, false, (0, 0));
    movement!(Right, false, (1, 1));
    movement!(Down, true, (1, 5));
    movement!(Up, true, (1, 1));
    movement!(DocumentEnd, true, (1, 11));
    movement!(DocumentStart, true, (0, 1));
    te.key(id, 'X')?;
    text(system, id, "Xbc\rdef\rghi")?;
    te.set_text(id, "Aé水🙂B")?;
    movement!(Left, false, (10, 10));
    movement!(Left, true, (6, 10));
    movement!(Left, true, (3, 10));
    movement!(Right, true, (6, 10));
    movement!(DocumentStart, true, (0, 10));
    movement!(DocumentEnd, true, (10, 11));
    movement!(Left, false, (10, 10));
    te.set_text(id, "aaaa\raa\raaaa")?;
    te.select(id, 3, 3)?;
    movement!(Down, false, (7, 7));
    movement!(Down, false, (11, 11));
    movement!(Up, false, (7, 7));
    movement!(Up, false, (3, 3));
    movement!(Down, true, (3, 7));
    movement!(Down, true, (3, 11));
    movement!(Up, true, (3, 7));
    movement!(Up, true, (3, 3));
    te.set_rects(id, Rect::new(6, 4, 30, 90), Rect::new(6, 4, 106, 90))?;
    te.set_text(id, "aaaaaa")?;
    te.select(id, 1, 1)?;
    movement!(LineEnd, false, (3, 3));
    movement!(LineStart, false, (0, 0));
    movement!(LineEnd, false, (3, 3));
    movement!(LineStart, true, (0, 3));
    movement!(LineEnd, true, (3, 3));
    movement!(Down, false, (6, 6));
    movement!(LineStart, false, (3, 3));
    te.set_text(id, "ab\r")?;
    movement!(LineStart, false, (3, 3));
    movement!(LineEnd, false, (3, 3));
    movement!(Up, false, (0, 0));
    movement!(Down, false, (3, 3));
    te.set_text(id, "")?;
    for direction in [Left, Right, Up, Down, LineStart, LineEnd, DocumentStart, DocumentEnd]
    {
        movement!(direction, true, (0, 0));
    }
    te.dispose(id)?;
    Ok(count)
}
