
use std::fmt::Write;

use image::{ ImageFormat, RgbaImage, imageops::{ FilterType, resize },
             load_from_memory_with_format };

use super::terminal::TerminalCapabilities;

const LOGO: &[u8] = include_bytes!("../../Shelly.png");
const WELCOME: &str = include_str!("../../welcome_true.txt");
const COLUMNS: u32 = 35;
const ROWS: u32 = 18;
const GAP: u32 = 3;

/// Fit the embedded logo to the same cell bounding box for both image protocols.
/// Preserve alpha while filtering; sixel applies its binary mask afterwards.
pub(super) fn image(capabilities: &TerminalCapabilities) -> Option<RgbaImage>
{
    let (cell_width, cell_height) = capabilities.cell_size.unwrap_or((8, 16));
    let (screen_columns, screen_rows) = capabilities.screen_size.unwrap_or((80, 24));
    let columns = COLUMNS.min(u32::from(screen_columns) / 2).max(1);
    let rows = ROWS.min(u32::from(screen_rows).saturating_sub(2)).max(1);
    let available_width = columns * u32::from(cell_width);
    let available_height = rows * u32::from(cell_height);
    let mut source = load_from_memory_with_format(LOGO, ImageFormat::Png).ok()?.to_rgba8();
    let scale = (f64::from(available_width) / f64::from(source.width()))
        .min(f64::from(available_height) / f64::from(source.height()));
    let width = ((f64::from(source.width()) * scale).round() as u32)
        .clamp(1, available_width);
    let height = ((f64::from(source.height()) * scale).round() as u32)
        .clamp(1, available_height);

    // imageops::resize expects premultiplied alpha. Undo it after filtering so
    // colors at the edge do not acquire a black matte on a light terminal.
    for pixel in source.pixels_mut()
    {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel.0[..3]
        { *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8; }
    }
    let mut image = resize(&source, width, height, FilterType::Lanczos3);
    for pixel in image.pixels_mut()
    {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel.0[..3]
        {
            *channel = if alpha == 0 { 0 }
                else { ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8 };
        }
    }
    Some(image)
}

/// Reserve rows before drawing, center the logo and welcome text vertically,
/// then restore the cursor and place the text three columns to the logo's right.
pub(super) fn compose(capabilities: &TerminalCapabilities, width: u32, height: u32,
                      graphics: &str) -> String
{
    let (cell_width, cell_height) = capabilities.cell_size.unwrap_or((8, 16));
    let text = WELCOME.trim().lines().map(|line| line.trim_end().replace("\\x1b", "\x1b"))
        .collect::<Vec<_>>();
    let image_rows = height.div_ceil(u32::from(cell_height)) as usize;
    let total_rows = image_rows.max(text.len());
    let mut output = "\r\n".repeat(total_rows);
    let _ = write!(output, "\x1b[{total_rows}A\r\x1b7");
    let image_offset = (total_rows - image_rows) / 2;
    if image_offset != 0 { let _ = write!(output, "\x1b[{image_offset}B"); }
    output.push_str(graphics);
    output.push_str("\x1b8");
    let text_offset = (total_rows - text.len()) / 2;
    let text_column = width.div_ceil(u32::from(cell_width)) + GAP;
    for row in 0..total_rows
    {
        if    let Some(index) = row.checked_sub(text_offset)
           && let Some(line) = text.get(index).filter(|line| !line.is_empty())
        { let _ = write!(output, "\x1b[{text_column}C{line}"); }
        output.push_str("\r\n");
    }
    output
}
