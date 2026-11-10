//! Allocation-free System 7 MacRoman and checked Pascal-string conversion.
#![no_std]
#![forbid(unsafe_code)]

pub const HIGH: [char; 128] =
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

/// A conversion refusal. Positions name the zero-based Unicode scalar index
/// and UTF-8 byte offset in the original input. Conversion never substitutes characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Unrepresentable
    {
        character:       char,
        character_index: usize,
        byte_offset:     usize
    },
    MissingLength,
    LengthOverrun
    {
        declared:  usize,
        available: usize
    },
    TrailingBytes
    {
        used:      usize,
        available: usize
    },
    TooLong
    {
        capacity: usize,
        required: usize
    },
    OutputTooSmall
    {
        required:  usize,
        available: usize
    },
    LengthOverflow
}
impl core::fmt::Display for Error
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Unrepresentable { character,
                                    character_index,
                                    byte_offset } =>
            {
                write!(f,
                       "character {character:?} (U+{:04X}) at character {character_index}, UTF-8 \
                        byte {byte_offset}, has no System 7 MacRoman representation",
                       *character as u32)
            }
            Self::MissingLength => f.write_str("Pascal string is missing its length byte"),
            Self::LengthOverrun { declared,
                                  available } =>
            {
                write!(f,
                       "Pascal string declares {declared} bytes but only {available} payload \
                        bytes are available")
            }
            Self::TrailingBytes { used,
                                  available } =>
            {
                write!(f,
                       "Pascal string uses {used} bytes but its exact input contains {available}")
            }
            Self::TooLong { capacity,
                            required } =>
            {
                write!(f, "Pascal string needs {required} characters; capacity is {capacity}")
            }
            Self::OutputTooSmall { required,
                                   available } =>
            {
                write!(f,
                       "conversion needs {required} output bytes; only {available} are available")
            }
            Self::LengthOverflow =>
            {
                f.write_str("decoded UTF-8 length exceeds the addressable buffer size")
            }
        }
    }
}
impl core::error::Error for Error
{
}

/// Decode one System 7 MacRoman byte. Every byte, including control bytes, has
/// one Unicode scalar. Byte DB is the pre-8.5 currency sign, not the euro sign.
pub fn character(value: u8) -> char
{
    if value < 128 { value as char } else { HIGH[value as usize - 128] }
}

/// The exact inverse scalar mapping; no normalization or substitution.
pub fn byte(character: char) -> Option<u8>
{
    if character.is_ascii()
    {
        Some(character as u8)
    }
    else
    {
        HIGH.iter().position(|&value| value == character).map(|index| index as u8 + 128)
    }
}

/// Decode without allocating. A caller may collect these scalars into an owned
/// string, or write them through its own formatter.
pub fn characters(bytes: &[u8]) -> impl ExactSizeIterator<Item = char> + DoubleEndedIterator + '_
{
    bytes.iter().map(|&value| character(value))
}

pub fn utf8_length(bytes: &[u8]) -> Result<usize, Error>
{
    characters(bytes).try_fold(0usize, |length, character| {
                         length.checked_add(character.len_utf8()).ok_or(Error::LengthOverflow)
                     })
}

/// Write UTF-8 into caller-owned storage. Ordinary refusals preserve the entire
/// output buffer; successful conversion writes only the returned prefix.
pub fn decode_into(bytes: &[u8], output: &mut [u8]) -> Result<usize, Error>
{
    let required = utf8_length(bytes)?;
    if output.len() < required
    {
        return Err(Error::OutputTooSmall { required,
                                           available: output.len() });
    }
    let mut offset = 0;
    for character in characters(bytes)
    {
        let end = offset + character.len_utf8();
        character.encode_utf8(&mut output[offset..end]);
        offset = end;
    }
    Ok(offset)
}

/// A fully validated immutable UTF-8 borrow and its MacRoman encoded length.
/// Validation precedes output publication and any caller's optional allocation.
#[derive(Clone, Copy, Debug)]
pub struct MacRomanText<'a>
{
    text:   &'a str,
    length: usize
}
impl<'a> MacRomanText<'a>
{
    pub fn new(text: &'a str) -> Result<Self, Error>
    {
        let mut length = 0;
        for (character_index, (byte_offset, character)) in text.char_indices().enumerate()
        {
            byte(character).ok_or(Error::Unrepresentable { character,
                                                           character_index,
                                                           byte_offset })?;
            length += 1;
        }
        Ok(Self { text,
                  length })
    }

    pub const fn len(self) -> usize
    {
        self.length
    }

    pub const fn is_empty(self) -> bool
    {
        self.length == 0
    }

    pub fn write_into(self, output: &mut [u8]) -> Result<usize, Error>
    {
        if output.len() < self.length
        {
            return Err(Error::OutputTooSmall { required:  self.length,
                                               available: output.len() });
        }
        // The held immutable borrow and the immutable mapping preserve the
        // complete validation performed by new; no input can change mid-write.
        for (index, (offset, character)) in self.text.char_indices().enumerate()
        {
            output[index] = byte(character).ok_or(Error::Unrepresentable { character,
                                                                           character_index:
                                                                               index,
                                                                           byte_offset: offset })?;
        }
        Ok(self.length)
    }

    /// Validate capacity and return the required storage, including the prefix.
    pub fn pascal_length(self, capacity: u8) -> Result<usize, Error>
    {
        if self.length > usize::from(capacity)
        {
            return Err(Error::TooLong { capacity: usize::from(capacity),
                                        required: self.length });
        }
        Ok(self.length + 1)
    }

    /// Capacity counts MacRoman payload bytes (one per representable scalar).
    /// Str255, Str63, Str31 and Str27 use capacities 255, 63, 31 and 27.
    pub fn write_pascal(self, capacity: u8, output: &mut [u8]) -> Result<usize, Error>
    {
        let required = self.pascal_length(capacity)?;
        if output.len() < required
        {
            return Err(Error::OutputTooSmall { required,
                                               available: output.len() });
        }
        self.write_into(&mut output[1..required])?;
        output[0] = self.length as u8;
        Ok(required)
    }
}

pub fn encode_into(text: &str, output: &mut [u8]) -> Result<usize, Error>
{
    MacRomanText::new(text)?.write_into(output)
}
pub fn encode_pascal(text: &str, capacity: u8, output: &mut [u8]) -> Result<usize, Error>
{
    MacRomanText::new(text)?.write_pascal(capacity, output)
}

/// Checked Pascal-string payload. Exact encoded values and padded storage have
/// different constructors so trailing data is never silently accepted by an
/// exact-value reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PascalStr<'a>
{
    payload: &'a [u8]
}
impl<'a> PascalStr<'a>
{
    /// Read one string from storage that may have unused bytes after it.
    /// Capacity is the maximum permitted payload length, not padding to consume.
    pub fn from_buffer(buffer: &'a [u8], capacity: u8) -> Result<Self, Error>
    {
        let length = usize::from(*buffer.first().ok_or(Error::MissingLength)?);
        if length > usize::from(capacity)
        {
            return Err(Error::TooLong { capacity: usize::from(capacity),
                                        required: length });
        }
        let payload = buffer.get(1..1 + length)
                            .ok_or(Error::LengthOverrun { declared:  length,
                                                          available: buffer.len() - 1 })?;
        Ok(Self { payload })
    }

    /// Read exactly one encoded value, rejecting extra bytes after its payload.
    pub fn from_exact(buffer: &'a [u8], capacity: u8) -> Result<Self, Error>
    {
        let value = Self::from_buffer(buffer, capacity)?;
        let used = value.payload.len() + 1;
        if used != buffer.len()
        {
            return Err(Error::TrailingBytes { used,
                                              available: buffer.len() });
        }
        Ok(value)
    }

    pub const fn payload(self) -> &'a [u8]
    {
        self.payload
    }

    pub fn characters(self) -> impl ExactSizeIterator<Item = char> + DoubleEndedIterator + 'a
    {
        characters(self.payload)
    }

    pub fn decode_into(self, output: &mut [u8]) -> Result<usize, Error>
    {
        decode_into(self.payload, output)
    }
}

/// Project a native filename to a classic Str31 view. Validate the ENTIRE name,
/// including any suffix beyond the projected prefix, and the native 255-scalar
/// limit before writing. This view never changes the input or stored filename.
/// This explicit projection is separate from Pascal builders, which never
/// truncate. The returned count includes the prefix byte.
pub fn project_filename_31(text: &str, output: &mut [u8]) -> Result<usize, Error>
{
    let validated = MacRomanText::new(text)?;
    if validated.len() > 255
    {
        return Err(Error::TooLong { capacity: 255,
                                    required: validated.len() });
    }
    let length = validated.len().min(31);
    let required = length + 1;
    if output.len() < required
    {
        return Err(Error::OutputTooSmall { required,
                                           available: output.len() });
    }
    for (index, (offset, character)) in text.char_indices().take(length).enumerate()
    {
        output[index + 1] = byte(character).ok_or(Error::Unrepresentable { character,
                                                                           character_index:
                                                                               index,
                                                                           byte_offset: offset })?;
    }
    output[0] = length as u8;
    Ok(required)
}
