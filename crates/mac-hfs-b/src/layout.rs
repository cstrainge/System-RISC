//! Version-one disk codecs. All integers are big endian; no native layouts leak.
use crate::{Error, Result, names};
/// One whole-number disk contract, including its pinned Unicode tables.
pub const VERSION: u16 = 1;
pub const BLOCK_SIZE: usize = 4096;
pub const SECTORS_PER_BLOCK: u64 = 8;
pub const ROOT_ID: u32 = 2;
pub const DIRTY: u32 = 1;
pub const FORMATTING: u32 = 2;
pub(crate) fn u16_at(b: &[u8], at: usize) -> Result<u16>
{
    Ok(u16::from_be_bytes(b.get(at..at + 2).ok_or(Error::Corrupt)?.try_into().unwrap()))
}
pub(crate) fn u32_at(b: &[u8], at: usize) -> Result<u32>
{
    Ok(u32::from_be_bytes(b.get(at..at + 4).ok_or(Error::Corrupt)?.try_into().unwrap()))
}
pub(crate) fn u64_at(b: &[u8], at: usize) -> Result<u64>
{
    Ok(u64::from_be_bytes(b.get(at..at + 8).ok_or(Error::Corrupt)?.try_into().unwrap()))
}
pub(crate) fn put16(b: &mut [u8], at: usize, n: u16)
{
    b[at..at + 2].copy_from_slice(&n.to_be_bytes());
}
pub(crate) fn put32(b: &mut [u8], at: usize, n: u32)
{
    b[at..at + 4].copy_from_slice(&n.to_be_bytes());
}
pub(crate) fn put64(b: &mut [u8], at: usize, n: u64)
{
    b[at..at + 8].copy_from_slice(&n.to_be_bytes());
}
pub(crate) fn crc32(bytes: &[u8]) -> u32
{
    let mut crc = !0u32;
    for &b in bytes
    {
        crc ^= u32::from(b);
        for _ in 0..8
        {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header
{
    pub sectors:       u64,
    pub blocks:        u32,
    pub free:          u32,
    pub bitmap_blocks: u32,
    pub roots:         [u32; 3],
    pub next_id:       u32,
    pub blessed:       u32,
    pub hint:          u32,
    pub created:       u64,
    pub modified:      u64,
    pub revision:      u64,
    pub uuid:          [u8; 16],
    pub name:          alloc::string::String,
    pub finder_info:   [u8; 32],
    pub flags:         u32
}
impl Header
{
    pub fn encode(&self) -> Result<[u8; 512]>
    {
        self.validate()?;
        let mut b = [0; 512];
        b[..2].copy_from_slice(b"HB");
        put16(&mut b, 2, VERSION);
        put32(&mut b, 4, BLOCK_SIZE as u32);
        put32(&mut b, 8, names::UNICODE_VERSION);
        put32(&mut b, 12, self.flags);
        put64(&mut b, 16, self.sectors);
        put32(&mut b, 24, self.blocks);
        put32(&mut b, 28, self.free);
        put32(&mut b, 32, 1);
        put32(&mut b, 36, self.bitmap_blocks);
        for (i, &root) in self.roots.iter().enumerate()
        {
            put32(&mut b, 40 + i * 4, root);
        }
        put32(&mut b, 52, self.next_id);
        put32(&mut b, 56, self.blessed);
        put32(&mut b, 60, self.hint);
        put64(&mut b, 64, self.created);
        put64(&mut b, 72, self.modified);
        put64(&mut b, 80, self.revision);
        b[88..104].copy_from_slice(&self.uuid);
        put16(&mut b, 104, self.name.len() as u16);
        b[128..128 + self.name.len()].copy_from_slice(self.name.as_bytes());
        b[384..416].copy_from_slice(&self.finder_info);
        let checksum = crc32(&b[..508]);
        put32(&mut b, 508, checksum);
        Ok(b)
    }

    pub fn decode(b: &[u8; 512]) -> Result<Self>
    {
        if &b[..2] != b"HB"
           || u16_at(b, 2)? != VERSION
           || u32_at(b, 4)? != BLOCK_SIZE as u32
           || u32_at(b, 8)? != names::UNICODE_VERSION
        {
            return Err(Error::Incompatible);
        }
        if crc32(&b[..508]) != u32_at(b, 508)?
           || u32_at(b, 32)? != 1
           || b[106..128].iter().chain(&b[416..508]).any(|&b| b != 0)
        {
            return Err(Error::Corrupt);
        }
        let n = u16_at(b, 104)? as usize;
        if n > 255 || b[128 + n..384].iter().any(|&b| b != 0)
        {
            return Err(Error::Corrupt);
        }
        let name = core::str::from_utf8(&b[128..128 + n]).map_err(|_| Error::Corrupt)?;
        let h = Self { sectors:       u64_at(b, 16)?,
                       blocks:        u32_at(b, 24)?,
                       free:          u32_at(b, 28)?,
                       bitmap_blocks: u32_at(b, 36)?,
                       roots:         [u32_at(b, 40)?, u32_at(b, 44)?, u32_at(b, 48)?],
                       next_id:       u32_at(b, 52)?,
                       blessed:       u32_at(b, 56)?,
                       hint:          u32_at(b, 60)?,
                       created:       u64_at(b, 64)?,
                       modified:      u64_at(b, 72)?,
                       revision:      u64_at(b, 80)?,
                       uuid:          b[88..104].try_into().unwrap(),
                       name:          names::normalize(name)?,
                       finder_info:   b[384..416].try_into().unwrap(),
                       flags:         u32_at(b, 12)? };
        if h.name != name
        {
            return Err(Error::Corrupt);
        }
        h.validate()?;
        Ok(h)
    }

    fn validate(&self) -> Result<()>
    {
        if self.blocks < 16
           || self.sectors != u64::from(self.blocks) * 8
           || self.bitmap_blocks != self.blocks.div_ceil(32768)
           || self.free > self.blocks
           || self.next_id < 16
           || self.revision == 0
           || self.flags & !(DIRTY | FORMATTING) != 0
           || self.name.len() > 255
           || self.name.is_empty()
           || self.roots.iter().any(|&b| !self.valid_data_block(b))
           || !self.valid_data_block(self.hint)
           || self.roots[0] == self.roots[1]
           || self.roots[0] == self.roots[2]
           || self.roots[1] == self.roots[2]
        {
            return Err(Error::Corrupt);
        }
        Ok(())
    }

    pub(crate) fn valid_data_block(&self, b: u32) -> bool
    {
        b > self.bitmap_blocks && b < self.blocks - 1
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Extent
{
    pub start: u32,
    pub count: u32
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fork
{
    pub length:  u64,
    pub blocks:  u64,
    pub extents: [Extent; 8]
}
impl Fork
{
    pub(crate) fn encode(&self, b: &mut [u8])
    {
        put64(b, 0, self.length);
        put64(b, 8, self.blocks);
        for (i, e) in self.extents.iter().enumerate()
        {
            put32(b, 16 + i * 8, e.start);
            put32(b, 20 + i * 8, e.count);
        }
    }

    pub(crate) fn decode(b: &[u8], h: &Header) -> Result<Self>
    {
        let mut f = Self { length: u64_at(b, 0)?,
                           blocks: u64_at(b, 8)?,
                           ..Self::default() };
        if f.blocks < f.length.div_ceil(BLOCK_SIZE as u64) || f.blocks > u64::from(h.blocks)
        {
            return Err(Error::Corrupt);
        }
        let mut empty = false;
        let mut inline = 0u64;
        for (i, e) in f.extents.iter_mut().enumerate()
        {
            *e = Extent { start: u32_at(b, 16 + i * 8)?,
                          count: u32_at(b, 20 + i * 8)? };
            if e.count == 0
            {
                if e.start != 0
                {
                    return Err(Error::Corrupt);
                }
                empty = true;
            }
            else if empty
                      || !h.valid_data_block(e.start)
                      || e.start.checked_add(e.count).is_none_or(|end| end > h.blocks - 1)
            {
                return Err(Error::Corrupt);
            }
            inline += u64::from(e.count);
        }
        if inline > f.blocks || (empty && inline != f.blocks)
        {
            return Err(Error::Corrupt);
        }
        Ok(f)
    }
}
