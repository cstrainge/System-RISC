//! Strict bounded PNG framing, no-allocation decoders, fallible owned image storage.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{BAD_ARGUMENT, NO_MEMORY, NOT_SUPPORTED, Rect, images::*};

use super::{Canvas,
            service::{self, Port}};
fn error(kind: u32) -> ImageError
{
    ImageError { status: match kind
                 {
                     UNSUPPORTED => NOT_SUPPORTED,
                     LIMIT_EXCEEDED | ALLOCATION_FAILED => NO_MEMORY,
                     _ => BAD_ARGUMENT
                 },
                 kind }
}
fn be32(b: &[u8]) -> u32
{
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}
fn be16(b: &[u8]) -> u16
{
    u16::from_be_bytes([b[0], b[1]])
}
const fn crc_table() -> [u32; 256]
{
    let mut table = [0; 256];
    let mut i = 0;
    while i < 256
    {
        let mut x = i as u32;
        let mut n = 0;
        while n < 8
        {
            x = if x & 1 != 0 { (x >> 1) ^ 0xedb88320 } else { x >> 1 };
            n += 1;
        }
        table[i] = x;
        i += 1;
    }
    table
}
const CRC_TABLE: [u32; 256] = crc_table();
fn crc(b: &[u8]) -> u32
{
    let mut c = 0xffffffff;
    for x in b
    {
        c = (c >> 8) ^ CRC_TABLE[((c as u8) ^ x) as usize];
    }
    !c
}
struct Metadata<'a>
{
    info:         ImageInfo,
    palette:      &'a [u8],
    transparency: &'a [u8],
    row_bytes:    usize,
    raw_bytes:    usize
}
fn metadata(data: &[u8]) -> Result<Metadata<'_>>
{
    if data.len() > MAX_INPUT_BYTES
    {
        return Err(error(LIMIT_EXCEEDED));
    }
    if data.get(..8) != Some(b"\x89PNG\r\n\x1a\n".as_slice())
    {
        return Err(error(BAD_SIGNATURE));
    }
    let mut out = Metadata { info:         ImageInfo::default(),
                             palette:      &[],
                             transparency: &[],
                             row_bytes:    0,
                             raw_bytes:    0 };
    let (mut offset,
         mut count,
         mut seen_header,
         mut seen_palette,
         mut seen_trns,
         mut seen_data,
         mut data_closed,
         mut compressed) = (8usize, 0usize, false, false, false, false, false, 0usize);
    while offset < data.len()
    {
        count += 1;
        if count > 4096
        {
            return Err(error(LIMIT_EXCEEDED));
        }
        let head = data.get(offset..offset + 8).ok_or(error(TRUNCATED))?;
        let n = be32(head) as usize;
        if n > 0x7fffffff
        {
            return Err(error(INVALID_CHUNK));
        }
        let end =
            offset.checked_add(n).and_then(|x| x.checked_add(12)).ok_or(error(INVALID_CHUNK))?;
        let chunk = data.get(offset..end).ok_or(error(TRUNCATED))?;
        let kind = &head[4..8];
        let body = &chunk[8..8 + n];
        if !kind.iter().all(u8::is_ascii_alphabetic) || kind[2] & 0x20 != 0
        {
            return Err(error(INVALID_CHUNK));
        }
        if crc(&chunk[4..8 + n]) != be32(&chunk[8 + n..])
        {
            return Err(error(CRC_MISMATCH));
        }
        if !seen_header && kind != b"IHDR"
        {
            return Err(error(INVALID_HEADER));
        }
        if seen_data && kind != b"IDAT"
        {
            data_closed = true;
        }
        match kind
        {
            b"IHDR" =>
            {
                if seen_header || n != 13
                {
                    return Err(error(INVALID_HEADER));
                }
                seen_header = true;
                let (width, height, depth, color) =
                    (be32(body), be32(&body[4..]), body[8], body[9]);
                let legal = match color
                {
                    0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
                    2 | 4 | 6 => matches!(depth, 8 | 16),
                    3 => matches!(depth, 1 | 2 | 4 | 8),
                    _ => false
                };
                if width == 0
                   || height == 0
                   || width > 0x7fffffff
                   || height > 0x7fffffff
                   || !legal
                   || body[10] != 0
                   || body[11] != 0
                   || body[12] > 1
                {
                    return Err(error(INVALID_HEADER));
                }
                if width > MAX_DIMENSION
                   || height > MAX_DIMENSION
                   || (width as usize).checked_mul(height as usize)
                                      .filter(|x| *x <= MAX_PIXELS)
                                      .is_none()
                {
                    return Err(error(LIMIT_EXCEEDED));
                }
                if body[12] == 1
                {
                    return Err(error(UNSUPPORTED));
                }
                out.info =
                    ImageInfo { width,
                                height,
                                bit_depth: depth,
                                color_type: color,
                                flags: if color == 4 || color == 6 { HAS_ALPHA } else { 0 } };
                let channels = match color
                {
                    0 | 3 => 1,
                    2 => 3,
                    4 => 2,
                    6 => 4,
                    _ => return Err(error(INVALID_HEADER))
                };
                out.row_bytes = (width as usize * channels * depth as usize).div_ceil(8);
                out.raw_bytes = (out.row_bytes + 1) * height as usize;
            }
            b"PLTE" =>
            {
                if seen_palette
                   || seen_data
                   || seen_trns
                   || matches!(out.info.color_type, 0 | 4)
                   || n == 0
                   || n > 768
                   || !n.is_multiple_of(3)
                   || out.info.color_type == 3 && n / 3 > 1usize << out.info.bit_depth
                {
                    return Err(error(INVALID_PALETTE));
                }
                seen_palette = true;
                out.palette = body;
            }
            b"tRNS" =>
            {
                if seen_trns || seen_data
                {
                    return Err(error(INVALID_CHUNK));
                }
                let max =
                    if out.info.bit_depth == 16 { 65535 } else { (1u32 << out.info.bit_depth) - 1 };
                let legal = match out.info.color_type
                {
                    0 => n == 2 && u32::from(be16(body)) <= max,
                    2 => n == 6 && (0..3).all(|i| u32::from(be16(&body[i * 2..])) <= max),
                    3 => seen_palette && n > 0 && n <= out.palette.len() / 3,
                    _ => false
                };
                if !legal
                {
                    return Err(error(INVALID_PALETTE));
                }
                seen_trns = true;
                out.transparency = body;
                out.info.flags |= HAS_ALPHA;
            }
            b"IDAT" =>
            {
                if data_closed || out.info.color_type == 3 && !seen_palette
                {
                    return Err(error(INVALID_CHUNK));
                }
                seen_data = true;
                compressed += n;
            }
            b"IEND" =>
            {
                if n != 0 || !seen_data || compressed == 0 || end != data.len()
                {
                    return Err(error(INVALID_CHUNK));
                }
                return Ok(out);
            }
            b"acTL" | b"fcTL" | b"fdAT" => return Err(error(UNSUPPORTED)),
            _ =>
            {
                if kind[0] & 0x20 == 0
                {
                    return Err(error(UNSUPPORTED));
                }
            }
        }
        offset = end;
    }
    Err(error(TRUNCATED))
}
/// Header/chunk validation does not inflate pixel data.
pub fn info(data: &[u8]) -> Result<ImageInfo>
{
    Ok(metadata(data)?.info)
}
struct Idats<'a>
{
    data:   &'a [u8],
    offset: usize
}
impl<'a> Iterator for Idats<'a>
{
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item>
    {
        while self.offset < self.data.len()
        {
            let at = self.offset;
            let n = be32(&self.data[at..]) as usize;
            self.offset += n + 12;
            if &self.data[at + 4..at + 8] == b"IDAT"
            {
                return Some(&self.data[at + 8..at + 8 + n]);
            }
        }
        None
    }
}
fn inflate_exact(data: &[u8], raw: &mut [u8], expected: usize) -> Result<()>
{
    use miniz_oxide::inflate::{TINFLStatus,
                               core::{DecompressorOxide, decompress, inflate_flags::*}};
    let mut state = DecompressorOxide::new();
    let mut chunks = Idats { data,
                             offset: 8 }.filter(|x| !x.is_empty())
                                        .peekable();
    let mut written = 0;
    while let Some(chunk) = chunks.next()
    {
        let flags = TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF
                    | TINFL_FLAG_PARSE_ZLIB_HEADER
                    | if chunks.peek().is_some() { TINFL_FLAG_HAS_MORE_INPUT } else { 0 };
        let (status, consumed, n) = decompress(&mut state, chunk, raw, written, flags);
        written += n;
        match status
        {
            TINFLStatus::NeedsMoreInput if consumed == chunk.len() =>
            {}
            TINFLStatus::Done
                if consumed == chunk.len() && chunks.next().is_none() && written == expected =>
            {
                return Ok(());
            }
            _ => return Err(error(BAD_COMPRESSED_DATA))
        }
    }
    Err(error(BAD_COMPRESSED_DATA))
}
fn buffer(n: usize) -> Result<Vec<u8>>
{
    let mut out = Vec::new();
    out.try_reserve_exact(n).map_err(|_| error(ALLOCATION_FAILED))?;
    out.resize(n, 0);
    Ok(out)
}
pub struct Image
{
    pub info: ImageInfo,
    rgba:     Vec<u8>
}
impl Image
{
    pub fn decode(data: &[u8]) -> Result<Self>
    {
        let m = metadata(data)?;
        let mut scratch = buffer(m.raw_bytes + 1)?;
        inflate_exact(data, &mut scratch, m.raw_bytes)?;
        let decoded =
            minipng::decode_png(data, &mut scratch[..m.raw_bytes]).map_err(|_| {
                                                                      error(BAD_COMPRESSED_DATA)
                                                                  })?;
        let source = decoded.pixels();
        let mut rgba = buffer(m.info.width as usize * m.info.height as usize * 4)?;
        let depth = m.info.bit_depth as usize;
        let color = m.info.color_type;
        let channels = match color
        {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            _ => 4
        };
        let sample = |row: &[u8], index: usize| -> u16 {
            match depth
            {
                16 => be16(&row[index * 2..]),
                8 => u16::from(row[index]),
                _ =>
                {
                    u16::from((row[index * depth / 8] >> (8 - depth - (index * depth % 8)))
                              & ((1 << depth) - 1))
                }
            }
        };
        let scale = |v: u16| -> u8 {
            if depth == 16
            {
                (v >> 8) as u8
            }
            else
            {
                (u32::from(v) * 255 / ((1u32 << depth) - 1)) as u8
            }
        };
        for y in 0..m.info.height as usize
        {
            let row = &source[y * m.row_bytes..(y + 1) * m.row_bytes];
            for x in 0..m.info.width as usize
            {
                let at = x * channels;
                let s = sample(row, at);
                let pixel = match color
                {
                    0 =>
                    {
                        let a = if !m.transparency.is_empty() && s == be16(m.transparency)
                        {
                            0
                        }
                        else
                        {
                            255
                        };
                        [scale(s), scale(s), scale(s), a]
                    }
                    2 =>
                    {
                        let (g, b) = (sample(row, at + 1), sample(row, at + 2));
                        let a = if !m.transparency.is_empty()
                                   && [s, g, b]
                                      == [be16(m.transparency),
                                          be16(&m.transparency[2..]),
                                          be16(&m.transparency[4..])]
                        {
                            0
                        }
                        else
                        {
                            255
                        };
                        [scale(s), scale(g), scale(b), a]
                    }
                    3 =>
                    {
                        let index = s as usize;
                        let p = m.palette
                                 .get(index * 3..index * 3 + 3)
                                 .ok_or(error(INVALID_PALETTE))?;
                        [p[0], p[1], p[2], m.transparency.get(index).copied().unwrap_or(255)]
                    }
                    4 => [scale(s), scale(s), scale(s), scale(sample(row, at + 1))],
                    _ =>
                    {
                        [scale(s),
                         scale(sample(row, at + 1)),
                         scale(sample(row, at + 2)),
                         scale(sample(row, at + 3))]
                    }
                };
                let out = (y * m.info.width as usize + x) * 4;
                rgba[out..out + 4].copy_from_slice(&pixel);
            }
        }
        Ok(Self { info: m.info,
                  rgba })
    }
}
pub struct Drawing
{
    pub image:       Image,
    pub destination: Rect,
    pub port:        Port
}
impl Drawing
{
    pub fn new(data: &[u8], destination: Rect, port: Port) -> Result<Self>
    {
        if destination.is_empty()
        {
            return Err(error(INVALID_DESTINATION));
        }
        Ok(Self { image: Image::decode(data)?,
                  destination,
                  port })
    }

    pub fn storage(&self) -> usize
    {
        self.image.rgba.len() + self.port.storage() + core::mem::size_of::<Self>()
    }

    pub fn paint(&self, canvas: &mut Canvas<'_>)
    {
        service::with_clip(canvas, &self.port, |canvas| self.paint_clipped(canvas));
    }

    fn paint_clipped(&self, canvas: &mut Canvas<'_>)
    {
        let r = self.destination;
        let origin = canvas.origin();
        let clip = canvas.clip();
        let l = i32::from(r.left) + i32::from(origin.h);
        let t = i32::from(r.top) + i32::from(origin.v);
        let w = i32::from(r.right) - i32::from(r.left);
        let h = i32::from(r.bottom) - i32::from(r.top);
        let left = l.max(i32::from(clip.left)).max(0);
        let top = t.max(i32::from(clip.top)).max(0);
        let right = (l + w).min(i32::from(clip.right)).min(canvas.width as i32);
        let bottom = (t + h).min(i32::from(clip.bottom)).min(canvas.height as i32);
        for y in top..bottom
        {
            for x in left..right
            {
                let sx = ((x - l) * self.image.info.width as i32 / w) as usize;
                let sy = ((y - t) * self.image.info.height as i32 / h) as usize;
                let p = &self.image.rgba[(sy * self.image.info.width as usize + sx) * 4..][..4];
                let alpha = u32::from(p[3]);
                if alpha == 0
                {
                    continue;
                }
                let out = &mut canvas.pixels[y as usize * canvas.width + x as usize];
                let mut color = 0;
                for (i, shift) in [16, 8, 0].into_iter().enumerate()
                {
                    color |=
                        ((u32::from(p[i]) * alpha + ((*out >> shift) & 255) * (255 - alpha) + 127)
                         / 255)
                        << shift;
                }
                *out = color;
            }
        }
    }
}
