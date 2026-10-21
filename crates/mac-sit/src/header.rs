use crate::{Crc16, Error, Result};
pub const ARCHIVE_HEADER_BYTES: usize = 22;
pub const ENTRY_HEADER_BYTES: usize = 112;
pub const ROOT_ID: u32 = 2;
pub const FIRST_ENTRY_ID: u32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArchiveHeader
{
    /// Top-level files/folders, excluding nested contents and end sentinels.
    pub top_level_count: u16,
    pub length:          u64
}
impl ArchiveHeader
{
    pub fn decode(bytes: &[u8], source_length: u64) -> Result<Self>
    {
        let bytes: &[u8; ARCHIVE_HEADER_BYTES] = bytes.try_into().map_err(|_| Error::Truncated)?;
        if bytes[..4] != *b"SIT!" || bytes[10..14] != *b"rLau"
        {
            return Err(Error::Signature);
        }
        if bytes[14] != 1
        {
            return Err(Error::UnsupportedVersion(bytes[14]));
        }
        let length =
            u64::from(u32::from_be_bytes(bytes[6..10].try_into().map_err(|_| Error::Bounds)?));
        if length < ARCHIVE_HEADER_BYTES as u64 || length > source_length
        {
            return Err(Error::Bounds);
        }
        Ok(Self { top_level_count: u16::from_be_bytes([bytes[4], bytes[5]]),
                  length })
    }
}

/// Archive bytes stay MacRoman. Decode to UTF-8 without allocation; filesystem
/// lookup/normalization policy is separate from decoding the external format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Name
{
    bytes:  [u8; 63],
    length: u8
}
impl Name
{
    fn decode(bytes: &[u8; 64]) -> Result<Self>
    {
        let length = bytes[0];
        if length == 0 || length > 63
        {
            return Err(Error::Name);
        }
        let content = &bytes[1..1 + usize::from(length)];
        if content.contains(&0)
        {
            return Err(Error::Name);
        }
        let mut name = Self { bytes: [0; 63],
                              length };
        name.bytes[..content.len()].copy_from_slice(content);
        Ok(name)
    }

    pub fn as_macroman(&self) -> &[u8]
    {
        &self.bytes[..usize::from(self.length)]
    }

    pub fn utf8<'a>(&self, output: &'a mut [u8]) -> Result<&'a str>
    {
        let mut at = 0;
        for byte in self.as_macroman()
        {
            let ch = mac_text::character(*byte);
            let end = at + ch.len_utf8();
            ch.encode_utf8(output.get_mut(at..end).ok_or(Error::Limit)?);
            at = end;
        }
        core::str::from_utf8(&output[..at]).map_err(|_| Error::Name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method
{
    Stored,
    Rle,
    Lzw,
    Huffman
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fork
{
    pub offset:        u64,
    pub packed_length: u64,
    pub length:        u64,
    pub crc:           u16,
    pub method_byte:   u8
}
impl Fork
{
    /// Encryption/unsupported methods leave the entry's catalog readable.
    pub fn method(self) -> Result<Method>
    {
        if self.method_byte & 0x10 != 0
        {
            return Err(Error::Encrypted);
        }
        match self.method_byte
        {
            0 => Ok(Method::Stored),
            1 => Ok(Method::Rle),
            2 => Ok(Method::Lzw),
            3 => Ok(Method::Huffman),
            other => Err(Error::UnsupportedMethod(other))
        }
    }

    pub fn end(self) -> Result<u64>
    {
        self.offset.checked_add(self.packed_length).ok_or(Error::Bounds)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileHeader
{
    pub name:             Name,
    pub file_type:        [u8; 4],
    pub creator:          [u8; 4],
    pub finder_flags:     u16,
    pub created_seconds:  u32,
    pub modified_seconds: u32,
    pub resource:         Fork,
    pub data:             Fork
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FolderHeader
{
    pub name:             Name,
    pub finder_flags:     u16,
    pub created_seconds:  u32,
    pub modified_seconds: u32
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryHeader
{
    File(FileHeader),
    Folder(FolderHeader),
    EndFolder
}
fn word(bytes: &[u8; ENTRY_HEADER_BYTES], at: usize) -> u16
{
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}
fn long(bytes: &[u8; ENTRY_HEADER_BYTES], at: usize) -> u32
{
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
impl EntryHeader
{
    pub fn decode(bytes: &[u8], offset: u64, archive_length: u64) -> Result<Self>
    {
        let bytes: &[u8; ENTRY_HEADER_BYTES] = bytes.try_into().map_err(|_| Error::Truncated)?;
        let payload = offset.checked_add(ENTRY_HEADER_BYTES as u64)
                            .filter(|end| *end <= archive_length)
                            .ok_or(Error::Bounds)?;
        if Crc16::checksum(&bytes[..110]) != word(bytes, 110)
        {
            return Err(Error::HeaderChecksum);
        }
        // Only the data-method sentinel is specified for endFolder. Every other
        // field may be invalid, including its Pascal-name length and sizes.
        if bytes[1] == 33
        {
            return Ok(Self::EndFolder);
        }
        let name = Name::decode(bytes[2..66].try_into().map_err(|_| Error::Name)?)?;
        if bytes[0] == 32 && bytes[1] == 32
        {
            return Ok(Self::Folder(FolderHeader { name,
                                                  finder_flags: word(bytes, 74),
                                                  created_seconds: long(bytes, 76),
                                                  modified_seconds: long(bytes, 80) }));
        }
        let resource = Fork { offset:        payload,
                              packed_length: u64::from(long(bytes, 92)),
                              length:        u64::from(long(bytes, 84)),
                              crc:           word(bytes, 100),
                              method_byte:   bytes[0] };
        let data = Fork { offset:        resource.end()?,
                          packed_length: u64::from(long(bytes, 96)),
                          length:        u64::from(long(bytes, 88)),
                          crc:           word(bytes, 102),
                          method_byte:   bytes[1] };
        if data.end()? > archive_length
        {
            return Err(Error::Bounds);
        }
        Ok(Self::File(FileHeader { name,
                                   file_type: bytes[66..70].try_into()
                                                           .map_err(|_| Error::Bounds)?,
                                   creator: bytes[70..74].try_into()
                                                         .map_err(|_| Error::Bounds)?,
                                   finder_flags: word(bytes, 74),
                                   created_seconds: long(bytes, 76),
                                   modified_seconds: long(bytes, 80),
                                   resource,
                                   data }))
    }
}

pub fn mac_microseconds(seconds: u32) -> Result<u64>
{
    u64::from(seconds).checked_mul(1_000_000).ok_or(Error::Bounds)
}
