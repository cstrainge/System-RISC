
#[cfg(not(target_os = "systemrisc"))]
use std::{ collections::BTreeMap, fmt::Write };

#[cfg(not(target_os = "systemrisc"))]
use color_quant::NeuQuant;

#[cfg(not(target_os = "systemrisc"))]
use super::banner::image;
use super::{ banner::compose, terminal::TerminalCapabilities };

/// Place the prepared logo beside the welcome text using the frontend's cell size.
/// Decoding and resampling the source image costs too much at guest startup.
#[cfg(target_os = "systemrisc")]
pub fn banner(capabilities: &TerminalCapabilities) -> Option<String>
{
    const PREPARED: &str = include_str!("../../banner_sixel.txt");
    let start = PREPARED.find("\x1bP")?;
    let end = start + PREPARED[start..].find("\x1b\\")? + 2;
    let graphics = &PREPARED[start..end];
    // Raster attributes follow the introducer: "aspect;aspect;width;height.
    let raster = &graphics[graphics.find('"')? + 1..];
    let mut values = raster.split(|c: char| !c.is_ascii_digit()).skip(2);
    let width = values.next()?.parse::<u32>().ok()?;
    let height = values.next()?.parse::<u32>().ok()?;
    Some(compose(capabilities, width, height, graphics))
}

/// Resize actual source pixels to a terminal-cell bounding box before encoding.
/// Cell dimensions are cached by startup detection; 8x16 is the fallback.
#[cfg(not(target_os = "systemrisc"))]
pub fn banner(capabilities: &TerminalCapabilities) -> Option<String>
{
    let mut image = image(capabilities)?;
    let (width, height) = image.dimensions();
    let mut visible = Vec::new();
    for pixel in image.pixels_mut()
    {
        // Sixel has binary transparency. Discard faint filtered edge pixels;
        // do not encode transparent source pixels as opaque black.
        if pixel[3] < 128 { continue; }
        pixel[3] = 255;
        visible.extend_from_slice(&pixel.0);
    }
    if visible.is_empty() { return None; }
    let palette = NeuQuant::new(10, 256, &visible);
    let indices = image.pixels().map(|pixel|
        {
            if pixel[3] < 128 { 256 }
            else { palette.index_of(&pixel.0) as u16 }
        }).collect::<Vec<_>>();

    let mut sixel = format!("\x1bP0;1;0q\"1;1;{width};{height}");
    for (index, rgb) in palette.color_map_rgb().as_chunks::<3>().0.iter().enumerate()
    {
        let percent = |value| (u32::from(value) * 100 + 127) / 255;
        let _ = write!(sixel, "#{index};2;{};{};{}",
                       percent(rgb[0]), percent(rgb[1]), percent(rgb[2]));
    }
    for top in (0..height).step_by(6)
    {
        let mut planes = BTreeMap::<u16, Vec<u8>>::new();
        for y in top..(top + 6).min(height)
        {
            for x in 0..width
            {
                let index = indices[(y * width + x) as usize];
                if index == 256 { continue; }
                let plane = planes.entry(index).or_insert_with(|| vec![0; width as usize]);
                plane[x as usize] |= 1 << (y - top);
            }
        }
        for (position, (index, mut values)) in planes.into_iter().enumerate()
        {
            if position != 0 { sixel.push('$'); }
            while values.last() == Some(&0) { values.pop(); }
            let _ = write!(sixel, "#{index}");
            append_runs(&mut sixel, &values);
        }
        if top + 6 < height { sixel.push('-'); }
    }
    sixel.push_str("\x1b\\");

    Some(compose(capabilities, width, height, &sixel))
}

#[cfg(not(target_os = "systemrisc"))]
fn append_runs(output: &mut String, values: &[u8])
{
    let mut start = 0;
    while start < values.len()
    {
        let mut end = start + 1;
        while end < values.len() && values[end] == values[start] { end += 1; }
        let count = end - start;
        let character = char::from(63 + values[start]);
        let repeat = format!("!{count}{character}");
        if repeat.len() < count { output.push_str(&repeat); }
        else { output.extend(std::iter::repeat_n(character, count)); }
        start = end;
    }
}
