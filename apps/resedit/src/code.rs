//! Allocation-free ELF64 executable indexing. Only PT_LOAD + PF_X bytes are code.
#![forbid(unsafe_code)]
use core::fmt::Write;

use mac_toolbox::{BAD_ARGUMENT, Error, IO_ERROR, ResourceKey, ToolboxResult,
                  resources::ResourceFile};

use super::document::Text;

const MAX_SEGMENTS: usize = 256;
const MAX_MARKS: usize = 512;
#[derive(Clone, Copy, Default)]
struct Segment
{
    offset:  usize,
    length:  usize,
    address: u64
}
#[derive(Clone, Copy, Default)]
struct Mark
{
    row:     u32,
    segment: usize,
    offset:  usize
}
pub struct Index
{
    segments:  [Segment; MAX_SEGMENTS],
    count:     usize,
    marks:     [Mark; MAX_MARKS],
    marked:    usize,
    pub rows:  u32,
    pub entry: u64
}
fn bad() -> Error
{
    Error(BAD_ARGUMENT)
}
fn read_exact(read: &mut impl FnMut(usize, &mut [u8]) -> ToolboxResult<usize>, at: usize,
              out: &mut [u8])
              -> ToolboxResult<()>
{
    if read(at, out)? != out.len()
    {
        return Err(Error(IO_ERROR));
    }
    Ok(())
}
fn le16(b: &[u8], at: usize) -> u16
{
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn le32(b: &[u8], at: usize) -> u32
{
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn le64(b: &[u8], at: usize) -> u64
{
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
struct Stream<F>
{
    read:  F,
    bytes: [u8; 4096],
    start: usize,
    used:  usize
}
impl<F: FnMut(usize, &mut [u8]) -> ToolboxResult<usize>> Stream<F>
{
    fn new(read: F) -> Self
    {
        Self { read,
               bytes: [0; 4096],
               start: 0,
               used: 0 }
    }

    fn instruction(&mut self, at: usize, end: usize) -> ToolboxResult<([u8; 8], usize)>
    {
        let need = (end - at).min(8);
        if at < self.start
           || at - self.start > self.used
           || need > self.used.saturating_sub(at - self.start)
        {
            self.start = at;
            self.used = (end - at).min(self.bytes.len());
            read_exact(&mut self.read, at, &mut self.bytes[..self.used])?;
        }
        let mut bytes = [0; 8];
        bytes[..need].copy_from_slice(&self.bytes[at - self.start..at - self.start + need]);
        let length = if need < 2
        {
            1
        }
        else
        {
            disc_v::inst_length(u16::from_le_bytes([bytes[0], bytes[1]])).unwrap_or(2).min(need)
        };
        Ok((bytes, length))
    }
}
impl Index
{
    pub fn build(length: usize, mut read: impl FnMut(usize, &mut [u8]) -> ToolboxResult<usize>)
                 -> ToolboxResult<Self>
    {
        if length < 64
        {
            return Err(bad());
        }
        let mut header = [0; 64];
        read_exact(&mut read, 0, &mut header)?;
        if &header[..7] != b"\x7fELF\x02\x01\x01"
           || le16(&header, 16) != 2
           || le16(&header, 18) != 243
           || le32(&header, 20) != 1
           || le16(&header, 52) != 64
           || le16(&header, 54) != 56
        {
            return Err(bad());
        }
        let phoff = usize::try_from(le64(&header, 32)).map_err(|_| bad())?;
        let count = usize::from(le16(&header, 56));
        if count == 0
           || count > MAX_SEGMENTS
           || phoff.checked_add(count * 56).is_none_or(|n| n > length)
        {
            return Err(bad());
        }
        let mut index = Self { segments: [Segment::default(); MAX_SEGMENTS],
                               count:    0,
                               marks:    [Mark::default(); MAX_MARKS],
                               marked:   0,
                               rows:     0,
                               entry:    le64(&header, 24) };
        let mut total = 0usize;
        for i in 0..count
        {
            let mut ph = [0; 56];
            read_exact(&mut read, phoff + i * 56, &mut ph)?;
            if le32(&ph, 0) != 1 || le32(&ph, 4) & 1 == 0
            {
                continue;
            }
            let offset = usize::try_from(le64(&ph, 8)).map_err(|_| bad())?;
            let size = usize::try_from(le64(&ph, 32)).map_err(|_| bad())?;
            let address = le64(&ph, 16);
            if size as u64 > le64(&ph, 40)
               || offset.checked_add(size).is_none_or(|n| n > length)
               || address.checked_add(size as u64).is_none()
            {
                return Err(bad());
            }
            if size == 0
            {
                continue;
            }
            for s in &index.segments[..index.count]
            {
                if address < s.address + s.length as u64 && s.address < address + size as u64
                {
                    return Err(bad());
                }
            }
            index.segments[index.count] = Segment { offset,
                                                    length: size,
                                                    address };
            index.count += 1;
            total = total.checked_add(size).ok_or_else(bad)?;
        }
        if index.count == 0
           || !index.segments[..index.count].iter().any(|s| {
                                                       index.entry >= s.address
                                                       && index.entry < s.address + s.length as u64
                                                   })
        {
            return Err(bad());
        }
        // One checkpoint per segment, with the remaining budget spread over bytes.
        let stride = total.div_ceil(MAX_MARKS - index.count).max(1);
        let mut stream = Stream::new(read);
        let mut traversed = 0;
        let mut next = stride;
        for s in 0..index.count
        {
            let segment = &mut index.segments[s];
            let mut offset = 0;
            while offset < segment.length
            {
                if offset == 0 || traversed >= next
                {
                    if index.marked < MAX_MARKS
                    {
                        index.marks[index.marked] = Mark { row: index.rows,
                                                           segment: s,
                                                           offset };
                        index.marked += 1;
                    }
                    if traversed >= next
                    {
                        next = (traversed / stride + 1) * stride;
                    }
                }
                let (_, n) =
                    stream.instruction(segment.offset + offset, segment.offset + segment.length)?;
                offset += n;
                traversed += n;
                index.rows = index.rows.checked_add(1).ok_or_else(bad)?;
            }
        }
        Ok(index)
    }

    pub fn lines(&self, top: u32, rows: u32,
                 read: impl FnMut(usize, &mut [u8]) -> ToolboxResult<usize>,
                 mut emit: impl FnMut(&str))
                 -> ToolboxResult<()>
    {
        let Some(mark) = self.marks[..self.marked].iter().rev().find(|m| m.row <= top)
        else
        {
            return Ok(());
        };
        let mut segment = mark.segment;
        let mut offset = mark.offset;
        let mut row = mark.row;
        let mut stream = Stream::new(read);
        while segment < self.count && row < top.saturating_add(rows)
        {
            let s = self.segments[segment];
            if offset == s.length
            {
                segment += 1;
                offset = 0;
                continue;
            }
            let (bytes, n) = stream.instruction(s.offset + offset, s.offset + s.length)?;
            if row >= top
            {
                let address = s.address + offset as u64;
                let mut text = Text::<192>::new();
                let _ = write!(text, "{address:08x} ");
                if let Some(ins) = disc_v::decode_bytes(disc_v::Isa::Rv64, address, &bytes[..n])
                {
                    let _ = write!(text, "{ins}");
                }
                else
                {
                    let _ = write!(text, ".byte");
                    for b in &bytes[..n]
                    {
                        let _ = write!(text, " {b:02x}");
                    }
                }
                emit(text.as_str());
            }
            row += 1;
            offset += n;
        }
        Ok(())
    }
}
/// Loader metadata stays visible above the native instructions; full relocation
/// records also have their own structured resource viewer.
pub const HEADER_ROWS: u32 = 7;
pub fn loader(file: &ResourceFile<'_>, entry: u64, mut emit: impl FnMut(&str))
{
    emit("RV64 executable (linked addresses)");
    let mut line = Text::<128>::new();
    let _ = write!(line, "Entry: 0x{entry:08x}");
    emit(line.as_str());
    let mut data = [0; 32];
    let descriptor = file.read(ResourceKey { kind: *b"NABI",
                                             id:   0 },
                               0,
                               &mut data);
    if descriptor == Ok(32) && &data[..4] == b"RVN1"
    {
        let mut line = Text::<128>::new();
        let _ = write!(line,
                       "NABI: format {} role {} ABI {}",
                       u16::from_be_bytes([data[4], data[5]]),
                       u16::from_be_bytes([data[6], data[7]]),
                       u32::from_be_bytes(data[8..12].try_into().unwrap()));
        emit(line.as_str());
        for (label, at) in [("Load base", 16), ("Load limit", 24)]
        {
            let mut line = Text::<128>::new();
            let _ = write!(line,
                           "{label}: 0x{:08x}",
                           u64::from_be_bytes(data[at..at + 8].try_into().unwrap()));
            emit(line.as_str());
        }
    }
    else
    {
        emit("NABI: absent or malformed");
        emit("");
        emit("");
    }
    let key = ResourceKey { kind: *b"RLOC",
                            id:   0 };
    match file.get(key)
    {
        Ok(info) =>
        {
            match relocation_count(info.info.length, |at, out| file.read(key, at, out))
            {
                Ok(count) =>
                {
                    let mut line = Text::<128>::new();
                    let _ = write!(line, "RLOC: {} patches (select RLOC to inspect)", count - 1);
                    emit(line.as_str());
                }
                Err(_) => emit("RLOC: malformed relocation table")
            }
        }
        Err(error) if error.0 == mac_toolbox::NOT_FOUND =>
        {
            emit("RLOC: absent (fixed-address image)")
        }
        Err(_) => emit("RLOC: unavailable")
    }
    emit("");
}

pub fn relocation_count(length: usize,
                        mut read: impl FnMut(usize, &mut [u8]) -> ToolboxResult<usize>)
                        -> ToolboxResult<u32>
{
    let mut header = [0; 8];
    read_exact(&mut read, 0, &mut header)?;
    let count = le32(&header, 4);
    if &header[..4] != b"RLC1" || count > 65536 || length != 8 + count as usize * 8
    {
        return Err(bad());
    }
    Ok(count + 1)
}
pub fn relocations(length: usize, top: u32, rows: u32,
                   mut read: impl FnMut(usize, &mut [u8]) -> ToolboxResult<usize>,
                   mut emit: impl FnMut(&str))
                   -> ToolboxResult<()>
{
    let count = relocation_count(length, &mut read)?;
    for row in top..top.saturating_add(rows).min(count)
    {
        let mut line = Text::<128>::new();
        if row == 0
        {
            let _ = write!(line, "RLOC: {} absolute pointer patches", count - 1);
        }
        else
        {
            let mut record = [0; 8];
            read_exact(&mut read, 8 + (row - 1) as usize * 8, &mut record)?;
            if !matches!(record[4], 4 | 8) || record[5..] != [0; 3]
            {
                return Err(bad());
            }
            let _ = write!(line, "+{:08x}  {}-bit pointer", le32(&record, 0), record[4] * 8);
        }
        emit(line.as_str());
    }
    Ok(())
}
