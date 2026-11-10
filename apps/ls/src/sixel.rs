//! Encode shared System icon pixels; no application-owned colour palette.
use sr_files_list::icons::IconRaster;

pub fn encode(icon: &IconRaster, color_mode: bool) -> Vec<u8>
{
    let mut palette = Vec::<u32>::new();
    let mut indices = [None; 256];
    for (i, pixel) in icon.pixels.iter().enumerate()
    {
        if let Some(rgb) = pixel
        {
            let value = if color_mode
            {
                *rgb
            }
            else
            {
                // Monochrome output keeps the dark strokes of the resolved
                // System glyph visible against the terminal background.
                let luminance =
                    ((rgb >> 16) & 255) * 299 + ((rgb >> 8) & 255) * 587 + (rgb & 255) * 114;
                if luminance >= 128000
                {
                    continue;
                }
                0xe5e5e5
            };
            let index = match palette.iter().position(|v| *v == value)
            {
                Some(index) => index,
                None =>
                {
                    palette.push(value);
                    palette.len() - 1
                }
            };
            indices[i] = Some(index);
        }
    }
    let mut out = b"\x1bPq\"1;1;16;16".to_vec();
    for (i, rgb) in palette.iter().enumerate()
    {
        let channel = |v: u32| (v * 100 + 127) / 255;
        out.extend_from_slice(format!("#{i};2;{};{};{}",
                                      channel((rgb >> 16) & 255),
                                      channel((rgb >> 8) & 255),
                                      channel(rgb & 255)).as_bytes());
    }
    for band in 0..3
    {
        let mut first = true;
        for color in 0..palette.len()
        {
            if !(band * 6..(band * 6 + 6).min(16)).any(|y| {
                                                      (0..16).any(|x| {
                                                                 indices[y * 16 + x] == Some(color)
                                                             })
                                                  })
            {
                continue;
            }
            if !first
            {
                out.push(b'$');
            }
            first = false;
            out.extend_from_slice(format!("#{color}").as_bytes());
            for x in 0..16
            {
                let mut bits = 0;
                for y in 0..6
                {
                    if band * 6 + y < 16 && indices[(band * 6 + y) * 16 + x] == Some(color)
                    {
                        bits |= 1 << y;
                    }
                }
                out.push(b'?' + bits);
            }
        }
        if band < 2
        {
            out.push(b'-');
        }
    }
    out.extend_from_slice(b"\x1b\\");
    out
}
