use super::{Canvas, Point, Rect, WHITE};
pub fn cursor(c: &mut Canvas, point: Point)
{
    // Classic Mac-style 16x16 monochrome arrow: black image, white mask edge,
    // and transparent pixels outside the mask. The black tip is the (1,1) hotspot.
    const IMAGE: [u16; 16] = [0x0000, 0x4000, 0x6000, 0x7000, 0x7800, 0x7c00, 0x7e00, 0x7f00,
                              0x7f80, 0x7fc0, 0x6c00, 0x4600, 0x0600, 0x0300, 0x0300, 0x0000];
    const MASK: [u16; 16] = [0xc000, 0xe000, 0xf000, 0xf800, 0xfc00, 0xfe00, 0xff00, 0xff80,
                             0xffc0, 0xffe0, 0xfe00, 0xef00, 0xcf00, 0x8780, 0x0780, 0x0380];
    for row in 0..16
    {
        for column in 0..16
        {
            let bit = 0x8000 >> column;
            if MASK[row] & bit != 0
            {
                let x = point.h - 1 + column;
                let y = point.v - 1 + row as i16;
                c.paint_rect(Rect::new(x, y, x + 1, y + 1),
                             if IMAGE[row] & bit != 0 { 0x000000 } else { WHITE });
            }
        }
    }
}
