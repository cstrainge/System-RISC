//! Shared no-std PNG acceptance through the public userspace API and real gate.
//! Independent sample oracles are encoded by the Python fixture generator.
use mac_toolbox::{BAD_ARGUMENT, NOT_SUPPORTED, System,
                  quickdraw::{Rect,
                              images::{ImageError, errors}}};

use super::{Failure, require};
pub struct Case
{
    pub name:   &'static str,
    pub data:   &'static [u8],
    pub width:  i16,
    pub height: i16,
    pub depth:  u8,
    pub color:  u8,
    pub alpha:  bool,
    pub rgba:   &'static [u8]
}
include!("png_oracle.rs");
fn service(error: ImageError) -> Failure
{
    Failure::Service(error.status)
}
pub fn run(system: &System<'_>) -> Result<usize, Failure>
{
    let images = system.quickdraw().images();
    for case in CASES
    {
        let info = images.info(case.data).map_err(service)?;
        if info.width != case.width as u32
           || info.height != case.height as u32
           || info.bit_depth != case.depth
           || info.color_type != case.color
           || (info.flags & errors::HAS_ALPHA != 0) != case.alpha
        {
            return Err(Failure::Observation { case:  case.name,
                                              field: "PNG metadata" });
        }
        let n = case.width as usize * case.height as usize;
        require(n <= 25 && case.rgba.len() == n * 4, "PNG oracle dimensions")?;
        let mut pixels = [0x204060; 25];
        let r = Rect::new(0, 0, case.width, case.height);
        images.draw_to_surface(case.data,
                               &mut pixels[..n],
                               case.width as usize,
                               case.height as usize,
                               r,
                               r)
              .map_err(service)?;
        for (index, p) in case.rgba.as_chunks::<4>().0.iter().enumerate()
        {
            let alpha = u32::from(p[3]);
            let mut expected = 0;
            for (i, shift) in [16, 8, 0].into_iter().enumerate()
            {
                let source = u32::from(p[i]);
                let dest = (0x204060 >> shift) & 255;
                expected |= ((source * alpha + dest * (255 - alpha) + 127) / 255) << shift;
            }
            if pixels[index] != expected
            {
                return Err(Failure::Observation { case:  case.name,
                                                  field: "PNG RGB pixel" });
            }
        }
    }
    let alpha = include_bytes!("../data/png/alpha-composite.png");
    let mut pixels = [0x204060; 24];
    images.draw_to_surface(alpha, &mut pixels, 6, 4, Rect::new(0, 0, 6, 4), Rect::new(1, 1, 5, 3))
          .map_err(service)?;
    let expected = [0xff0000, 0x10a030, 0x204060, 0x4a4954, 0x13263a, 0x214161];
    for y in 0..4
    {
        for x in 0..6
        {
            let color = if (1..5).contains(&x) && (1..3).contains(&y)
            {
                expected[(y / 2) * 3 + x / 2]
            }
            else
            {
                0x204060
            };
            require(pixels[y * 6 + x] == color, "PNG scale/clip/alpha")?;
        }
    }
    let invalid: [(&[u8], i32, u32); 8] =
        [(include_bytes!("../data/png/adam7.png"), NOT_SUPPORTED, errors::UNSUPPORTED),
         (include_bytes!("../data/png/apng.png"), NOT_SUPPORTED, errors::UNSUPPORTED),
         (include_bytes!("../data/png/short-inflate.png"),
          BAD_ARGUMENT,
          errors::BAD_COMPRESSED_DATA),
         (include_bytes!("../data/png/long-inflate.png"),
          BAD_ARGUMENT,
          errors::BAD_COMPRESSED_DATA),
         (include_bytes!("../data/png/bad-filter.png"), BAD_ARGUMENT, errors::BAD_COMPRESSED_DATA),
         (include_bytes!("../data/png/bad-adler.png"), BAD_ARGUMENT, errors::BAD_COMPRESSED_DATA),
         (include_bytes!("../data/png/inflate-bomb.png"),
          BAD_ARGUMENT,
          errors::BAD_COMPRESSED_DATA),
         (include_bytes!("../data/png/palette-index-outside.png"),
          BAD_ARGUMENT,
          errors::INVALID_PALETTE)];
    for (data, status, kind) in invalid
    {
        let mut pixels = [0x123456; 6];
        let r = Rect::new(0, 0, 3, 2);
        require(images.draw_to_surface(data, &mut pixels, 3, 2, r, r)
                == Err(ImageError { status,
                                    kind }),
                "PNG invalid/unsupported error")?;
        require(pixels == [0x123456; 6], "PNG failed draw atomic")?;
    }
    Ok(CASES.len())
}
