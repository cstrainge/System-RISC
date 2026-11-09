//! One-bit Macintosh FONT/NFNT strikes, drawn at their original integer metrics.
//! Asset identities and extraction offsets: provenance/system7-font-assets.json.
#![forbid(unsafe_code)]
use super::{Canvas, Rect};

#[derive(Clone, Copy)]
pub struct BitmapFont<'a>
{
    data: &'a [u8]
}

pub const CHICAGO_12: BitmapFont<'static> =
    BitmapFont { data: include_bytes!("assets/chicago-12.font") };
pub const GENEVA_9: BitmapFont<'static> =
    BitmapFont { data: include_bytes!("assets/geneva-9.font") };

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics
{
    pub ascent:  i16,
    pub descent: i16,
    pub leading: i16
}

struct Strike<'a>
{
    data:      &'a [u8],
    first:     u16,
    last:      u16,
    row_bytes: usize,
    height:    usize,
    locations: usize,
    widths:    usize,
    kern:      i16,
    metrics:   Metrics
}

fn word(data: &[u8], offset: usize) -> u16
{
    u16::from_be_bytes([data[offset], data[offset + 1]])
}

impl<'a> Strike<'a>
{
    fn parse(data: &'a [u8]) -> Option<Self>
    {
        if data.len() < 26 || word(data, 0) & 0x020c != 0
        {
            return None; // This renderer accepts only one-bit, uncolored strikes.
        }
        let first = word(data, 2);
        let last = word(data, 4);
        let row_bytes = usize::from(word(data, 24)).checked_mul(2)?;
        let height = usize::from(word(data, 14));
        if first > last || last > 255 || row_bytes == 0 || height == 0 || height > 32767
        {
            return None;
        }
        let locations = 26usize.checked_add(row_bytes.checked_mul(height)?)?;
        let count = usize::from(last - first) + 3;
        // A nonnegative nDescent extends owTLoc for very large font resources.
        let high = word(data, 10) as i16;
        let offset =
            usize::from(word(data, 16)) | if high >= 0 { (high as usize) << 16 } else { 0 };
        let widths = 16usize.checked_add(offset.checked_mul(2)?)?;
        if locations.checked_add(count.checked_mul(2)?)? > data.len()
           || widths < locations + count * 2
           || widths.checked_add((count - 1).checked_mul(2)?)? > data.len()
        {
            return None;
        }
        let mut previous = 0;
        for index in 0..count
        {
            let location = usize::from(word(data, locations + index * 2));
            if location < previous || location > row_bytes * 8
            {
                return None;
            }
            previous = location;
        }
        let metrics = Metrics { ascent:  word(data, 18) as i16,
                                descent: word(data, 20) as i16,
                                leading: word(data, 22) as i16 };
        if metrics.ascent < 0 || metrics.descent < 0
        {
            return None;
        }
        Some(Self { data,
                    first,
                    last,
                    row_bytes,
                    height,
                    locations,
                    widths,
                    kern: word(data, 8) as i16,
                    metrics })
    }

    fn glyph(&self, character: char) -> (usize, usize, i16, u8)
    {
        let code = roman(character).map(u16::from);
        let missing = usize::from(self.last - self.first) + 1;
        let index = code.filter(|&c| (self.first..=self.last).contains(&c))
                        .map_or(missing, |c| usize::from(c - self.first));
        let index =
            if word(self.data, self.widths + index * 2) == 0xffff { missing } else { index };
        let entry = word(self.data, self.widths + index * 2);
        if entry == 0xffff
        {
            return (0, 0, 0, 0);
        }
        (usize::from(word(self.data, self.locations + index * 2)),
         usize::from(word(self.data, self.locations + (index + 1) * 2)),
         i16::from((entry >> 8) as u8 as i8) + self.kern,
         entry as u8)
    }
}

impl<'a> BitmapFont<'a>
{
    pub fn parse(data: &'a [u8]) -> Option<Self>
    {
        Strike::parse(data)?;
        Some(Self { data })
    }

    fn strike(&self) -> Strike<'_>
    {
        // Embedded assets are checked in tests; externally supplied fonts must
        // pass the only public constructor, which validates every table boundary.
        Strike::parse(self.data).expect("validated bitmap font")
    }

    pub fn metrics(&self) -> Metrics
    {
        self.strike().metrics
    }

    pub(super) fn is_monospaced(&self) -> bool
    {
        let strike = self.strike();
        let mut width = None;
        // Classic Monaco's special non-ASCII symbols can have narrower
        // advances and its metadata need not declare fixed pitch. Text columns
        // are defined by its printable ASCII advances, not those symbols.
        for code in 32u8..=126
        {
            let (_, _, _, advance) = strike.glyph(char::from(code));
            if advance == 0
            {
                continue;
            }
            if width.is_some_and(|old| old != advance)
            {
                return false;
            }
            width = Some(advance);
        }
        width.is_some()
    }

    pub fn maximum_width(&self) -> i16
    {
        word(self.data, 6).min(i16::MAX as u16) as i16
    }

    /// Complete strike bitmap extent relative to the baseline. Line-layout
    /// descent need not contain all stored ink, so it is not a drawing bound.
    pub(super) fn vertical_ink_bounds(&self) -> (i32, i32)
    {
        let top = -i32::from(word(self.data, 18));
        (top, top + i32::from(word(self.data, 14)))
    }

    /// Visit original one-bit glyph pixels relative to its baseline and pen.
    pub(super) fn glyph_ink(&self, character: char, mut pixel: impl FnMut(i32, i32)) -> i16
    {
        let strike = self.strike();
        let (start, end, bearing, advance) = strike.glyph(character);
        for row in 0..strike.height
        {
            for bit in start..end
            {
                if strike.data[26 + row * strike.row_bytes + bit / 8] & (0x80 >> (bit & 7)) != 0
                {
                    pixel(i32::from(bearing) + (bit - start) as i32,
                          row as i32 - i32::from(strike.metrics.ascent));
                }
            }
        }
        i16::from(advance)
    }

    pub fn width(&self, text: &str) -> i16
    {
        let strike = self.strike();
        text.chars().fold(0i16, |width, c| width.saturating_add(i16::from(strike.glyph(c).3)))
    }

    /// Draw transparent glyph ink at a QuickDraw baseline, preserving the port clip.
    pub fn draw(&self, canvas: &mut Canvas<'_>, x: i16, baseline: i16, text: &str, color: u32)
    {
        self.draw_span(canvas, x, baseline, text, color, None);
    }

    /// Draw within [x, x+max_width), clipping individual glyphs at the right edge.
    pub fn draw_clipped(&self, canvas: &mut Canvas<'_>, x: i16, baseline: i16, text: &str,
                        max_width: i16, color: u32)
    {
        self.draw_span(canvas, x, baseline, text, color, Some(i32::from(max_width.max(0))));
    }

    fn draw_span(&self, canvas: &mut Canvas<'_>, x: i16, baseline: i16, text: &str, color: u32,
                 limit: Option<i32>)
    {
        let strike = self.strike();
        let mut pen = i32::from(x);
        let top = i32::from(baseline) - i32::from(strike.metrics.ascent);
        for c in text.chars()
        {
            if limit.is_some_and(|limit| pen >= i32::from(x) + limit)
            {
                break;
            }
            let (start, end, bearing, advance) = strike.glyph(c);
            for row in 0..strike.height
            {
                let y = top + row as i32;
                for bit in start..end
                {
                    if strike.data[26 + row * strike.row_bytes + bit / 8] & (0x80 >> (bit & 7)) == 0
                    {
                        continue;
                    }
                    let px = pen + i32::from(bearing) + (bit - start) as i32;
                    if limit.is_some_and(|limit| px < i32::from(x) || px >= i32::from(x) + limit)
                    {
                        continue;
                    }
                    if let (Ok(px), Ok(y)) = (i16::try_from(px), i16::try_from(y))
                       && px < i16::MAX
                       && y < i16::MAX
                    {
                        canvas.paint_rect(Rect::new(px, y, px + 1, y + 1), color);
                    }
                }
            }
            pen = pen.saturating_add(i32::from(advance));
        }
    }
}

// Mac Roman's non-ASCII half; no allocation is needed to map native UTF-8 titles.
const HIGH: [char; 128] =
    ['\u{c4}', '\u{c5}', '\u{c7}', '\u{c9}', '\u{d1}', '\u{d6}', '\u{dc}', '\u{e1}', '\u{e0}',
     '\u{e2}', '\u{e4}', '\u{e3}', '\u{e5}', '\u{e7}', '\u{e9}', '\u{e8}', '\u{ea}', '\u{eb}',
     '\u{ed}', '\u{ec}', '\u{ee}', '\u{ef}', '\u{f1}', '\u{f3}', '\u{f2}', '\u{f4}', '\u{f6}',
     '\u{f5}', '\u{fa}', '\u{f9}', '\u{fb}', '\u{fc}', '\u{2020}', '\u{b0}', '\u{a2}', '\u{a3}',
     '\u{a7}', '\u{2022}', '\u{b6}', '\u{df}', '\u{ae}', '\u{a9}', '\u{2122}', '\u{b4}', '\u{a8}',
     '\u{2260}', '\u{c6}', '\u{d8}', '\u{221e}', '\u{b1}', '\u{2264}', '\u{2265}', '\u{a5}',
     '\u{b5}', '\u{2202}', '\u{2211}', '\u{220f}', '\u{3c0}', '\u{222b}', '\u{aa}', '\u{ba}',
     '\u{3a9}', '\u{e6}', '\u{f8}', '\u{bf}', '\u{a1}', '\u{ac}', '\u{221a}', '\u{192}',
     '\u{2248}', '\u{2206}', '\u{ab}', '\u{bb}', '\u{2026}', '\u{a0}', '\u{c0}', '\u{c3}',
     '\u{d5}', '\u{152}', '\u{153}', '\u{2013}', '\u{2014}', '\u{201c}', '\u{201d}', '\u{2018}',
     '\u{2019}', '\u{f7}', '\u{25ca}', '\u{ff}', '\u{178}', '\u{2044}', '\u{a4}', '\u{2039}',
     '\u{203a}', '\u{fb01}', '\u{fb02}', '\u{2021}', '\u{b7}', '\u{201a}', '\u{201e}', '\u{2030}',
     '\u{c2}', '\u{ca}', '\u{c1}', '\u{cb}', '\u{c8}', '\u{cd}', '\u{ce}', '\u{cf}', '\u{cc}',
     '\u{d3}', '\u{d4}', '\u{f8ff}', '\u{d2}', '\u{da}', '\u{db}', '\u{d9}', '\u{131}', '\u{2c6}',
     '\u{2dc}', '\u{af}', '\u{2d8}', '\u{2d9}', '\u{2da}', '\u{b8}', '\u{2dd}', '\u{2db}',
     '\u{2c7}'];

pub(super) fn roman(c: char) -> Option<u8>
{
    if c.is_ascii()
    {
        Some(c as u8)
    }
    else
    {
        HIGH.iter().position(|&v| v == c).map(|i| i as u8 + 128)
    }
}

