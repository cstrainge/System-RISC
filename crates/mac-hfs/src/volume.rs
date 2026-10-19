pub mod mutation;
pub mod scan;
use alloc::{vec, vec::Vec};

use crate::{BlockDevice, Error, Partition, Result, be16, be32, bytes, read_at};

#[derive(Clone, Copy, Debug)]
struct Extent
{
    start: u16,
    count: u16
}

#[derive(Clone, Debug)]
pub struct Fork
{
    id:         u32,
    kind:       u8,
    pub length: u64,
    extents:    Vec<Extent>
}

impl Fork
{
    fn new(id: u32, kind: u8, length: u32, record: &[u8]) -> Result<Self>
    {
        let mut extents = Vec::new();
        for index in 0..3
        {
            let start = be16(record, index * 4)?;
            let count = be16(record, index * 4 + 2)?;
            if count != 0
            {
                extents.push(Extent { start,
                                      count });
            }
            else if start != 0
            {
                return Err(Error("invalid empty HFS extent"));
            }
        }
        Ok(Self { id,
                  kind,
                  length: length as u64,
                  extents })
    }

    fn blocks(&self) -> u64
    {
        self.extents.iter().map(|e| e.count as u64).sum()
    }
}

#[derive(Clone, Debug)]
pub struct Folder
{
    pub id:     u32,
    pub parent: u32,
    pub name:   Vec<u8>
}

#[derive(Clone, Debug)]
pub struct File
{
    pub id:        u32,
    pub file_type: [u8; 4],
    pub creator:   [u8; 4],
    pub data:      Fork,
    pub resources: Fork
}

/// Catalog metadata uses original Mac Roman names and HFS seconds since 1904.
#[derive(Clone, Debug)]
pub struct CatalogEntry
{
    pub id:                 u32,
    pub parent:             u32,
    pub name:               Vec<u8>,
    pub directory:          bool,
    pub flags:              u16,
    pub finder_info:        [u8; 32],
    pub created:            u32,
    pub modified:           u32,
    pub backup:             u32,
    pub child_count:        u16,
    pub data:               Option<Fork>,
    pub resources:          Option<Fork>,
    /// Exact physical lengths stored in the catalog, including preallocation.
    pub data_allocated:     u64,
    pub resource_allocated: u64
}

pub struct Volume<'a, D: BlockDevice>
{
    disk:                 &'a mut D,
    partition:            Partition,
    allocation_start:     u64,
    allocation_size:      u64,
    allocation_count:     u64,
    catalog:              Fork,
    overflow:             Fork,
    pub blessed_id:       u32,
    pub name:             Vec<u8>,
    pub created:          u32,
    pub modified:         u32,
    pub allocation_bytes: u64,
    pub free_bytes:       u64,
    pub attributes:       u16,
    pub finder_info:      [u8; 32]
}

impl<'a, D: BlockDevice> Volume<'a, D>
{
    pub fn mount(disk: &'a mut D, partition: Partition) -> Result<Self>
    {
        partition.start
                 .checked_add(partition.sectors)
                 .and_then(|end| end.checked_mul(512))
                 .ok_or(Error("HFS byte range overflow"))?;
        if partition.sectors < 3
           || partition.start
                       .checked_add(partition.sectors)
                       .is_none_or(|end| end > disk.sector_count())
        {
            return Err(Error("volume outside disk"));
        }
        let mut mdb = [0; 512];
        disk.read_sector(partition.start + 2, &mut mdb)?;
        if &mdb[..2] != b"BD"
        {
            return Err(Error("not a classic HFS volume"));
        }
        if be16(&mdb, 124)? == 0x482b
        {
            return Err(Error("HFS+ wrappers are not supported"));
        }
        let allocation_size = be32(&mdb, 20)? as u64;
        let allocation_start = be16(&mdb, 28)? as u64 * 512;
        let allocation_count = be16(&mdb, 18)? as u64;
        if allocation_size == 0
           || !allocation_size.is_multiple_of(512)
           || allocation_count == 0
           || allocation_start < 1536
           || allocation_start + allocation_count * allocation_size > partition.sectors * 512
        {
            return Err(Error("invalid HFS allocation geometry"));
        }
        let length = mdb[36] as usize;
        if length == 0 || length > 27
        {
            return Err(Error("invalid HFS volume name"));
        }
        let mut volume =
            Self { disk,
                   partition,
                   allocation_start,
                   allocation_size,
                   allocation_count,
                   catalog: Fork::new(4, 0, be32(&mdb, 146)?, bytes(&mdb, 150, 12)?)?,
                   overflow: Fork::new(3, 0, be32(&mdb, 130)?, bytes(&mdb, 134, 12)?)?,
                   blessed_id: be32(&mdb, 92)?,
                   name: bytes(&mdb, 37, length)?.to_vec(),
                   created: be32(&mdb, 2)?,
                   modified: be32(&mdb, 6)?,
                   allocation_bytes: allocation_count * allocation_size,
                   free_bytes: be16(&mdb, 34)? as u64 * allocation_size,
                   attributes: be16(&mdb, 10)?,
                   finder_info: bytes(&mdb, 92, 32)?.try_into().unwrap() };
        volume.validate_extents(&volume.overflow)?;
        volume.validate_extents(&volume.catalog)?;
        let mut catalog = volume.catalog.clone();
        volume.resolve_extents(&mut catalog)?;
        volume.catalog = catalog;
        Ok(volume)
    }

    fn validate_extents(&self, fork: &Fork) -> Result<()>
    {
        for (index, extent) in fork.extents.iter().enumerate()
        {
            let end = extent.start as u64 + extent.count as u64;
            if end > self.allocation_count
            {
                return Err(Error("HFS extent outside volume"));
            }
            if fork.extents[..index].iter().any(|other| {
                                               (extent.start as u64)
                                               < other.start as u64 + other.count as u64
                                               && (other.start as u64) < end
                                           })
            {
                return Err(Error("overlapping HFS extents"));
            }
        }
        Ok(())
    }

    fn read_fork(&mut self, fork: &Fork, offset: u64, output: &mut [u8]) -> Result<()>
    {
        if offset.checked_add(output.len() as u64).is_none_or(|end| end > fork.length)
        {
            return Err(Error("read past HFS fork EOF"));
        }
        let mut skip = offset;
        let mut done = 0;
        for extent in &fork.extents
        {
            let size = extent.count as u64 * self.allocation_size;
            if skip >= size
            {
                skip -= size;
                continue;
            }
            let length = (size - skip).min((output.len() - done) as u64) as usize;
            let physical = self.partition.start * 512
                           + self.allocation_start
                           + extent.start as u64 * self.allocation_size
                           + skip;
            read_at(self.disk, physical, &mut output[done..done + length])?;
            done += length;
            skip = 0;
            if done == output.len()
            {
                return Ok(());
            }
        }
        if done == output.len() { Ok(()) } else { Err(Error("HFS fork requires missing extents")) }
    }

    /// Scan the linked leaf nodes, with checked node boundaries and cycle bounds.
    fn find_record<T>(&mut self, fork: &Fork, mut find: impl FnMut(&[u8]) -> Result<Option<T>>)
                      -> Result<Option<T>>
    {
        let mut header = [0; 512];
        self.read_fork(fork, 0, &mut header)?;
        if header[8] != 1
        {
            return Err(Error("invalid HFS B-tree header node"));
        }
        let size = be16(&header, 32)? as usize;
        let total = be32(&header, 36)?;
        if !(512..=32768).contains(&size)
           || !size.is_power_of_two()
           || total == 0
           || total as u64 * size as u64 > fork.length
        {
            return Err(Error("invalid HFS B-tree geometry"));
        }
        let mut current = be32(&header, 24)?;
        let mut previous = 0;
        let mut buffer = vec![0; size];
        for _ in 0..total.min(65536)
        {
            if current == 0
            {
                return Ok(None);
            }
            if current >= total
            {
                return Err(Error("HFS leaf outside B-tree"));
            }
            self.read_fork(fork, current as u64 * size as u64, &mut buffer)?;
            if buffer[8] != 0xff || be32(&buffer, 4)? != previous
            {
                return Err(Error("invalid HFS leaf chain"));
            }
            let count = be16(&buffer, 10)? as usize;
            let table =
                size.checked_sub((count + 1) * 2).ok_or(Error("invalid HFS node record count"))?;
            let mut start = be16(&buffer, size - 2)? as usize;
            if start < 14 || start > table
            {
                return Err(Error("invalid HFS record offset"));
            }
            for index in 0..count
            {
                let end = be16(&buffer, size - (index + 2) * 2)? as usize;
                if end <= start || end > table
                {
                    return Err(Error("invalid HFS record bounds"));
                }
                if let Some(result) = find(&buffer[start..end])?
                {
                    return Ok(Some(result));
                }
                start = end;
            }
            previous = current;
            current = be32(&buffer, 0)?;
        }
        Err(Error("HFS leaf chain cycle or scan limit exceeded"))
    }

    fn resolve_extents(&mut self, fork: &mut Fork) -> Result<()>
    {
        while fork.blocks() * self.allocation_size < fork.length
        {
            if fork.id == 3 || fork.extents.len() > 1024
            {
                return Err(Error("unsupported extents-overflow tree growth"));
            }
            let overflow = self.overflow.clone();
            let next =
                u16::try_from(fork.blocks()).map_err(|_| Error("HFS allocation index overflow"))?;
            let record = self.find_record(&overflow, |record| {
                                 if record.first() != Some(&7)
                                 {
                                     return Err(Error("invalid HFS extent key"));
                                 }
                                 bytes(record, 0, 20)?;
                                 if record[1] == fork.kind
                                    && be32(record, 2)? == fork.id
                                    && be16(record, 6)? == next
                                 {
                                     Ok(Some(bytes(record, 8, 12)?.to_vec()))
                                 }
                                 else
                                 {
                                     Ok(None)
                                 }
                             })?
                             .ok_or(Error("missing HFS overflow extent record"))?;
            let extension = Fork::new(fork.id, fork.kind, 0, &record)?;
            if extension.extents.is_empty()
            {
                return Err(Error("empty HFS overflow extent record"));
            }
            fork.extents.extend(extension.extents);
            self.validate_extents(fork)?;
        }
        Ok(())
    }

    /// Find a catalog object by persistent CNID, ignoring thread records.
    pub fn entry(&mut self, id: u32) -> Result<Option<CatalogEntry>>
    {
        let catalog = self.catalog.clone();
        self.find_record(&catalog, |record| {
                let entry = parse_entry(record)?;
                Ok(entry.filter(|entry| entry.id == id))
            })
    }

    /// Exact physical fork lengths, including allocation beyond logical EOF.
    /// Directories themselves have no data/resource fork allocation.
    pub fn allocation_info(&mut self, id: u32) -> Result<Option<(u64, u64)>>
    {
        let Some(entry) = self.entry(id)?
        else
        {
            return Ok(None);
        };
        for (physical, fork) in
            [(entry.data_allocated, &entry.data), (entry.resource_allocated, &entry.resources)]
        {
            if let Some(fork) = fork
               && (physical < fork.length
                   || !physical.is_multiple_of(self.allocation_size)
                   || physical > self.allocation_bytes)
            {
                return Err(Error("invalid HFS physical fork length"));
            }
        }
        Ok(Some((entry.data_allocated, entry.resource_allocated)))
    }

    /// Enumerate direct children in native catalog order without collecting the directory.
    pub fn child(&mut self, parent: u32, index: u32) -> Result<Option<CatalogEntry>>
    {
        let catalog = self.catalog.clone();
        let mut current = 0;
        self.find_record(&catalog, |record| {
                let entry = parse_entry(record)?;
                if let Some(entry) = entry.filter(|entry| entry.parent == parent)
                {
                    if current == index
                    {
                        return Ok(Some(entry));
                    }
                    current += 1;
                }
                Ok(None)
            })
    }

    pub fn lookup(&mut self, parent: u32, name: &[u8]) -> Result<Option<CatalogEntry>>
    {
        let catalog = self.catalog.clone();
        self.find_record(&catalog, |record| {
                let entry = parse_entry(record)?;
                Ok(entry.filter(|entry| entry.parent == parent && names_equal(&entry.name, name)))
            })
    }

    /// Resolve both inline and overflow extents, then read at most the remaining fork bytes.
    /// The caller owns the output buffer; no whole-fork allocation is performed.
    pub fn read(&mut self, fork: &mut Fork, offset: u64, output: &mut [u8]) -> Result<usize>
    {
        if offset > fork.length
        {
            return Err(Error("read past HFS fork EOF"));
        }
        self.validate_extents(fork)?;
        self.resolve_extents(fork)?;
        let length = (fork.length - offset).min(output.len() as u64) as usize;
        self.read_fork(fork, offset, &mut output[..length])?;
        Ok(length)
    }

    pub fn blessed_folder(&mut self) -> Result<Folder>
    {
        if self.blessed_id < 2
        {
            return Err(Error("volume has no blessed System Folder"));
        }
        let id = self.blessed_id;
        let catalog = self.catalog.clone();
        self.find_record(&catalog, |record| {
                let (parent, name, data) = catalog_record(record)?;
                if data.first() == Some(&1) && be32(data, 6)? == id
                {
                    Ok(Some(Folder { id,
                                     parent,
                                     name: name.to_vec() }))
                }
                else
                {
                    Ok(None)
                }
            })?
            .ok_or(Error("blessed System Folder CNID is missing"))
    }

    pub fn find_file(&mut self, parent_id: u32, name: &[u8]) -> Result<File>
    {
        let catalog = self.catalog.clone();
        let mut file = self.find_record(&catalog, |record| {
                               let (parent, candidate, data) = catalog_record(record)?;
                               if parent != parent_id
                                  || !names_equal(candidate, name)
                                  || data.first() != Some(&2)
                               {
                                   return Ok(None);
                               }
                               let id = be32(data, 20)?;
                               Ok(Some(File { id,
                                              file_type: bytes(data, 4, 4)?.try_into().unwrap(),
                                              creator: bytes(data, 8, 4)?.try_into().unwrap(),
                                              data: Fork::new(id,
                                                              0,
                                                              be32(data, 26)?,
                                                              bytes(data, 74, 12)?)?,
                                              resources: Fork::new(id,
                                                                   0xff,
                                                                   be32(data, 36)?,
                                                                   bytes(data, 86, 12)?)? }))
                           })?
                           .ok_or(Error("startup file is missing from blessed folder"))?;
        self.validate_extents(&file.data)?;
        self.validate_extents(&file.resources)?;
        self.resolve_extents(&mut file.resources)?;
        Ok(file)
    }

    pub fn read_resources(&mut self, file: &File, maximum: usize) -> Result<Vec<u8>>
    {
        let size =
            usize::try_from(file.resources.length).map_err(|_| Error("resource fork too large"))?;
        if size == 0 || size > maximum
        {
            return Err(Error("startup resource fork is empty or too large"));
        }
        let mut data = vec![0; size];
        self.read_fork(&file.resources, 0, &mut data)?;
        Ok(data)
    }
}

fn catalog_record(record: &[u8]) -> Result<(u32, &[u8], &[u8])>
{
    let key = *record.first().ok_or(Error("empty HFS catalog record"))? as usize;
    if !(6..=37).contains(&key)
    {
        return Err(Error("invalid HFS catalog key length"));
    }
    let name_length = *bytes(record, 6, 1)?.first().unwrap() as usize;
    if name_length > 31 || key < name_length + 6 || key > name_length + 7
    {
        return Err(Error("invalid HFS catalog name"));
    }
    let data_start = (key + 2) & !1;
    Ok((be32(record, 2)?,
        bytes(record, 7, name_length)?,
        bytes(record,
              data_start,
              record.len().checked_sub(data_start).ok_or(Error("truncated HFS catalog key"))?)?))
}

fn parse_entry(record: &[u8]) -> Result<Option<CatalogEntry>>
{
    let (parent, name, data) = catalog_record(record)?;
    let kind = *data.first().ok_or(Error("empty HFS catalog data"))?;
    let (id, flags, info, dates, child_count, forks) = match kind
    {
        1 =>
        {
            bytes(data, 0, 70)?;
            (be32(data, 6)?, be16(data, 2)?, 22, 10, be16(data, 4)?, None)
        }
        2 =>
        {
            bytes(data, 0, 102)?;
            (be32(data, 20)?,
             data[2] as u16,
             4,
             44,
             0,
             Some((Fork::new(be32(data, 20)?, 0, be32(data, 26)?, bytes(data, 74, 12)?)?,
                   Fork::new(be32(data, 20)?, 0xff, be32(data, 36)?, bytes(data, 86, 12)?)?)))
        }
        3 | 4 => return Ok(None),
        _ => return Err(Error("unknown HFS catalog record kind"))
    };
    let (data_fork, resource_fork) = match forks
    {
        Some((d, r)) => (Some(d), Some(r)),
        None => (None, None)
    };
    let mut finder_info = [0; 32];
    finder_info[..16].copy_from_slice(bytes(data, info, 16)?);
    finder_info[16..].copy_from_slice(bytes(data, if kind == 1 { 38 } else { 56 }, 16)?);
    Ok(Some(CatalogEntry { id,
                           parent,
                           name: name.to_vec(),
                           directory: kind == 1,
                           flags,
                           finder_info,
                           created: be32(data, dates)?,
                           modified: be32(data, dates + 4)?,
                           backup: be32(data, dates + 8)?,
                           child_count,
                           data: data_fork,
                           resources: resource_fork,
                           data_allocated: if kind == 2
                           {
                               u64::from(be32(data, 30)?)
                           }
                           else
                           {
                               0
                           },
                           resource_allocated: if kind == 2
                           {
                               u64::from(be32(data, 40)?)
                           }
                           else
                           {
                               0
                           } }))
}

/// Classic HFS is case insensitive and accent sensitive. Mac Roman's upper/lower
/// pairs are folded before comparing; callers keep original spelling for display.
pub fn names_equal(a: &[u8], b: &[u8]) -> bool
{
    // Equality classes from Apple's classic HFS FastRelString gCompareTable:
    // https://github.com/apple-oss-distributions/hfs/blob/main/core/UCStringCompareData.h
    // This deliberately does not use Unicode case folding: later Mac Roman
    // uppercase additions are distinct in the original catalog comparator.
    fn fold(byte: u8) -> u8
    {
        match byte
        {
            0x61 => 0x41,
            0x62 => 0x42,
            0x63 => 0x43,
            0x64 => 0x44,
            0x65 => 0x45,
            0x66 => 0x46,
            0x67 => 0x47,
            0x68 => 0x48,
            0x69 => 0x49,
            0x6a => 0x4a,
            0x6b => 0x4b,
            0x6c => 0x4c,
            0x6d => 0x4d,
            0x6e => 0x4e,
            0x6f => 0x4f,
            0x70 => 0x50,
            0x71 => 0x51,
            0x72 => 0x52,
            0x73 => 0x53,
            0x74 => 0x54,
            0x75 => 0x55,
            0x76 => 0x56,
            0x77 => 0x57,
            0x78 => 0x58,
            0x79 => 0x59,
            0x7a => 0x5a,
            0x8a => 0x80,
            0x8c => 0x81,
            0x8d => 0x82,
            0x8e => 0x83,
            0x96 => 0x84,
            0x9a => 0x85,
            0x9f => 0x86,
            0xbe => 0xae,
            0xbf => 0xaf,
            0xca => 0x20,
            0xcb => 0x88,
            0xcc => 0x8b,
            0xcd => 0x9b,
            0xcf => 0xce,
            _ => byte
        }
    }
    a.len() == b.len() && a.iter().zip(b).all(|(&a, &b)| fold(a) == fold(b))
}
