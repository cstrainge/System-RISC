//! Checked classic PICT v1/v2 decoding, supported-command recording and playback.
//!
//! File operands remain big endian and v2 opcodes remain word aligned. Unknown
//! drawing semantics fail explicitly instead of guessing how many bytes to skip.
#![forbid(unsafe_code)]

use alloc::vec::Vec;

use super::{Canvas, Point, Rect,
            fonts::{FontManager, FontPreference, FontSelection},
            geometry,
            port::{self, PortState, Shape, Verb},
            raster::{self, Bitmap, TransferMode},
            region::{BooleanOp, Region},
            service};

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_COMMANDS: usize = 4096;
const MAX_PIXELS: usize = 512 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Truncated,
    InvalidHeader,
    InvalidData,
    TooLarge,
    Allocation,
    CoordinateOverflow,
    UnsupportedOpcode(u16),
    UnsupportedMode(i16),
    UnsupportedState,
    FontUnavailable
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version
{
    One,
    Two
}

#[derive(Debug, PartialEq, Eq)]
pub enum PictureShape
{
    Rect(Rect),
    Oval(Rect),
    RoundRect(Rect),
    Arc
    {
        bounds: Rect,
        start:  i16,
        sweep:  i16
    },
    Polygon(Vec<Point>),
    Region(Region)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureVerb
{
    Frame,
    Paint,
    Erase,
    Invert,
    Fill
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextPosition
{
    Absolute(Point),
    Delta
    {
        dh: u8,
        dv: u8
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct PictureBitmap
{
    pub bounds:      Rect,
    pub row_bytes:   usize,
    pub depth:       u8,
    /// Unpacked rows. 32-bit direct rows use XRGB byte order.
    pub data:        Vec<u8>,
    pub palette:     Vec<u32>,
    pub source:      Rect,
    pub destination: Rect,
    pub mode:        i16,
    pub mask:        Option<Region>
}

impl PictureBitmap
{
    fn bitmap(&self) -> Result<Bitmap<'_>, Error>
    {
        Ok(match self.depth
        {
            1 if self.palette.is_empty() =>
            {
                Bitmap::Mono { data:      &self.data,
                               row_bytes: self.row_bytes,
                               bounds:    self.bounds }
            }
            1 | 2 | 4 | 8 =>
            {
                Bitmap::Indexed { data:      &self.data,
                                  row_bytes: self.row_bytes,
                                  bounds:    self.bounds,
                                  depth:     self.depth,
                                  palette:   &self.palette }
            }
            16 =>
            {
                Bitmap::Rgb16 { data:      &self.data,
                                row_bytes: self.row_bytes,
                                bounds:    self.bounds }
            }
            32 =>
            {
                Bitmap::Xrgb32 { data:      &self.data,
                                 row_bytes: self.row_bytes,
                                 bounds:    self.bounds }
            }
            _ => return Err(Error::UnsupportedState)
        })
    }

    fn validate(&self) -> Result<(), Error>
    {
        self.bitmap()?.validate().map_err(|_| Error::InvalidData)?;
        if self.source.is_empty()
           || self.source.left < self.bounds.left
           || self.source.top < self.bounds.top
           || self.source.right > self.bounds.right
           || self.source.bottom > self.bounds.bottom
        {
            return Err(Error::InvalidData);
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command
{
    Clip(Region),
    BackgroundPattern([u8; 8]),
    PenPattern([u8; 8]),
    FillPattern([u8; 8]),
    TextFont(i16),
    TextFace(u8),
    TextMode(i16),
    SpaceExtra(i32),
    PenSize(Point),
    PenMode(i16),
    OvalSize(Point),
    Origin(Point),
    TextSize(i16),
    Foreground(u32),
    Background(u32),
    RgbForeground([u16; 3]),
    RgbBackground([u16; 3]),
    TextRatio
    {
        numerator:   Point,
        denominator: Point
    },
    PenFraction(u16),
    CharExtra(i16),
    FontName
    {
        id:   i16,
        name: Vec<u8>
    },
    GlyphState
    {
        outline_preferred: bool,
        preserve_glyph:    bool,
        fractional_widths: bool,
        scaling_disabled:  bool
    },
    Line
    {
        from: Point,
        to:   Point
    },
    Text
    {
        position: TextPosition,
        bytes:    Vec<u8>
    },
    Draw
    {
        shape: PictureShape,
        verb:  PictureVerb
    },
    Bitmap(PictureBitmap),
    Comment
    {
        kind: u16,
        data: Vec<u8>
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Picture
{
    pub bounds:   Rect,
    pub version:  Version,
    pub commands: Vec<Command>
}

struct Reader<'a>
{
    bytes:    &'a [u8],
    position: usize
}
impl<'a> Reader<'a>
{
    fn take(&mut self, count: usize) -> Result<&'a [u8], Error>
    {
        let end = self.position.checked_add(count).ok_or(Error::TooLarge)?;
        let value = self.bytes.get(self.position..end).ok_or(Error::Truncated)?;
        self.position = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, Error>
    {
        Ok(self.take(1)?[0])
    }

    fn word(&mut self) -> Result<u16, Error>
    {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn short(&mut self) -> Result<i16, Error>
    {
        Ok(self.word()? as i16)
    }

    fn long(&mut self) -> Result<u32, Error>
    {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn point(&mut self) -> Result<Point, Error>
    {
        let v = self.short()?;
        Ok(Point { h: self.short()?,
                   v })
    }

    fn rect(&mut self) -> Result<Rect, Error>
    {
        let top = self.short()?;
        let left = self.short()?;
        let bottom = self.short()?;
        let right = self.short()?;
        Ok(Rect::new(left, top, right, bottom))
    }

    fn pattern(&mut self) -> Result<[u8; 8], Error>
    {
        Ok(self.take(8)?.try_into().unwrap())
    }

    fn rgb(&mut self) -> Result<u32, Error>
    {
        let r = self.word()?;
        let g = self.word()?;
        let b = self.word()?;
        Ok((u32::from(r >> 8) << 16) | (u32::from(g >> 8) << 8) | u32::from(b >> 8))
    }

    fn region(&mut self) -> Result<Region, Error>
    {
        let start = self.position;
        let size = self.word()? as usize;
        if size < 10 || size & 1 != 0
        {
            return Err(Error::InvalidData);
        }
        self.take(size - 2)?;
        Region::from_be_bytes(&self.bytes[start..self.position]).map_err(|_| Error::InvalidData)
    }

    fn pstring(&mut self) -> Result<Vec<u8>, Error>
    {
        let size = self.byte()? as usize;
        super::storage::copy(self.take(size)?).map_err(|_| Error::Allocation)
    }
}

fn classic_color(value: u32) -> Result<u32, Error>
{
    match value
    {
        30 => Ok(0xffffff),
        33 => Ok(0),
        69 => Ok(0xffff00),
        137 => Ok(0xff00ff),
        205 => Ok(0xff0000),
        273 => Ok(0x00ffff),
        341 => Ok(0x00ff00),
        409 => Ok(0x0000ff),
        _ => Err(Error::UnsupportedState)
    }
}
fn verb(value: u16) -> Result<PictureVerb, Error>
{
    match value
    {
        0 => Ok(PictureVerb::Frame),
        1 => Ok(PictureVerb::Paint),
        2 => Ok(PictureVerb::Erase),
        3 => Ok(PictureVerb::Invert),
        4 => Ok(PictureVerb::Fill),
        _ => Err(Error::UnsupportedOpcode(value))
    }
}
fn transfer(mode: i16) -> Result<TransferMode, Error>
{
    TransferMode::new(mode).ok_or(Error::UnsupportedMode(mode))
}

fn unpack_bits(bytes: &[u8], expected: usize, unit: usize) -> Result<Vec<u8>, Error>
{
    let mut r = Reader { bytes,
                         position: 0 };
    let mut output = super::storage::vector(expected).map_err(|_| Error::Allocation)?;
    while r.position < bytes.len()
    {
        let control = r.byte()? as i8;
        if control >= 0
        {
            let count = (control as usize + 1).checked_mul(unit).ok_or(Error::TooLarge)?;
            if output.len() + count > expected
            {
                return Err(Error::InvalidData);
            }
            output.extend_from_slice(r.take(count)?);
        }
        else if control != -128
        {
            let count = (1i16 - i16::from(control)) as usize;
            let value = r.take(unit)?;
            if output.len() + count * unit > expected
            {
                return Err(Error::InvalidData);
            }
            for _ in 0..count
            {
                output.extend_from_slice(value);
            }
        }
    }
    if output.len() != expected
    {
        return Err(Error::InvalidData);
    }
    Ok(output)
}

fn read_bitmap(r: &mut Reader<'_>, opcode: u16) -> Result<PictureBitmap, Error>
{
    let direct = matches!(opcode, 0x9a | 0x9b);
    if direct
    {
        r.long()?;
    }
    let raw_row = r.word()?;
    let row_bytes = usize::from(raw_row & 0x3fff);
    let bounds = r.rect()?;
    if bounds.is_empty() || row_bytes == 0
    {
        return Err(Error::InvalidData);
    }
    let width = (i32::from(bounds.right) - i32::from(bounds.left)) as usize;
    let height = (i32::from(bounds.bottom) - i32::from(bounds.top)) as usize;
    if width.checked_mul(height).is_none_or(|n| n > MAX_PIXELS)
       || row_bytes.checked_mul(height).is_none_or(|n| n > MAX_BYTES)
    {
        return Err(Error::TooLarge);
    }
    let pixmap = raw_row & 0x8000 != 0;
    let mut depth = 1u8;
    let mut pack_type = 0u16;
    let mut components = 1u16;
    let mut palette = Vec::new();
    if pixmap
    {
        r.word()?;
        pack_type = r.word()?;
        r.long()?;
        r.long()?;
        r.long()?;
        let pixel_type = r.word()?;
        depth = r.word()?.try_into().map_err(|_| Error::InvalidData)?;
        components = r.word()?;
        let component_size = r.word()?;
        r.long()?;
        r.long()?;
        r.long()?;
        if direct
        {
            if !((pixel_type == 16 && depth == 16 && components == 3 && component_size == 5)
                 || (pixel_type == 16
                     && depth == 32
                     && matches!(components, 3 | 4)
                     && component_size == 8))
            {
                return Err(Error::UnsupportedState);
            }
        }
        else
        {
            if pixel_type != 0
               || !matches!(depth, 1 | 2 | 4 | 8)
               || components != 1
               || component_size != u16::from(depth)
            {
                return Err(Error::UnsupportedState);
            }
            r.long()?;
            let flags = r.word()?;
            let count = usize::from(r.word()?) + 1;
            if count > 256
            {
                return Err(Error::TooLarge);
            }
            palette.try_reserve_exact(1usize << depth).map_err(|_| Error::Allocation)?;
            palette.resize(1usize << depth, 0);
            for index in 0..count
            {
                let value = r.word()? as usize;
                let color = r.rgb()?;
                let slot = if flags & 0x8000 != 0 { index } else { value };
                *palette.get_mut(slot).ok_or(Error::InvalidData)? = color;
            }
        }
    }
    else if direct
    {
        return Err(Error::InvalidData);
    }
    if row_bytes < width.checked_mul(usize::from(depth)).ok_or(Error::TooLarge)?.div_ceil(8)
    {
        return Err(Error::InvalidData);
    }
    let source = r.rect()?;
    let destination = r.rect()?;
    let mode = r.short()?;
    transfer(mode)?;
    let mask = if matches!(opcode, 0x91 | 0x99 | 0x9b) { Some(r.region()?) } else { None };
    let mut data = super::storage::vector(row_bytes * height).map_err(|_| Error::Allocation)?;
    let mut final_row = row_bytes;
    if direct && pack_type == 2
    {
        if depth != 32 || row_bytes % 4 != 0
        {
            return Err(Error::InvalidData);
        }
        final_row = row_bytes / 4 * 4;
        for _ in 0..height
        {
            for rgb in r.take(row_bytes / 4 * 3)?.as_chunks::<3>().0
            {
                data.push(0);
                data.extend_from_slice(rgb);
            }
        }
    }
    else if row_bytes < 8 || pack_type == 1 || matches!(opcode, 0x90 | 0x91)
    {
        data.extend_from_slice(r.take(row_bytes * height)?);
    }
    else
    {
        if direct && !matches!(pack_type, 0 | 3 | 4)
        {
            return Err(Error::UnsupportedState);
        }
        let effective =
            if direct && pack_type == 0 { if depth == 32 { 4 } else { 3 } } else { pack_type };
        for _ in 0..height
        {
            let size =
                if row_bytes > 250 { usize::from(r.word()?) } else { usize::from(r.byte()?) };
            let packed = r.take(size)?;
            if direct && effective == 4
            {
                let expected =
                    (row_bytes / 4).checked_mul(usize::from(components)).ok_or(Error::TooLarge)?;
                let row = unpack_bits(packed, expected, 1)?;
                let stride = row_bytes / 4;
                for x in 0..stride
                {
                    data.push(if components == 4 { row[x] } else { 0 });
                    let first = usize::from(components - 3) * stride;
                    data.extend_from_slice(&[row[first + x],
                                             row[first + stride + x],
                                             row[first + 2 * stride + x]]);
                }
            }
            else
            {
                data.extend_from_slice(&unpack_bits(packed,
                                                    row_bytes,
                                                    if direct && effective == 3 { 2 } else { 1 })?);
            }
        }
    }
    if direct && depth == 32
    {
        final_row = row_bytes;
    }
    Ok(PictureBitmap { bounds,
                       row_bytes: final_row,
                       depth,
                       data,
                       palette,
                       source,
                       destination,
                       mode,
                       mask })
}

impl Picture
{
    /// Decode raw PICT resource/handle bytes. A disk-file's 512-byte wrapper is
    /// intentionally separate; use `parse_file` for that explicit carrier.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error>
    {
        if bytes.len() > MAX_BYTES
        {
            return Err(Error::TooLarge);
        }
        let mut r = Reader { bytes,
                             position: 0 };
        r.word()?;
        let bounds = r.rect()?;
        if bounds.is_empty()
        {
            return Err(Error::InvalidHeader);
        }
        let version = match r.byte()?
        {
            0x11 =>
            {
                if r.byte()? != 1
                {
                    return Err(Error::InvalidHeader);
                }
                Version::One
            }
            0 =>
            {
                if r.byte()? != 0x11 || r.word()? != 0x02ff
                {
                    return Err(Error::InvalidHeader);
                }
                Version::Two
            }
            _ => return Err(Error::InvalidHeader)
        };
        let mut result = Self { bounds,
                                version,
                                commands: Vec::new() };
        let mut last = [None; 4];
        let mut line = Point::default();
        let mut decoded_bytes = 0usize;
        loop
        {
            if result.commands.len() >= MAX_COMMANDS
            {
                return Err(Error::TooLarge);
            }
            if version == Version::Two && r.position & 1 != 0
            {
                r.byte()?;
            }
            let opcode = if version == Version::Two { r.word()? } else { u16::from(r.byte()?) };
            let command =
                match opcode
                {
                    0 => continue,
                    0xff => break,
                    0x01 => Command::Clip(r.region()?),
                    0x02 => Command::BackgroundPattern(r.pattern()?),
                    0x03 => Command::TextFont(r.short()?),
                    0x04 => Command::TextFace(r.byte()?),
                    0x05 => Command::TextMode(r.short()?),
                    0x06 => Command::SpaceExtra(r.long()? as i32),
                    0x07 => Command::PenSize(r.point()?),
                    0x08 => Command::PenMode(r.short()?),
                    0x09 => Command::PenPattern(r.pattern()?),
                    0x0a => Command::FillPattern(r.pattern()?),
                    0x0b => Command::OvalSize(r.point()?),
                    0x0c =>
                    {
                        let h = r.short()?;
                        Command::Origin(Point { h,
                                                v: r.short()? })
                    }
                    0x0d => Command::TextSize(r.short()?),
                    0x0e => Command::Foreground(classic_color(r.long()?)?),
                    0x0f => Command::Background(classic_color(r.long()?)?),
                    0x10 =>
                    {
                        Command::TextRatio { numerator:   r.point()?,
                                             denominator: r.point()? }
                    }
                    0x15 => Command::PenFraction(r.word()?),
                    0x16 => Command::CharExtra(r.short()?),
                    0x1a => Command::RgbForeground([r.word()?, r.word()?, r.word()?]),
                    0x1b => Command::RgbBackground([r.word()?, r.word()?, r.word()?]),
                    0x20 =>
                    {
                        let from = r.point()?;
                        let to = r.point()?;
                        line = to;
                        Command::Line { from,
                                        to }
                    }
                    0x21 =>
                    {
                        let from = line;
                        let to = r.point()?;
                        line = to;
                        Command::Line { from,
                                        to }
                    }
                    0x22 | 0x23 =>
                    {
                        let from = if opcode == 0x22 { r.point()? } else { line };
                        let dh = r.byte()? as i8;
                        let dv = r.byte()? as i8;
                        let to = Point { h: from.h.wrapping_add(i16::from(dh)),
                                         v: from.v.wrapping_add(i16::from(dv)) };
                        line = to;
                        Command::Line { from,
                                        to }
                    }
                    0x28 =>
                    {
                        Command::Text { position: TextPosition::Absolute(r.point()?),
                                        bytes:    r.pstring()? }
                    }
                    0x29..=0x2b =>
                    {
                        let dh = if opcode != 0x2a { r.byte()? } else { 0 };
                        let dv = if opcode != 0x29 { r.byte()? } else { 0 };
                        Command::Text { position: TextPosition::Delta { dh,
                                                                        dv },
                                        bytes:    r.pstring()? }
                    }
                    0x2c =>
                    {
                        let size = usize::from(r.word()?);
                        let payload = r.take(size)?;
                        let mut body = Reader { bytes:    payload,
                                                position: 0 };
                        let id = body.short()?;
                        let name = body.pstring()?;
                        if body.position != size
                        {
                            return Err(Error::InvalidData);
                        }
                        Command::FontName { id,
                                            name }
                    }
                    0x2e =>
                    {
                        if r.word()? != 4
                        {
                            return Err(Error::UnsupportedState);
                        }
                        Command::GlyphState { outline_preferred: r.byte()? != 0,
                                              preserve_glyph:    r.byte()? != 0,
                                              fractional_widths: r.byte()? != 0,
                                              scaling_disabled:  r.byte()? != 0 }
                    }
                    0x30..=0x6c if opcode & 7 <= 4 =>
                    {
                        let family = ((opcode - 0x30) / 16) as usize;
                        let same = opcode & 8 != 0;
                        let bounds = if same
                        {
                            last[family].ok_or(Error::InvalidData)?
                        }
                        else
                        {
                            let rect = r.rect()?;
                            last[family] = Some(rect);
                            rect
                        };
                        let shape = match family
                        {
                            0 => PictureShape::Rect(bounds),
                            1 => PictureShape::RoundRect(bounds),
                            2 => PictureShape::Oval(bounds),
                            3 =>
                            {
                                PictureShape::Arc { bounds,
                                                    start: r.short()?,
                                                    sweep: r.short()? }
                            }
                            _ => return Err(Error::InvalidData)
                        };
                        Command::Draw { shape,
                                        verb: verb(opcode & 7)? }
                    }
                    0x70..=0x74 =>
                    {
                        let size = usize::from(r.word()?);
                        if size < 10 || (size - 10) % 4 != 0
                        {
                            return Err(Error::InvalidData);
                        }
                        let mut body = Reader { bytes:    r.take(size - 2)?,
                                                position: 0 };
                        body.rect()?;
                        let mut points =
                            super::storage::vector((size - 10) / 4).map_err(|_| Error::Allocation)?;
                        while body.position < body.bytes.len()
                        {
                            points.push(body.point()?);
                        }
                        Command::Draw { shape: PictureShape::Polygon(points),
                                        verb:  verb(opcode & 7)? }
                    }
                    0x80..=0x84 =>
                    {
                        Command::Draw { shape: PictureShape::Region(r.region()?),
                                        verb:  verb(opcode & 7)? }
                    }
                    0x90 | 0x91 | 0x98..=0x9b => Command::Bitmap(read_bitmap(&mut r, opcode)?),
                    0xa0 =>
                    {
                        Command::Comment { kind: r.word()?,
                                           data: Vec::new() }
                    }
                    0xa1 =>
                    {
                        let kind = r.word()?;
                        let size = usize::from(r.word()?);
                        Command::Comment { kind,
                                       data: super::storage::copy(r.take(size)?)
                                           .map_err(|_| Error::Allocation)? }
                    }
                    0x0c00 if version == Version::Two =>
                    {
                        r.take(24)?;
                        continue;
                    }
                    _ => return Err(Error::UnsupportedOpcode(opcode))
                };
            if let Command::Bitmap(bitmap) = &command
            {
                decoded_bytes =
                    decoded_bytes.checked_add(bitmap.data.len()).ok_or(Error::TooLarge)?;
                if decoded_bytes > MAX_BYTES
                {
                    return Err(Error::TooLarge);
                }
            }
            result.commands.try_reserve(1).map_err(|_| Error::Allocation)?;
            result.commands.push(command);
        }
        // PicSize is only a 16-bit legacy hint; real large v2 handles can exceed it.
        Ok(result)
    }

    pub fn parse_file(bytes: &[u8]) -> Result<Self, Error>
    {
        Self::parse(bytes.get(512..).ok_or(Error::Truncated)?)
    }

    pub fn new(bounds: Rect) -> Result<Self, Error>
    {
        if bounds.is_empty()
        {
            return Err(Error::InvalidHeader);
        }
        Ok(Self { bounds,
                  version: Version::Two,
                  commands: Vec::new() })
    }

    /// Record the same checked, inspectable command vocabulary used by PICT.
    pub fn record(&mut self, command: Command) -> Result<(), Error>
    {
        if self.commands.len() >= MAX_COMMANDS
        {
            return Err(Error::TooLarge);
        }
        self.commands.try_reserve(1).map_err(|_| Error::Allocation)?;
        self.commands.push(command);
        Ok(())
    }

    /// Validate the initial supported semantic subset before retained playback.
    /// Unsupported state is reported even when the picture would be clipped.
    pub fn validate_supported(&self) -> Result<(), Error>
    {
        if self.bounds.is_empty() || self.commands.len() > MAX_COMMANDS
        {
            return Err(Error::InvalidHeader);
        }
        let mut total = 0usize;
        for command in &self.commands
        {
            match command
            {
                Command::Origin(p) if *p != Point::default() =>
                {
                    return Err(Error::UnsupportedState);
                }
                Command::TextMode(mode) if *mode != 1 => return Err(Error::UnsupportedMode(*mode)),
                Command::SpaceExtra(value) if *value != 0 => return Err(Error::UnsupportedState),
                Command::CharExtra(value) if *value != 0 => return Err(Error::UnsupportedState),
                Command::TextRatio { numerator,
                                     denominator }
                    if numerator != denominator || denominator.h == 0 || denominator.v == 0 =>
                {
                    return Err(Error::UnsupportedState);
                }
                Command::PenFraction(value) if *value != 0 && *value != 0x8000 =>
                {
                    return Err(Error::UnsupportedState);
                }
                Command::GlyphState { preserve_glyph,
                                      fractional_widths,
                                      scaling_disabled,
                                      .. } if *preserve_glyph || *fractional_widths || *scaling_disabled =>
                {
                    return Err(Error::UnsupportedState);
                }
                Command::PenMode(mode) =>
                {
                    transfer(*mode)?;
                }
                Command::PenSize(p) if p.h < 0 || p.v < 0 => return Err(Error::InvalidData),
                Command::TextSize(value) if *value < 0 || *value > 512 =>
                {
                    return Err(Error::InvalidData);
                }
                Command::Bitmap(bitmap) =>
                {
                    transfer(bitmap.mode)?;
                    bitmap.validate()?;
                    total = total.checked_add(bitmap.data.len()).ok_or(Error::TooLarge)?;
                    if total > MAX_BYTES
                    {
                        return Err(Error::TooLarge);
                    }
                    if !matches!(bitmap.depth, 1 | 2 | 4 | 8 | 16 | 32)
                       || bitmap.depth != 1 && bitmap.depth < 16 && bitmap.palette.is_empty()
                    {
                        return Err(Error::InvalidData);
                    }
                }
                _ =>
                {}
            }
        }
        Ok(())
    }

    /// Resolve text against the resident catalog before a retained command is accepted.
    pub fn validate_with_fonts(&self, fonts: &FontManager) -> Result<(), Error>
    {
        self.validate_supported()?;
        let mut selection = FontSelection::default();
        let mut preference = FontPreference::BitmapPreferred;
        for command in &self.commands
        {
            match command
            {
                Command::TextFont(family) => selection.family = *family,
                Command::TextFace(face) => selection.face = u16::from(*face),
                Command::TextSize(size) => selection.size = *size,
                Command::FontName { id,
                                    name }
                    if selection.family == *id =>
                {
                    selection.family = fonts.family_id(&mac_formats::text::decode(name))
                                            .ok_or(Error::FontUnavailable)?
                }
                Command::GlyphState { outline_preferred,
                                      .. } =>
                {
                    preference = if *outline_preferred
                    {
                        FontPreference::OutlinePreferred
                    }
                    else
                    {
                        FontPreference::BitmapPreferred
                    }
                }
                Command::Text { bytes,
                                .. } =>
                {
                    fonts.text_width_with_preference(selection,
                                                     &mac_formats::text::decode(bytes),
                                                     preference)
                         .map_err(|_| Error::FontUnavailable)?;
                }
                _ =>
                {}
            }
        }
        Ok(())
    }

    /// Check destination-dependent limitations before queuing retained playback.
    pub fn validate_destination(&self, destination: Rect) -> Result<(), Error>
    {
        if self.bounds.is_empty() || destination.is_empty()
        {
            return Err(Error::InvalidHeader);
        }
        let sw = i64::from(self.bounds.right) - i64::from(self.bounds.left);
        let sh = i64::from(self.bounds.bottom) - i64::from(self.bounds.top);
        let dw = i64::from(destination.right) - i64::from(destination.left);
        let dh = i64::from(destination.bottom) - i64::from(destination.top);
        if dw * sh != dh * sw
           && self.commands.iter().any(|command| matches!(command, Command::Text { .. }))
        {
            return Err(Error::UnsupportedState);
        }
        Ok(())
    }

    pub fn replay(&self, canvas: &mut Canvas<'_>, fonts: &FontManager, destination: Rect)
                  -> Result<(), Error>
    {
        self.validate_destination(destination)?;
        self.validate_with_fonts(fonts)?;
        let old_origin = canvas.origin();
        let old_clip = canvas.clip();
        let mut frame = destination;
        frame.offset(old_origin.h, old_origin.v);
        let clip = geometry::sect_rect(frame, old_clip, &mut frame);
        canvas.set_origin(0, 0);
        canvas.set_clip(frame);
        canvas.set_origin(old_origin.h, old_origin.v);
        let result = if clip { self.replay_inner(canvas, fonts, destination) } else { Ok(()) };
        canvas.set_origin(0, 0);
        canvas.set_clip(old_clip);
        canvas.set_origin(old_origin.h, old_origin.v);
        result
    }

    fn replay_inner(&self, canvas: &mut Canvas<'_>, fonts: &FontManager, destination: Rect)
                    -> Result<(), Error>
    {
        let map = |p: Point| {
            geometry::map_point(p, self.bounds, destination).map_err(|_| Error::CoordinateOverflow)
        };
        let rect = |r: Rect| {
            geometry::map_rect(r, self.bounds, destination).map_err(|_| Error::CoordinateOverflow)
        };
        let size = |p: Point| -> Result<Point, Error> {
            let source=Point{h:(i32::from(self.bounds.right)-i32::from(self.bounds.left)).try_into().map_err(|_|Error::CoordinateOverflow)?,v:(i32::from(self.bounds.bottom)-i32::from(self.bounds.top)).try_into().map_err(|_|Error::CoordinateOverflow)?};
            let target=Point{h:(i32::from(destination.right)-i32::from(destination.left)).try_into().map_err(|_|Error::CoordinateOverflow)?,v:(i32::from(destination.bottom)-i32::from(destination.top)).try_into().map_err(|_|Error::CoordinateOverflow)?};
            geometry::scale_point(p, target, source).map_err(|_| Error::CoordinateOverflow)
        };
        let mut current = service::Port { state:  PortState::default(),
                                          origin: Point::default() };
        let mut font = FontSelection::default();
        let mut font_preference = FontPreference::BitmapPreferred;
        let mut oval = Point { h: 16,
                               v: 16 };
        let mut fill = [255; 8];
        let mut text_location = Point::default();
        let mut text_mode = 1;
        let mut extra = 0;
        let mut char_extra = 0;
        let mut ratio = (Point { h: 1,
                                 v: 1 },
                         Point { h: 1,
                                 v: 1 });
        for command in &self.commands
        {
            match command
            {
                Command::Clip(region) =>
                {
                    let bounded =
                        region.combine(&Region::from_rect(self.bounds), BooleanOp::Intersection);
                    current.state.clip = Some(bounded.map(self.bounds, destination)
                                                     .map_err(|_| Error::CoordinateOverflow)?);
                }
                Command::BackgroundPattern(value) => current.state.background_pattern = *value,
                Command::PenPattern(value) => current.state.pen_pattern = *value,
                Command::FillPattern(value) => fill = *value,
                Command::PenSize(value) => current.state.pen_size = size(*value)?,
                Command::PenMode(value) => current.state.pen_mode = transfer(*value)?,
                Command::OvalSize(value) => oval = size(*value)?,
                Command::Origin(value) =>
                {
                    if *value != Point::default()
                    {
                        return Err(Error::UnsupportedState);
                    }
                }
                Command::Foreground(value) => current.state.foreground = *value,
                Command::Background(value) => current.state.background = *value,
                Command::RgbForeground(rgb) =>
                {
                    current.state.foreground = (u32::from(rgb[0] >> 8) << 16)
                                               | (u32::from(rgb[1] >> 8) << 8)
                                               | u32::from(rgb[2] >> 8)
                }
                Command::RgbBackground(rgb) =>
                {
                    current.state.background = (u32::from(rgb[0] >> 8) << 16)
                                               | (u32::from(rgb[1] >> 8) << 8)
                                               | u32::from(rgb[2] >> 8)
                }
                Command::TextFont(value) => font.family = *value,
                Command::TextFace(value) => font.face = u16::from(*value),
                Command::TextMode(value) => text_mode = *value,
                Command::TextSize(value) =>
                {
                    font.size = size(Point { h: *value,
                                             v: *value })?.v;
                    char_extra = 0;
                }
                Command::SpaceExtra(value) => extra = *value,
                Command::CharExtra(value) => char_extra = *value,
                Command::TextRatio { numerator,
                                     denominator } => ratio = (*numerator, *denominator),
                Command::PenFraction(value) =>
                {
                    if *value != 0 && *value != 0x8000
                    {
                        return Err(Error::UnsupportedState);
                    }
                }
                Command::FontName { id,
                                    name } =>
                {
                    if font.family == *id
                    {
                        let name = mac_formats::text::decode(name);
                        font.family = fonts.family_id(&name).ok_or(Error::FontUnavailable)?;
                    }
                }
                Command::GlyphState { outline_preferred,
                                      preserve_glyph,
                                      fractional_widths,
                                      scaling_disabled } =>
                {
                    if *preserve_glyph || *fractional_widths || *scaling_disabled
                    {
                        return Err(Error::UnsupportedState);
                    }
                    font_preference = if *outline_preferred
                    {
                        FontPreference::OutlinePreferred
                    }
                    else
                    {
                        FontPreference::BitmapPreferred
                    };
                }
                Command::Line { from,
                                to } =>
                {
                    current.state.pen_location = map(*from)?;
                    let end = map(*to)?;
                    let mut failure = None;
                    service::with_clip(canvas, &current, |canvas| {
                        let mut state = current.state.clone();
                        if port::line_to(canvas, &mut state, end).is_err()
                        {
                            failure = Some(Error::InvalidData)
                        }
                    });
                    if let Some(error) = failure
                    {
                        return Err(error);
                    }
                    current.state.pen_location = end;
                }
                Command::Text { position,
                                bytes } =>
                {
                    text_location = match position
                    {
                        TextPosition::Absolute(point) => *point,
                        TextPosition::Delta { dh,
                                              dv } =>
                        {
                            Point { h: text_location.h
                                                    .checked_add(i16::from(*dh))
                                                    .ok_or(Error::CoordinateOverflow)?,
                                    v: text_location.v
                                                    .checked_add(i16::from(*dv))
                                                    .ok_or(Error::CoordinateOverflow)? }
                        }
                    };
                    if text_mode != 1 || extra != 0 || char_extra != 0 || ratio.0 != ratio.1
                    {
                        return Err(Error::UnsupportedState);
                    }
                    let text = mac_formats::text::decode(bytes);
                    let baseline = map(text_location)?;
                    let measured = fonts.text_width_with_preference(font, &text, font_preference)
                                        .map_err(|_| Error::FontUnavailable)?;
                    let mut failure = None;
                    service::with_clip(canvas, &current, |canvas| {
                        if fonts.draw_with_preference(font, canvas, baseline, &text,
                                                      current.state.foreground, font_preference)
                                .is_err()
                        {
                            failure = Some(Error::FontUnavailable)
                        }
                    });
                    if let Some(error) = failure
                    {
                        return Err(error);
                    }
                    let source_width = i32::from(self.bounds.right) - i32::from(self.bounds.left);
                    let target_width = i32::from(destination.right) - i32::from(destination.left);
                    text_location.h =
                        text_location.h
                                     .checked_add((i32::from(measured) * source_width
                                                   / target_width)
                                                                  .try_into()
                                                                  .map_err(|_| {
                                                                      Error::CoordinateOverflow
                                                                  })?)
                                     .ok_or(Error::CoordinateOverflow)?;
                }
                Command::Draw { shape,
                                verb } =>
                {
                    let verb = match verb
                    {
                        PictureVerb::Frame => Verb::Frame,
                        PictureVerb::Paint => Verb::Paint,
                        PictureVerb::Erase => Verb::Erase,
                        PictureVerb::Invert => Verb::Invert,
                        PictureVerb::Fill => Verb::Fill(fill)
                    };
                    let mut points;
                    let region;
                    let mapped = match shape
                    {
                        PictureShape::Rect(r) => Shape::Rect(rect(*r)?),
                        PictureShape::Oval(r) => Shape::Oval(rect(*r)?),
                        PictureShape::RoundRect(r) =>
                        {
                            Shape::RoundRect { bounds:      rect(*r)?,
                                               oval_width:  oval.h,
                                               oval_height: oval.v }
                        }
                        PictureShape::Arc { bounds,
                                            start,
                                            sweep } =>
                        {
                            Shape::Arc { bounds: rect(*bounds)?,
                                         start:  *start,
                                         sweep:  *sweep }
                        }
                        PictureShape::Polygon(values) =>
                        {
                            points = super::storage::vector(values.len()).map_err(|_| {
                                                                             Error::Allocation
                                                                         })?;
                            for point in values
                            {
                                points.push(map(*point)?);
                            }
                            Shape::Polygon(&points)
                        }
                        PictureShape::Region(value) =>
                        {
                            region = value.map(self.bounds, destination)
                                          .map_err(|_| Error::CoordinateOverflow)?;
                            Shape::Region(&region)
                        }
                    };
                    let mut failure = None;
                    service::with_clip(canvas, &current, |canvas| {
                        if port::draw(canvas, &current.state, mapped, verb).is_err()
                        {
                            failure = Some(Error::InvalidData)
                        }
                    });
                    if let Some(error) = failure
                    {
                        return Err(error);
                    }
                }
                Command::Bitmap(value) =>
                {
                    let source = value.bitmap()?;
                    let target = rect(value.destination)?;
                    let mode = transfer(value.mode)?;
                    let mask = value.mask
                                    .as_ref()
                                    .map(|r| {
                                        r.map(self.bounds, destination)
                                         .map_err(|_| Error::CoordinateOverflow)
                                    })
                                    .transpose()?;
                    let mut failure = None;
                    service::with_clip(canvas, &current, |canvas| {
                        if raster::copy_bits(canvas,
                                             &source,
                                             value.source,
                                             target,
                                             mode,
                                             mask.as_ref(),
                                             current.state.foreground,
                                             current.state.background).is_err()
                        {
                            failure = Some(Error::InvalidData)
                        }
                    });
                    if let Some(error) = failure
                    {
                        return Err(error);
                    }
                }
                Command::Comment { .. } =>
                {}
            }
        }
        Ok(())
    }
}

struct Writer
{
    bytes: Vec<u8>
}
impl Writer
{
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error>
    {
        if self.bytes.len().checked_add(bytes.len()).is_none_or(|n| n > MAX_BYTES)
        {
            return Err(Error::TooLarge);
        }
        self.bytes.try_reserve(bytes.len()).map_err(|_| Error::Allocation)?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn byte(&mut self, v: u8) -> Result<(), Error>
    {
        self.append(&[v])
    }

    fn word(&mut self, v: u16) -> Result<(), Error>
    {
        self.append(&v.to_be_bytes())
    }

    fn short(&mut self, v: i16) -> Result<(), Error>
    {
        self.word(v as u16)
    }

    fn long(&mut self, v: u32) -> Result<(), Error>
    {
        self.append(&v.to_be_bytes())
    }

    fn point(&mut self, p: Point) -> Result<(), Error>
    {
        self.short(p.v)?;
        self.short(p.h)
    }

    fn rect(&mut self, r: Rect) -> Result<(), Error>
    {
        self.short(r.top)?;
        self.short(r.left)?;
        self.short(r.bottom)?;
        self.short(r.right)
    }

    fn opcode(&mut self, opcode: u16) -> Result<(), Error>
    {
        if self.bytes.len() & 1 != 0
        {
            self.byte(0)?;
        }
        self.word(opcode)
    }

    fn rgb(&mut self, rgb: [u16; 3]) -> Result<(), Error>
    {
        for value in rgb
        {
            self.word(value)?;
        }
        Ok(())
    }

    fn color(&mut self, color: u32) -> Result<(), Error>
    {
        self.rgb([((color >> 16) as u16 & 255) * 257,
                  ((color >> 8) as u16 & 255) * 257,
                  (color as u16 & 255) * 257])
    }

    fn string(&mut self, bytes: &[u8]) -> Result<(), Error>
    {
        self.byte(bytes.len().try_into().map_err(|_| Error::TooLarge)?)?;
        self.append(bytes)
    }

    fn region(&mut self, region: &Region) -> Result<(), Error>
    {
        self.append(&region.to_be_bytes().map_err(|_| Error::TooLarge)?)
    }
}

impl Picture
{
    /// Emit standard extended-v2 PICT bytes suitable for a PICT resource or handle.
    /// Replay state is recorded as original opcodes; there is no private format.
    pub fn encode_v2(&self) -> Result<Vec<u8>, Error>
    {
        if self.bounds.is_empty() || self.commands.len() > MAX_COMMANDS
        {
            return Err(Error::InvalidHeader);
        }
        let mut w = Writer { bytes: Vec::new() };
        w.word(0)?;
        w.rect(self.bounds)?;
        w.opcode(0x11)?;
        w.word(0x02ff)?;
        w.opcode(0x0c00)?;
        w.word(0xfffe)?;
        w.word(0)?;
        w.long(72 << 16)?;
        w.long(72 << 16)?;
        w.rect(self.bounds)?;
        w.long(0)?;
        for command in &self.commands
        {
            match command
            {
                Command::Clip(r) =>
                {
                    w.opcode(1)?;
                    w.region(r)?;
                }
                Command::BackgroundPattern(p)
                | Command::PenPattern(p)
                | Command::FillPattern(p) =>
                {
                    w.opcode(match command
                             {
                                 Command::BackgroundPattern(_) => 2,
                                 Command::PenPattern(_) => 9,
                                 _ => 10
                             })?;
                    w.append(p)?;
                }
                Command::TextFont(v)
                | Command::TextMode(v)
                | Command::PenMode(v)
                | Command::TextSize(v)
                | Command::CharExtra(v) =>
                {
                    w.opcode(match command
                             {
                                 Command::TextFont(_) => 3,
                                 Command::TextMode(_) => 5,
                                 Command::PenMode(_) => 8,
                                 Command::TextSize(_) => 13,
                                 _ => 22
                             })?;
                    w.short(*v)?;
                }
                Command::TextFace(v) =>
                {
                    w.opcode(4)?;
                    w.byte(*v)?;
                }
                Command::SpaceExtra(v) =>
                {
                    w.opcode(6)?;
                    w.long(*v as u32)?;
                }
                Command::PenSize(v) | Command::OvalSize(v) =>
                {
                    w.opcode(if matches!(command, Command::PenSize(_)) { 7 } else { 11 })?;
                    w.point(*v)?;
                }
                Command::Origin(v) =>
                {
                    w.opcode(12)?;
                    w.short(v.h)?;
                    w.short(v.v)?;
                }
                Command::Foreground(v) | Command::Background(v) =>
                {
                    w.opcode(if matches!(command, Command::Foreground(_)) { 0x1a } else { 0x1b })?;
                    w.color(*v)?;
                }
                Command::RgbForeground(v) | Command::RgbBackground(v) =>
                {
                    w.opcode(if matches!(command, Command::RgbForeground(_))
                             {
                                 0x1a
                             }
                             else
                             {
                                 0x1b
                             })?;
                    w.rgb(*v)?;
                }
                Command::TextRatio { numerator,
                                     denominator } =>
                {
                    w.opcode(16)?;
                    w.point(*numerator)?;
                    w.point(*denominator)?;
                }
                Command::PenFraction(v) =>
                {
                    w.opcode(21)?;
                    w.word(*v)?;
                }
                Command::FontName { id,
                                    name } =>
                {
                    w.opcode(0x2c)?;
                    w.word((name.len() + 3).try_into().map_err(|_| Error::TooLarge)?)?;
                    w.short(*id)?;
                    w.string(name)?;
                }
                Command::GlyphState { outline_preferred,
                                      preserve_glyph,
                                      fractional_widths,
                                      scaling_disabled } =>
                {
                    w.opcode(0x2e)?;
                    w.word(4)?;
                    w.append(&[u8::from(*outline_preferred),
                               u8::from(*preserve_glyph),
                               u8::from(*fractional_widths),
                               u8::from(*scaling_disabled)])?;
                }
                Command::Line { from,
                                to } =>
                {
                    w.opcode(0x20)?;
                    w.point(*from)?;
                    w.point(*to)?;
                }
                Command::Text { position,
                                bytes } =>
                {
                    match position
                    {
                        TextPosition::Absolute(p) =>
                        {
                            w.opcode(0x28)?;
                            w.point(*p)?;
                        }
                        TextPosition::Delta { dh,
                                              dv } =>
                        {
                            w.opcode(0x2b)?;
                            w.append(&[*dh, *dv])?;
                        }
                    }
                    w.string(bytes)?;
                }
                Command::Draw { shape,
                                verb } =>
                {
                    let offset = match verb
                    {
                        PictureVerb::Frame => 0,
                        PictureVerb::Paint => 1,
                        PictureVerb::Erase => 2,
                        PictureVerb::Invert => 3,
                        PictureVerb::Fill => 4
                    };
                    match shape
                    {
                        PictureShape::Rect(r)
                        | PictureShape::Oval(r)
                        | PictureShape::RoundRect(r) =>
                        {
                            w.opcode(match shape
                                     {
                                         PictureShape::Rect(_) => 0x30,
                                         PictureShape::RoundRect(_) => 0x40,
                                         _ => 0x50
                                     }
                                     + offset)?;
                            w.rect(*r)?;
                        }
                        PictureShape::Arc { bounds,
                                            start,
                                            sweep } =>
                        {
                            w.opcode(0x60 + offset)?;
                            w.rect(*bounds)?;
                            w.short(*start)?;
                            w.short(*sweep)?;
                        }
                        PictureShape::Polygon(points) =>
                        {
                            w.opcode(0x70 + offset)?;
                            w.word((10usize.checked_add(points.len()
                                                              .checked_mul(4)
                                                              .ok_or(Error::TooLarge)?)
                                           .ok_or(Error::TooLarge)?).try_into()
                                                                    .map_err(|_| {
                                                                        Error::TooLarge
                                                                    })?)?;
                            let bounds = if let Some(first) = points.first()
                            {
                                let mut r = Rect::new(first.h, first.v, first.h, first.v);
                                for p in points
                                {
                                    r.left = r.left.min(p.h);
                                    r.top = r.top.min(p.v);
                                    r.right = r.right.max(p.h);
                                    r.bottom = r.bottom.max(p.v);
                                }
                                r
                            }
                            else
                            {
                                Rect::ZERO
                            };
                            w.rect(bounds)?;
                            for p in points
                            {
                                w.point(*p)?;
                            }
                        }
                        PictureShape::Region(r) =>
                        {
                            w.opcode(0x80 + offset)?;
                            w.region(r)?;
                        }
                    }
                }
                Command::Bitmap(bitmap) =>
                {
                    let direct = matches!(bitmap.depth, 16 | 32);
                    let indexed = !bitmap.palette.is_empty();
                    if bitmap.row_bytes > 0x3fff || bitmap.bounds.is_empty()
                    {
                        return Err(Error::InvalidData);
                    }
                    let height =
                        (i32::from(bitmap.bounds.bottom) - i32::from(bitmap.bounds.top)) as usize;
                    if bitmap.row_bytes.checked_mul(height) != Some(bitmap.data.len())
                    {
                        return Err(Error::InvalidData);
                    }
                    w.opcode(if direct
                             {
                                 if bitmap.mask.is_some() { 0x9b } else { 0x9a }
                             }
                             else if bitmap.mask.is_some()
                             {
                                 0x91
                             }
                             else
                             {
                                 0x90
                             })?;
                    if direct
                    {
                        w.long(0xff)?;
                    }
                    w.word(bitmap.row_bytes as u16 | if direct || indexed { 0x8000 } else { 0 })?;
                    w.rect(bitmap.bounds)?;
                    if direct || indexed
                    {
                        w.word(0)?;
                        w.word(1)?;
                        w.long(0)?;
                        w.long(72 << 16)?;
                        w.long(72 << 16)?;
                        w.word(if direct { 16 } else { 0 })?;
                        w.word(u16::from(bitmap.depth))?;
                        w.word(if direct { 3 } else { 1 })?;
                        w.word(if bitmap.depth == 16
                               {
                                   5
                               }
                               else if direct
                               {
                                   8
                               }
                               else
                               {
                                   u16::from(bitmap.depth)
                               })?;
                        w.long(0)?;
                        w.long(0)?;
                        w.long(0)?;
                        if indexed && !direct
                        {
                            if bitmap.palette.is_empty() || bitmap.palette.len() > 256
                            {
                                return Err(Error::InvalidData);
                            }
                            w.long(0)?;
                            w.word(0)?;
                            w.word((bitmap.palette.len() - 1) as u16)?;
                            for (index, color) in bitmap.palette.iter().enumerate()
                            {
                                w.word(index as u16)?;
                                w.color(*color)?;
                            }
                        }
                    }
                    w.rect(bitmap.source)?;
                    w.rect(bitmap.destination)?;
                    w.short(bitmap.mode)?;
                    if let Some(mask) = &bitmap.mask
                    {
                        w.region(mask)?;
                    }
                    w.append(&bitmap.data)?;
                }
                Command::Comment { kind,
                                   data } =>
                {
                    if data.is_empty()
                    {
                        w.opcode(0xa0)?;
                        w.word(*kind)?;
                    }
                    else
                    {
                        w.opcode(0xa1)?;
                        w.word(*kind)?;
                        w.word(data.len().try_into().map_err(|_| Error::TooLarge)?)?;
                        w.append(data)?;
                    }
                }
            }
            if w.bytes.len() > MAX_BYTES
            {
                return Err(Error::TooLarge);
            }
        }
        w.opcode(0xff)?;
        let size = w.bytes.len() as u16;
        w.bytes[..2].copy_from_slice(&size.to_be_bytes());
        // The decoder is also the recorder's structural validation boundary.
        Self::parse(&w.bytes)?;
        Ok(w.bytes)
    }
}

#[cfg(test)]
mod tests
{
    extern crate std;
    use alloc::vec;

    use mac_formats::resource::Resource;

    use super::*;

    const ORIGINAL_ONE: &[u8] = include_bytes!("../../../tests/fixtures/original-v1.pict");
    const ORIGINAL_TWO: &[u8] = include_bytes!("../../../tests/fixtures/original-v2.pict");

    fn fonts() -> FontManager
    {
        let mut fond = vec![0; 60];
        fond[54..56].copy_from_slice(&12i16.to_be_bytes());
        fond[58..60].copy_from_slice(&12i16.to_be_bytes());
        let mut resources = vec![Resource { kind:       *b"FOND",
                                            id:         0,
                                            name:       Some(b"Chicago".to_vec()),
                                            attributes: 0,
                                            data:       fond },
                                 Resource { kind:       *b"FONT",
                                            id:         12,
                                            name:       None,
                                            attributes: 0,
                                            data:
                                                include_bytes!("assets/chicago-12.font").to_vec() }];
        FontManager::take_resources(&mut resources)
    }
    fn render(picture: &Picture) -> Vec<u32>
    {
        let mut pixels = vec![0xffffff; 220 * 120];
        picture.replay(&mut Canvas::new(&mut pixels, 220, 120),
                       &fonts(),
                       Rect::new(0, 0, 220, 120))
               .unwrap();
        pixels
    }

    #[test]
    fn original_system_v1_and_extended_v2_decode_and_record()
    {
        for (bytes, version) in [(ORIGINAL_ONE, Version::One), (ORIGINAL_TWO, Version::Two)]
        {
            let picture = Picture::parse(bytes).unwrap();
            assert_eq!(picture.version, version);
            assert_eq!(picture.bounds, Rect::new(0, 0, 220, 120));
            assert!(picture.commands.iter().any(|c| matches!(c, Command::Text { .. })));
            picture.validate_supported().unwrap();
            assert_eq!(picture.validate_destination(Rect::ZERO), Err(Error::InvalidHeader));
            assert_eq!(picture.validate_destination(Rect::new(0, 0, 220, 121)),
                       Err(Error::UnsupportedState));
            picture.validate_destination(Rect::new(0, 0, 110, 60)).unwrap();
            let replay = Picture::parse(&picture.encode_v2().unwrap()).unwrap();
            assert_eq!(replay.commands, picture.commands);
            assert_eq!(render(&replay), render(&picture));
        }
    }

    #[test]
    fn every_truncated_original_prefix_is_rejected_without_panicking()
    {
        for original in [ORIGINAL_ONE, ORIGINAL_TWO]
        {
            for end in 0..original.len()
            {
                assert!(Picture::parse(&original[..end]).is_err(), "prefix {end}");
            }
        }
    }

    #[test]
    fn unknown_opcodes_and_unsupported_replay_state_are_explicit()
    {
        let mut bytes = ORIGINAL_TWO.to_vec();
        bytes[40..42].copy_from_slice(&0x1234u16.to_be_bytes());
        assert!(matches!(Picture::parse(&bytes), Err(Error::UnsupportedOpcode(0x1234))));
        let mut picture = Picture::new(Rect::new(0, 0, 4, 4)).unwrap();
        picture.record(Command::Origin(Point { h: 1,
                                               v: 0 }))
               .unwrap();
        assert_eq!(picture.validate_supported(), Err(Error::UnsupportedState));
        let mut pixels = [0x123456; 100];
        let mut canvas = Canvas::new(&mut pixels, 10, 10);
        canvas.set_origin(1, 2);
        canvas.set_clip(Rect::new(1, 1, 4, 4));
        let origin = canvas.origin();
        let clip = canvas.clip();
        assert_eq!(picture.replay(&mut canvas, &FontManager::default(), Rect::new(0, 0, 4, 4)),
                   Err(Error::UnsupportedState));
        assert_eq!(canvas.origin(), origin);
        assert_eq!(canvas.clip(), clip);
    }

    #[test]
    fn packbits_lengths_and_runs_are_checked()
    {
        assert_eq!(unpack_bits(&[1, 2, 3, 254, 9], 5, 1).unwrap(), vec![2, 3, 9, 9, 9]);
        assert_eq!(unpack_bits(&[255, 0x12, 0x34], 4, 2).unwrap(), vec![0x12, 0x34, 0x12, 0x34]);
        assert_eq!(unpack_bits(&[254, 9], 2, 1), Err(Error::InvalidData));
        assert_eq!(unpack_bits(&[2, 9], 3, 1), Err(Error::Truncated));
    }

    #[test]
    fn recorded_monochrome_bitmap_mask_and_odd_comments_align_to_v2_words()
    {
        let mut picture = Picture::new(Rect::new(0, 0, 8, 2)).unwrap();
        picture.record(Command::Comment { kind: 42,
                                          data: vec![1, 2, 3] })
               .unwrap();
        picture.record(Command::Bitmap(PictureBitmap{bounds:Rect::new(0,0,8,2),row_bytes:1,depth:1,data:vec![0xaa,0x55],palette:Vec::new(),source:Rect::new(0,0,8,2),destination:Rect::new(0,0,8,2),mode:0,mask:Some(Region::from_rect(Rect::new(0,0,4,2)))})).unwrap();
        let encoded = picture.encode_v2().unwrap();
        let decoded = Picture::parse(&encoded).unwrap();
        assert_eq!(decoded.commands, picture.commands);
        let mut pixels = [0xffffff; 16];
        decoded.replay(&mut Canvas::new(&mut pixels, 8, 2),
                       &FontManager::default(),
                       decoded.bounds)
               .unwrap();
        assert_eq!(&pixels[..8],
                   &[0, 0xffffff, 0, 0xffffff, 0xffffff, 0xffffff, 0xffffff, 0xffffff]);
    }

    #[test]
    fn compare_original_v1_rgb_pixels_and_retain_exact_difference_count()
    {
        let picture = Picture::parse(ORIGINAL_ONE).unwrap();
        let pixels = render(&picture);
        let golden = include_bytes!("../../../tests/fixtures/original-v1.rgb");
        assert_eq!(golden.len(), pixels.len() * 3);
        let differences = pixels.iter()
                                .zip(golden.as_chunks::<3>().0)
                                .filter(|(p, rgb)| {
                                    let expected = (u32::from(rgb[0]) << 16)
                                                   | (u32::from(rgb[1]) << 8)
                                                   | u32::from(rgb[2]);
                                    **p != expected
                                })
                                .count();
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let directory =
            mac_build::paths::build_dir(&workspace).join("acceptance/quickdraw-reference");
        std::fs::create_dir_all(&directory).unwrap();
        let mut ppm = b"P6\n220 120\n255\n".to_vec();
        for p in &pixels
        {
            ppm.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
        }
        std::fs::write(directory.join("native-picture-v1.ppm"), ppm).unwrap();
        std::fs::write(directory.join("picture-comparison.json"),
                       alloc::format!("{{\"source\":\"original-v1.pict\",\"reference\":\"\
                                       original-v1.rgb\",\"pixels\":{},\"rgb_differences\":{},\"\
                                       exact\":{}}}\n",
                                      pixels.len(),
                                      differences,
                                      differences == 0)).unwrap();
        // Original v1 fixture is a strict RGB pixel conformance gate.
        assert_eq!(differences, 0, "picture RGB regression");
    }
}
