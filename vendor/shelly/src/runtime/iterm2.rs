
use std::fmt::Write;

use base64::{ Engine as _, engine::general_purpose::STANDARD };

use image::{ ExtendedColorType, ImageEncoder, RgbaImage, codecs::png::PngEncoder };

use super::{ banner::{ compose, image }, terminal::TerminalCapabilities };

/// Native inline PNG avoids iTerm2's opaque sixel decoding and keeps full alpha.
/// https://iterm2.com/documentation-images.html
pub fn banner(capabilities: &TerminalCapabilities) -> Option<String>
{
    let image = image(capabilities)?;
    let graphics = inline_image(&image)?;
    Some(compose(capabilities, image.width(), image.height(), &graphics))
}

fn inline_image(image: &RgbaImage) -> Option<String>
{
    let mut png = Vec::new();
    PngEncoder::new(&mut png).write_image(image.as_raw(), image.width(), image.height(),
                                        ExtendedColorType::Rgba8).ok()?;
    Some(png_sequence(&png, image.width(), image.height()))
}

fn png_sequence(png: &[u8], width: u32, height: u32) -> String
{
    let arguments = format!("inline=1;size={};width={width}px;height={height}px;preserveAspectRatio=1",
                            png.len());
    let payload = STANDARD.encode(png);
    // iTerm2 limits each OSC to 1 MiB. Normal banners use the original protocol;
    // large/Retina images use the multipart form supported since iTerm2 3.5.
    if arguments.len() + payload.len() + 16 <= 1_048_576
    { return format!("\x1b]1337;File={arguments}:{payload}\x07"); }
    let mut output = format!("\x1b]1337;MultipartFile={arguments}\x07");
    for offset in (0..payload.len()).step_by(16_384)
    {
        let end = (offset + 16_384).min(payload.len());
        let _ = write!(output, "\x1b]1337;FilePart={}\x07", &payload[offset..end]);
    }
    output.push_str("\x1b]1337;FileEnd\x07");
    output
}

#[cfg(test)]
mod tests
{
    use image::{ ImageFormat, load_from_memory_with_format };

    use super::*;

    fn decode(graphics: &str) -> RgbaImage
    {
        let (header, payload) = graphics.split_once(':').unwrap();
        let png = STANDARD.decode(payload.strip_suffix('\x07').unwrap()).unwrap();
        assert!(header.contains(&format!(";size={};", png.len())));
        load_from_memory_with_format(&png, ImageFormat::Png).unwrap().to_rgba8()
    }

    #[test]
    fn png_preserves_transparent_and_translucent_pixels()
    {
        let image = RgbaImage::from_raw(3, 1,
            vec![255, 0, 0, 0, 10, 20, 30, 64, 40, 50, 60, 255]).unwrap();
        let graphics = inline_image(&image).unwrap();
        assert!(graphics.starts_with("\x1b]1337;File=inline=1;"));
        assert!(graphics.contains(";width=3px;height=1px;preserveAspectRatio=1:"));
        assert_eq!(decode(&graphics), image);
    }

    #[test]
    fn large_payload_is_split_into_bounded_control_sequences()
    {
        let png = vec![0; 800_000];
        let graphics = png_sequence(&png, 1400, 1400);
        assert!(graphics.starts_with("\x1b]1337;MultipartFile=inline=1;size=800000;"));
        assert!(graphics.ends_with("\x1b]1337;FileEnd\x07"));
        let mut payload = String::new();
        for sequence in graphics.split('\x07')
        {
            assert!(sequence.len() < 1_048_576);
            if let Some(part) = sequence.strip_prefix("\x1b]1337;FilePart=")
            {
                assert!(part.len() <= 16_384);
                payload.push_str(part);
            }
        }
        assert_eq!(STANDARD.decode(payload).unwrap(), png);
    }

    #[test]
    fn banner_embeds_fitted_png_with_alpha_and_welcome_text()
    {
        let capabilities = TerminalCapabilities
        {
            cell_size: Some((10, 20)), screen_size: Some((160, 40)),
            ..TerminalCapabilities::default()
        };
        let output = banner(&capabilities).unwrap();
        let start = output.find("\x1b]1337;File=").unwrap();
        let end = start + output[start..].find('\x07').unwrap() + 1;
        let graphics = &output[start..end];
        assert!(graphics.contains(";width=350px;height=350px;"));
        let png = decode(graphics);
        assert_eq!(png.dimensions(), (350, 350));
        assert!(png.pixels().any(|pixel| pixel[3] == 0));
        assert!(png.pixels().any(|pixel| (1..255).contains(&pixel[3])));
        assert!(png.pixels().any(|pixel| pixel[3] == 255));
        assert!(output.starts_with(&("\r\n".repeat(18) + "\x1b[18A\r\x1b7")));
        assert!(output[end..].starts_with("\x1b8"));
        assert!(output.contains("\x1b[38C\x1b[38;2;159;107;255mS H E L L Y"));
        assert!(output.contains("${HOSTNAME}"));
        assert!(output.contains("https://github.com/cstrainge/shelly"));
        assert!(output.ends_with("\r\n"));
        assert!(!output.contains("\x1bP"));
    }

    #[test]
    fn native_and_sixel_share_layout_at_default_and_reported_cell_sizes()
    {
        for (cell_size, screen_size, dimensions) in [
            (None, None, (280, 280)),
            (Some((12, 10)), Some((160, 40)), (180, 180)),
            (Some((10, 20)), Some((160, 10)), (160, 160)),
            (Some((10, 20)), Some((40, 40)), (200, 200)),
        ]
        {
            let capabilities = TerminalCapabilities { cell_size, screen_size,
                ..TerminalCapabilities::default() };
            let native = banner(&capabilities).unwrap();
            assert!(native.contains(&format!(";width={}px;height={}px;",
                                             dimensions.0, dimensions.1)));
            let sixel = super::super::sixel::banner(&capabilities).unwrap();
            assert_eq!(native.split_once("\x1b8").unwrap().1,
                       sixel.split_once("\x1b8").unwrap().1);
            let native_prefix = native.split_once("\x1b]1337;").unwrap().0;
            let sixel_prefix = sixel.split_once("\x1bP0;1;0q").unwrap().0;
            assert_eq!(native_prefix, sixel_prefix);
        }
    }
}
