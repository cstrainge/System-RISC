//! A small DEC sixel encoder for 16x16 icons.
//!
//! Output is the subset the System RISC Terminal decodes
//! (`apps/terminal/src/sixel.rs`): a DCS sequence whose payload starts with
//! `"1;1;<width>;<height>`, `#n` color selects, `#n;2;r;g;b` palette
//! definitions (RGB channels 0-100), sixel characters (`?`..`~`, where bit
//! i of the character value is the pixel i rows below the band top), `$`
//! carriage return, `-` next band (down 6 rows), terminated by ST.
//! Only set pixels are emitted, so unset pixels stay the terminal
//! background.
//!
//! Layout note, verified against `apps/terminal/src/screen.rs`: placing an
//! image does not move the text cursor, and the Terminal keeps a single
//! transient overlay image — each new sixel replaces the previous one, and
//! scrolling clears it. So per-entry icons are correct on the wire, but
//! only the last one is visible until the Terminal retains inline images.
//! That is a Terminal gap, not an `ls` bug; `ls` emits exactly what the
//! protocol asks for.

use crate::icons::Icon;

/// Palette colors as (index, red, green, blue), channels 0-100.
/// Index 0 is the terminal background and is never defined here.
const PALETTE_MONO: &[(u8, u8, u8, u8)] = &[(1, 90, 90, 90)];
const PALETTE_COLOR: &[(u8, u8, u8, u8)] =
    &[(1, 90, 90, 90), (2, 100, 85, 0), (3, 60, 75, 100), (4, 20, 60, 100)];

/// Highest palette index any icon layer uses.
const MAX_COLOR: u8 = 4;

/// Encode one icon as a complete DCS sixel sequence.
pub fn encode(icon: &Icon, color_mode: bool) -> Vec<u8>
{
    let grid = icon.pixels(color_mode);
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1bPq");
    out.extend_from_slice(format!("\"1;1;{};{}", icon.width, icon.height).as_bytes());
    let palette = if color_mode { PALETTE_COLOR } else { PALETTE_MONO };
    for (index, r, g, b) in palette
    {
        out.extend_from_slice(format!("#{index};2;{r};{g};{b}").as_bytes());
    }
    let bands = icon.height.div_ceil(6);
    let width = icon.width.min(16);
    for band in 0..bands
    {
        let rows = &grid[band * 6..((band + 1) * 6).min(icon.height)];
        let mut used = [false; MAX_COLOR as usize + 1];
        for row in rows
        {
            for color in row.iter().take(width).flatten()
            {
                used[*color as usize] = true;
            }
        }
        let colors: Vec<u8> = (1..=MAX_COLOR).filter(|&c| used[c as usize]).collect();
        // Transpose the band into columns: each column's six pixels
        // become one sixel character, top row as bit 0.
        let mut columns = [[None; 6]; 16];
        for (row, pixels) in rows.iter().enumerate()
        {
            for (x, pixel) in pixels.iter().enumerate().take(width)
            {
                columns[x][row] = *pixel;
            }
        }
        for (i, color) in colors.iter().enumerate()
        {
            out.extend_from_slice(format!("#{color}").as_bytes());
            for column in columns.iter().take(width)
            {
                let mut bits = 0u8;
                for (row, pixel) in column.iter().enumerate()
                {
                    if *pixel == Some(*color)
                    {
                        bits |= 1 << row;
                    }
                }
                out.push(b'?' + bits);
            }
            if i + 1 < colors.len()
            {
                out.push(b'$');
            }
        }
        if band + 1 < bands
        {
            out.push(b'-');
        }
    }
    out.extend_from_slice(b"\x1b\\");
    out
}
