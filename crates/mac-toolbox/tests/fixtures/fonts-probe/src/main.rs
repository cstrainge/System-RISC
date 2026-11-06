//! Guest execution probe for font selection.
//!
//! It replaces Finder on a cloned disk so the System runs it at boot, and it
//! asks for families the way an application does: resolve a name to a family
//! through the public client, then ask for metrics at a size and style. No
//! family ID is written here — the IDs an import allocates are an implementation
//! detail, so pinning one would break this probe on a renumbering that no user
//! could see.
//!
//! Every outcome is a serial marker; the harness accepts only
//! `FONTS_PROBE_PASS cases=N`.
#![no_std]
#![no_main]
use core::fmt::{self, Write};

use mac_toolbox::{Error, System, quickdraw::fonts::{FontSelection, Fonts}};

struct Line
{
    bytes:  [u8; 192],
    length: usize
}

impl Line
{
    fn new() -> Self
    {
        Self { bytes:  [0; 192],
               length: 0 }
    }

    fn text(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.length]).unwrap_or("FONTS_PROBE_TEXT_ERROR\n")
    }
}

impl Write for Line
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        let remaining = self.bytes.len() - self.length;
        let count = text.len().min(remaining);
        self.bytes[self.length..self.length + count].copy_from_slice(&text.as_bytes()[..count]);
        self.length += count;
        if count != text.len() { Err(fmt::Error) } else { Ok(()) }
    }
}

fn note(system: &System<'_>, text: &str)
{
    let _ = system.log(text);
}

fn failed(stage: &'static str, error: Error) -> Line
{
    let mut line = Line::new();
    let _ = writeln!(line, "FONTS_PROBE_FAIL {stage} {error:?}");
    line
}

fn check(condition: bool, cause: &'static str) -> Result<(), Line>
{
    if condition
    {
        return Ok(());
    }
    let mut line = Line::new();
    let _ = writeln!(line, "FONTS_PROBE_FAIL {cause}");
    Err(line)
}

/// Resolve one family by name and select it at a size and style.
fn select(system: &System<'_>, fonts: Fonts<'_>, name: &str, size: i16, face: u16)
          -> Result<(i16, i16, i16, i16), Line>
{
    // FONT_ID: the wire operation that maps a name to a family.
    let family = fonts.id(name).map_err(|error| failed("resolve name", error))?;
    let reported = fonts.name(family).map_err(|error| failed("family name", error))?;
    check(reported.name() == name, "the family answered with another name")?;
    // FONT_METRICS: the same selection an application makes to measure text.
    let metrics = fonts.metrics(FontSelection { family, size, face })
                       .map_err(|error| failed("metrics", error))?;
    check(metrics.ascent > 0, "the selected face reported no ascent")?;
    // A real face reports ink above and below the baseline, and a width. The
    // descent is the magnitude below the baseline in this API, not a signed
    // offset, so the sanity check is that it is no larger than the ascent.
    check(metrics.descent >= 0 && metrics.descent <= metrics.ascent,
          "the selected face reported an implausible descent")?;
    check(metrics.max_width > 0, "the selected face reported no width")?;
    let mut line = Line::new();
    let _ = writeln!(line,
                     "FONTS_PROBE_SELECT name=\"{name}\" size={size} style={face} \
                      ascent={} descent={} width={}",
                     metrics.ascent,
                     metrics.descent,
                     metrics.max_width);
    note(system, line.text());
    note(system, "\n");
    Ok((family, metrics.ascent, metrics.descent, metrics.max_width))
}

fn run(system: &System<'_>) -> Result<(), Line>
{
    let fonts = system.quickdraw().fonts();
    let count = fonts.count().map_err(|error| failed("catalog count", error))?;
    let mut line = Line::new();
    let _ = writeln!(line, "FONTS_PROBE_CATALOG families={count}");
    note(system, line.text());
    note(system, "\n");
    check(count >= 9, "the catalog lost the nine families it always carries")?;

    // Families this project imports, one of each shape: an outline, a family of
    // strikes, and a strike family with two styles at one size.
    select(system, fonts, "Klang MT", 12, 0)?;
    select(system, fonts, "Espy Sans", 12, 0)?;
    let (_, plain_ascent, _, plain_width) = select(system, fonts, "VT100", 9, 0)?;
    let (_, bold_ascent, _, bold_width) = select(system, fonts, "VT100", 9, 1)?;
    select(system, fonts, "American Heritage", 12, 0)?;
    // Two styles of one family must resolve to different faces. The catalog
    // picks the closest style match, so a manager that ignored the style field
    // would return identical measurements for both.
    check((plain_ascent, plain_width) != (bold_ascent, bold_width),
          "the bold style returned the plain face")?;
    note(system, "FONTS_PROBE_STYLE_OK\n");
    Ok(())
}

fn main(system: &System<'_>) -> mac_toolbox::ToolboxResult<()>
{
    match run(system)
    {
        Ok(()) =>
        {
            system.log("FONTS_PROBE_PASS cases=4\n")?;
            // The boot check stops reading at the platform's readiness line, so
            // it must follow every outcome marker, never precede them.
            system.log("DESKTOP_READY: fonts probe pass\n")?;
            Ok(())
        }
        Err(line) =>
        {
            system.log(line.text())?;
            system.log("\n")?;
            system.log("DESKTOP_READY: fonts probe failed\n")?;
            Err(Error::BadArgument)
        }
    }
}

mac_toolbox::application!(main);
