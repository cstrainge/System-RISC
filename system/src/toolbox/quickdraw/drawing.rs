//! Clipped framebuffer surface shared by resident QuickDraw and window definitions.
//! Stateful shapes, regions, bitmap transfers and resource fonts live in adjacent modules.
#![forbid(unsafe_code)]
use super::geometry::{Rect, sect_rect};

pub const BLACK: u32 = 0x18212b;
pub const WHITE: u32 = 0xffffff;

pub struct Canvas<'a>
{
    pub pixels: &'a mut [u32],
    pub width:  usize,
    pub height: usize,
    clip:       Rect,
    origin:     super::geometry::Point
}

impl<'a> Canvas<'a>
{
    pub fn origin(&self) -> super::geometry::Point
    {
        self.origin
    }

    pub fn clip(&self) -> Rect
    {
        self.clip
    }

    pub fn new(pixels: &'a mut [u32], width: usize, height: usize) -> Self
    {
        assert!(width * height <= pixels.len()
                && width <= i16::MAX as usize
                && height <= i16::MAX as usize);
        Self { pixels,
               width,
               height,
               clip: Rect::new(0, 0, width as i16, height as i16),
               origin: super::geometry::Point::default() }
    }

    pub fn set_origin(&mut self, h: i16, v: i16)
    {
        self.origin = super::geometry::Point { h,
                                               v };
    }

    pub fn reset_port(&mut self)
    {
        self.origin = super::geometry::Point::default();
        self.clip = Rect::new(0, 0, self.width as i16, self.height as i16);
    }

    fn global(&self, mut rect: Rect) -> Rect
    {
        rect.offset(self.origin.h, self.origin.v);
        rect
    }

    pub fn set_clip(&mut self, clip: Rect)
    {
        sect_rect(self.global(clip),
                  Rect::new(0, 0, self.width as i16, self.height as i16),
                  &mut self.clip);
    }

    pub fn paint_rect(&mut self, rect: Rect, color: u32)
    {
        let mut clipped = Rect::ZERO;
        if !sect_rect(self.global(rect), self.clip, &mut clipped)
        {
            return;
        }
        for y in clipped.top as usize..clipped.bottom as usize
        {
            self.pixels
                [y * self.width + clipped.left as usize..y * self.width + clipped.right as usize]
                .fill(color);
        }
    }

    pub fn fill_rect(&mut self, rect: Rect, pattern: [u8; 8], foreground: u32, background: u32)
    {
        let mut clipped = Rect::ZERO;
        if !sect_rect(self.global(rect), self.clip, &mut clipped)
        {
            return;
        }
        for y in clipped.top as usize..clipped.bottom as usize
        {
            for x in clipped.left as usize..clipped.right as usize
            {
                self.pixels[y * self.width + x] =
                    if pattern[y & 7] & (0x80 >> (x & 7)) != 0 { foreground } else { background };
            }
        }
    }

    pub fn frame_rect(&mut self, rect: Rect, color: u32)
    {
        if rect.is_empty()
        {
            return;
        }
        self.paint_rect(Rect::new(rect.left, rect.top, rect.right, rect.top + 1), color);
        self.paint_rect(Rect::new(rect.left, rect.bottom - 1, rect.right, rect.bottom), color);
        self.paint_rect(Rect::new(rect.left, rect.top, rect.left + 1, rect.bottom), color);
        self.paint_rect(Rect::new(rect.right - 1, rect.top, rect.right, rect.bottom), color);
    }

    pub fn invert_rect(&mut self, rect: Rect)
    {
        let mut clipped = Rect::ZERO;
        if !sect_rect(self.global(rect), self.clip, &mut clipped)
        {
            return;
        }
        for y in clipped.top as usize..clipped.bottom as usize
        {
            for x in clipped.left as usize..clipped.right as usize
            {
                self.pixels[y * self.width + x] ^= 0xffffff;
            }
        }
    }

    /// Original compact demo lettering; not a Macintosh font implementation.
    pub fn text(&mut self, x: i16, y: i16, text: &str, scale: i16, color: u32)
    {
        let mut left = x;
        for c in text.bytes()
        {
            for (row, bits) in glyph(c.to_ascii_uppercase()).iter().enumerate()
            {
                for column in 0..5
                {
                    if bits & (16 >> column) != 0
                    {
                        let px = left + column * scale;
                        let py = y + row as i16 * scale;
                        self.paint_rect(Rect::new(px, py, px + scale, py + scale), color);
                    }
                }
            }
            left += 6 * scale;
        }
    }
}

fn glyph(c: u8) -> [u8; 7]
{
    match c
    {
        b'A' => [14, 17, 17, 31, 17, 17, 17],
        b'B' => [30, 17, 17, 30, 17, 17, 30],
        b'C' => [14, 17, 16, 16, 16, 17, 14],
        b'D' => [30, 17, 17, 17, 17, 17, 30],
        b'E' => [31, 16, 16, 30, 16, 16, 31],
        b'F' => [31, 16, 16, 30, 16, 16, 16],
        b'G' => [14, 17, 16, 23, 17, 17, 15],
        b'H' => [17, 17, 17, 31, 17, 17, 17],
        b'I' => [14, 4, 4, 4, 4, 4, 14],
        b'J' => [7, 2, 2, 2, 2, 18, 12],
        b'K' => [17, 18, 20, 24, 20, 18, 17],
        b'L' => [16, 16, 16, 16, 16, 16, 31],
        b'M' => [17, 27, 21, 21, 17, 17, 17],
        b'N' => [17, 25, 25, 21, 19, 19, 17],
        b'O' => [14, 17, 17, 17, 17, 17, 14],
        b'P' => [30, 17, 17, 30, 16, 16, 16],
        b'Q' => [14, 17, 17, 17, 21, 18, 13],
        b'R' => [30, 17, 17, 30, 20, 18, 17],
        b'S' => [15, 16, 16, 14, 1, 1, 30],
        b'T' => [31, 4, 4, 4, 4, 4, 4],
        b'U' => [17, 17, 17, 17, 17, 17, 14],
        b'V' => [17, 17, 17, 17, 17, 10, 4],
        b'W' => [17, 17, 17, 21, 21, 21, 10],
        b'X' => [17, 17, 10, 4, 10, 17, 17],
        b'Y' => [17, 17, 10, 4, 4, 4, 4],
        b'Z' => [31, 1, 2, 4, 8, 16, 31],
        b'0' => [14, 17, 19, 21, 25, 17, 14],
        b'1' => [4, 12, 4, 4, 4, 4, 14],
        b'2' => [14, 17, 1, 2, 4, 8, 31],
        b'3' => [30, 1, 1, 14, 1, 1, 30],
        b'4' => [2, 6, 10, 18, 31, 2, 2],
        b'5' => [31, 16, 16, 30, 1, 1, 30],
        b'6' => [14, 16, 16, 30, 17, 17, 14],
        b'7' => [31, 1, 2, 4, 8, 8, 8],
        b'8' => [14, 17, 17, 14, 17, 17, 14],
        b'9' => [14, 17, 17, 15, 1, 1, 14],
        b'-' => [0, 0, 0, 31, 0, 0, 0],
        b'+' => [0, 4, 4, 31, 4, 4, 0],
        b'=' => [0, 0, 31, 0, 31, 0, 0],
        b'/' => [1, 2, 2, 4, 8, 8, 16],
        b':' => [0, 4, 4, 0, 4, 4, 0],
        b'.' => [0, 0, 0, 0, 0, 12, 12],
        b',' => [0, 0, 0, 0, 0, 4, 8],
        b'>' => [16, 8, 4, 2, 4, 8, 16],
        b'(' => [2, 4, 8, 8, 8, 4, 2],
        b')' => [8, 4, 2, 2, 2, 4, 8],
        _ => [0; 7]
    }
}

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn local_window_coordinates_and_clip_stay_inside_port()
    {
        let mut pixels = [WHITE; 64];
        let mut c = Canvas::new(&mut pixels, 8, 8);
        c.set_origin(3, 2);
        c.set_clip(Rect::new(0, 0, 3, 4));
        c.paint_rect(Rect::new(-10, -10, 10, 10), BLACK);
        for y in 0..8
        {
            for x in 0..8
            {
                assert_eq!(c.pixels[y * 8 + x],
                           if (3..6).contains(&x) && (2..6).contains(&y) { BLACK } else { WHITE });
            }
        }
        c.reset_port();
        c.paint_rect(Rect::new(0, 0, 1, 1), BLACK);
        assert_eq!(c.pixels[0], BLACK);
    }

    #[test]
    fn clipping_and_inverse_preserve_pixels_outside_surface()
    {
        let mut pixels = [WHITE; 18];
        let mut c = Canvas::new(&mut pixels[..16], 4, 4);
        c.set_clip(Rect::new(1, 1, 3, 3));
        c.paint_rect(Rect::new(-20, -20, 20, 20), BLACK);
        c.invert_rect(Rect::new(-20, -20, 20, 20));
        c.invert_rect(Rect::new(-20, -20, 20, 20));
        for y in 0..4
        {
            for x in 0..4
            {
                assert_eq!(c.pixels[y * 4 + x],
                           if (1..3).contains(&x) && (1..3).contains(&y) { BLACK } else { WHITE });
            }
        }
        assert_eq!(pixels[16..], [WHITE; 2]);
    }

    #[test]
    fn pattern_phase_uses_surface_coordinates()
    {
        let mut pixels = [WHITE; 64];
        let mut c = Canvas::new(&mut pixels, 8, 8);
        c.fill_rect(Rect::new(1, 1, 5, 5),
                    [0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55],
                    BLACK,
                    WHITE);
        assert_eq!(c.pixels[9], BLACK);
        assert_eq!(c.pixels[10], WHITE);
        assert_eq!(c.pixels[17], WHITE);
        assert_eq!(c.pixels[18], BLACK);
    }
}
