//! Native public-SDK clipboard/editor newline scenario. This runs inside the
//! existing disk-booted diagnostic app, with real Scrap and TextEdit traps.
use mac_toolbox::{Error, Rect, System, textedit::MAX_BYTES};

use crate::conformance::Failure;
fn require(value: bool, message: &'static str) -> Result<(), Failure>
{
    if value { Ok(()) } else { Err(Failure::Mismatch(message)) }
}

pub fn run(system: &System<'_>) -> Result<(), Failure>
{
    let te = system.textedit();
    let mut editor = te.create_owned(Rect::new(0, 0, 500, 320), Rect::new(0, 0, 500, 320))?;
    let id = editor.id();
    let scrap = system.scrap();
    let mut bytes = [0; 256];
    for (external, internal, exported) in
        [("café\r\n雪 🦀\r\n", "café\r雪 🦀\r", "café\n雪 🦀\n"),
         ("café\n雪 🦀\n", "café\r雪 🦀\r", "café\n雪 🦀\n"),
         ("café\r雪 🦀\r", "café\r雪 🦀\r", "café\n雪 🦀\n"),
         ("one\r\ntwo\nthree\rfour\r\n", "one\rtwo\rthree\rfour\r", "one\ntwo\nthree\nfour\n")]
    {
        te.set_text(id, "old selection")?;
        te.select(id, 0, u32::MAX)?;
        scrap.clear()?;
        scrap.put_text(external)?;
        let info = te.paste(id)?;
        require(te.text(id, &mut bytes)? == internal, "Paste normalizes line breaks once")?;
        require(info.selection_start == internal.len() as u32
                && info.selection_end == info.selection_start,
                "Paste caret uses normalized UTF-8 offsets")?;
        require(te.display_info(id)?.logical_lines
                == 1 + internal.bytes().filter(|&b| b == b'\r').count() as u32,
                "CRLF and LF are actual single editor line breaks")?;
        let second = internal.find('\r').ok_or(Failure::Mismatch("scenario line break"))? + 1;
        require(te.point(id, second as u32)?.v > te.point(id, 0)?.v,
                "Caret advances vertically after normalized line break")?;
        require(scrap.text(&mut bytes)? == external,
                "Paste preserves original shared Scrap bytes")?;
        te.select(id, 0, u32::MAX)?;
        te.copy(id)?;
        require(scrap.text(&mut bytes)? == exported, "Copy exports Unicode text with LF")?;
        te.cut(id)?;
        require(te.text(id, &mut bytes)?.is_empty(), "Cut deletes normalized text")?;
        require(scrap.text(&mut bytes)? == exported, "Cut exports same LF text")?;
        te.paste(id)?;
        require(te.text(id, &mut bytes)? == internal,
                "Copy/cut/paste does not accumulate line breaks")?;
    }
    te.set_text(id, "keep\rthis")?;
    te.select(id, 1, 3)?;
    let before = te.info(id)?;
    let oversized = [b'\n'; MAX_BYTES + 1];
    let oversized =
        core::str::from_utf8(&oversized).map_err(|_| Failure::Mismatch("native scenario UTF-8"))?;
    scrap.clear()?;
    scrap.put_text(oversized)?;
    let clipboard_before = scrap.info()?;
    require(te.paste(id) == Err(Error::NoMemory), "Oversize Paste gives typed memory refusal")?;
    require(te.info(id)? == before && te.text(id, &mut bytes)? == "keep\rthis",
            "Refused Paste preserves text, revision and selection")?;
    require(scrap.info()? == clipboard_before, "Refused Paste preserves shared Scrap")?;
    scrap.clear()?;
    scrap.put_text("retry\r\nworks")?;
    te.select(id, 0, u32::MAX)?;
    te.paste(id)?;
    require(te.text(id, &mut bytes)? == "retry\rworks", "Editor remains usable after refusal")?;
    te.select(id, 0, 0)?;
    let clipboard_before = scrap.info()?;
    te.copy(id)?;
    te.cut(id)?;
    require(scrap.info()? == clipboard_before, "Empty Copy/Cut preserves clipboard")?;
    // Raw set_text remains the documented CR-based editor contract. Newline
    // adaptation is confined to native clipboard integration.
    te.set_text(id, "raw\nLF")?;
    require(te.text(id, &mut bytes)? == "raw\nLF", "Raw editor API remains byte preserving")?;
    editor.dispose()?;
    system.log("TEXTEDIT_CLIPBOARD_NEWLINES_PASS unicode=preserved crlf=one lf=one cr=one \
                copy=lf cut=atomic refusal=retained retry=usable\n")?;
    Ok(())
}
