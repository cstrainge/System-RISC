//! Allocation-free native segment indexing; only executable segment bytes are code.
#![forbid(unsafe_code)]
use core::fmt::Write;

use mac_code_resource::{DESCRIPTOR_SIZE, Descriptor, image};
use mac_toolbox::{Error, ResourceKey, ToolboxResult, resources::ResourceFile};

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
    segments: [Segment; MAX_SEGMENTS],
    count:    usize,
    marks:    [Mark; MAX_MARKS],
    marked:   usize,
    pub rows: u32
}
fn bad() -> Error
{
    Error::BadArgument
}
fn read_exact(read: &mut impl FnMut(usize, &mut [u8]) -> ToolboxResult<usize>, at: usize,
              out: &mut [u8])
              -> ToolboxResult<()>
{
    if read(at, out)? != out.len()
    {
        return Err(Error::Io);
    }
    Ok(())
}
fn le32(b: &[u8], at: usize) -> u32
{
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
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
    pub fn build(length: usize, descriptor: &Descriptor,
                 mut read: impl FnMut(usize, &mut [u8]) -> ToolboxResult<usize>)
                 -> ToolboxResult<Self>
    {
        let mut index = Self { segments: [Segment::default(); MAX_SEGMENTS],
                               count:    0,
                               marks:    [Mark::default(); MAX_MARKS],
                               marked:   0,
                               rows:     0 };
        let mut total = 0usize;
        let end = image::inspect(length,
                                 descriptor,
                                 |at, out| {
                                     read_exact(&mut read, at, out).map_err(|_| {
                                         mac_code_resource::Error("code resource read failed")
                                     })
                                 },
                                 |s| {
                                     if s.flags & image::EXECUTE != 0
                                     {
                                         index.segments[index.count] =
                                             Segment { offset:  s.data_offset,
                                                       length:  s.length,
                                                       address: descriptor.load_base + s.offset };
                                         index.count += 1;
                                         total += s.length;
                                     }
                                 }).map_err(|_| bad())?;
        if end != length
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
#[derive(Clone, Copy)]
pub struct Source
{
    pub descriptor:  Descriptor,
    pub key:         ResourceKey,
    pub offset:      usize,
    pub length:      usize,
    pub relocations: Option<(usize, usize)>
}
impl Source
{
    pub fn open(file: &ResourceFile<'_>, key: ResourceKey, length: usize) -> ToolboxResult<Self>
    {
        let mut data = [0; DESCRIPTOR_SIZE];
        let embedded = key.kind != *b"RV64";
        let abi = if embedded
        {
            key
        }
        else
        {
            ResourceKey { kind: *b"NABI",
                          id:   0 }
        };
        if !embedded && file.get(abi)?.info.length != DESCRIPTOR_SIZE
        {
            return Err(bad());
        }
        if file.read(abi, 0, &mut data)? != data.len()
        {
            return Err(bad());
        }
        let descriptor = Descriptor::decode(&data).map_err(|_| bad())?;
        if embedded && descriptor.role != mac_code_resource::Role::CodeResource
        {
            return Err(bad());
        }
        let offset = if embedded { DESCRIPTOR_SIZE } else { 0 };
        let available = length.checked_sub(offset).ok_or_else(bad)?;
        let end =
            image::inspect(available,
                           &descriptor,
                           |at, out| {
                               match file.read(key, offset + at, out)
                               {
                                   Ok(size) if size == out.len() => Ok(()),
                                   _ => Err(mac_code_resource::Error("code resource read failed"))
                               }
                           },
                           |_| {}).map_err(|_| bad())?;
        if !embedded && end != length
        {
            return Err(bad());
        }
        let relocations = embedded.then_some((offset + end, available - end));
        let source = Self { descriptor,
                            key,
                            offset,
                            length: end,
                            relocations };
        if embedded
        {
            source.relocation_rows(file)?;
        }
        Ok(source)
    }

    pub fn read(&self, file: &ResourceFile<'_>, at: usize, out: &mut [u8]) -> ToolboxResult<usize>
    {
        if at > self.length
        {
            return Err(bad());
        }
        let size = out.len().min(self.length - at);
        file.read(self.key, self.offset + at, &mut out[..size])
    }

    fn relocation_rows(&self, file: &ResourceFile<'_>) -> ToolboxResult<u32>
    {
        if let Some((at, length)) = self.relocations
        {
            relocation_count(length, |offset, out| file.read(self.key, at + offset, out))
        }
        else
        {
            let key = ResourceKey { kind: *b"RLOC",
                                    id:   0 };
            relocation_count(file.get(key)?.info.length, |offset, out| file.read(key, offset, out))
        }
    }
}
pub fn loader(file: &ResourceFile<'_>, source: &Source, mut emit: impl FnMut(&str))
{
    let d = source.descriptor;
    emit("RV64 executable (linked addresses)");
    let mut line = Text::<128>::new();
    let _ = write!(line, "Entry: 0x{:08x}", d.entry);
    emit(line.as_str());
    let mut line = Text::<128>::new();
    let _ = write!(line,
                   "NABI: format {} role {} ABI {}",
                   mac_code_resource::FORMAT_VERSION,
                   d.role as u16,
                   d.abi_version);
    emit(line.as_str());
    for (label, value) in [("Load base", d.load_base), ("Load limit", d.load_limit)]
    {
        let mut line = Text::<128>::new();
        let _ = write!(line, "{label}: 0x{value:08x}");
        emit(line.as_str());
    }
    match source.relocation_rows(file)
    {
        Ok(count) =>
        {
            let mut line = Text::<128>::new();
            let _ = write!(line, "RLOC: {} patches", count - 1);
            emit(line.as_str());
        }
        Err(mac_toolbox::Error::NotFound) => emit("RLOC: absent (fixed-address image)"),
        Err(_) => emit("RLOC: malformed relocation table")
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
