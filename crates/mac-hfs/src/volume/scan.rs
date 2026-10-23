//! Incremental catalog reads using the resolved fork captured at mount time.
//! One step reads one leaf and inspects at most eight catalog records.
use alloc::vec::Vec;

use super::{CatalogEntry, Fork, Volume, catalog_record, parse_entry};
use crate::{BlockDevice, Error, Partition, Result, be16, be32, read_at};

const MAX_NODES: u32 = 65_536;
const RECORD_BUDGET: usize = 8;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor
{
    pub node:      u32,
    pub previous:  u32,
    pub index:     u32,
    pub traversed: u32,
    /// 0=new, 1=active, 2=finished.
    pub state:     u32
}
#[derive(Debug)]
pub enum ScanError
{
    InvalidCursor,
    NoMemory,
    Catalog(crate::Error)
}
impl From<crate::Error> for ScanError
{
    fn from(error: crate::Error) -> Self
    {
        Self::Catalog(error)
    }
}
pub enum Step
{
    Entry(CatalogEntry),
    Pending,
    Done
}
/// Resolved catalog extents and validated header geometry. This snapshot must be
/// replaced whenever a mutation can rewrite the catalog or its fork extents.
pub struct CatalogScanner
{
    partition:        Partition,
    allocation_start: u64,
    allocation_size:  u64,
    allocation_bytes: u64,
    fork:             Fork,
    node_size:        usize,
    total:            u32,
    first:            u32
}
impl<D: BlockDevice> Volume<'_, D>
{
    pub fn catalog_scanner(&mut self) -> Result<CatalogScanner>
    {
        let mut extents = Vec::new();
        extents.try_reserve_exact(self.catalog.extents.len()).map_err(|_| {
                                                                  Error::Malformed("catalog scan \
                                                                                    allocation \
                                                                                    failed")
                                                              })?;
        extents.extend_from_slice(&self.catalog.extents);
        let fork = Fork { id: self.catalog.id,
                          kind: self.catalog.kind,
                          length: self.catalog.length,
                          extents };
        let mut header = [0; 512];
        self.read_fork(&fork, 0, &mut header)?;
        let size = usize::from(be16(&header, 32)?);
        let total = be32(&header, 36)?;
        let first = be32(&header, 24)?;
        if header[8] != 1
           || !(512..=32768).contains(&size)
           || !size.is_power_of_two()
           || total == 0
           || total > MAX_NODES
           || u64::from(total) * size as u64 > fork.length
           || first >= total
        {
            return Err(Error::Malformed("invalid incremental catalog geometry"));
        }
        Ok(CatalogScanner { partition: self.partition,
                            allocation_start: self.allocation_start,
                            allocation_size: self.allocation_size,
                            allocation_bytes: self.allocation_bytes,
                            fork,
                            node_size: size,
                            total,
                            first })
    }
}
impl CatalogScanner
{
    fn read(&self, disk: &mut impl BlockDevice, offset: u64, output: &mut [u8]) -> Result<()>
    {
        if offset.checked_add(output.len() as u64).is_none_or(|end| end > self.fork.length)
        {
            return Err(Error::Malformed("scan outside catalog fork"));
        }
        let mut skip = offset;
        let mut done = 0;
        // Resolved extents were validated at mount. No overflow-tree traversal
        // occurs while a caller advances a scan cursor.
        for extent in &self.fork.extents
        {
            let size = u64::from(extent.count) * self.allocation_size;
            if skip >= size
            {
                skip -= size;
                continue;
            }
            let length = (size - skip).min((output.len() - done) as u64) as usize;
            let physical = self.partition.start * 512
                           + self.allocation_start
                           + u64::from(extent.start) * self.allocation_size
                           + skip;
            read_at(disk, physical, &mut output[done..done + length])?;
            done += length;
            skip = 0;
            if done == output.len()
            {
                return Ok(());
            }
        }
        Err(Error::Malformed("catalog scan has missing extents"))
    }

    pub fn step(&self, disk: &mut impl BlockDevice, parent: u32, cursor: &mut Cursor)
                -> core::result::Result<Step, ScanError>
    {
        let mut next = *cursor;
        if parent < 2
        {
            return Err(ScanError::InvalidCursor);
        }
        match next.state
        {
            0 =>
            {
                if next != Cursor::default()
                {
                    return Err(ScanError::InvalidCursor);
                }
                next.node = self.first;
                next.state = 1;
            }
            1 =>
            {}
            2 =>
            {
                if next.node != 0 || next.previous != 0 || next.index != 0 || next.traversed != 0
                {
                    return Err(ScanError::InvalidCursor);
                }
                return Ok(Step::Done);
            }
            _ => return Err(ScanError::InvalidCursor)
        }
        if next.node == 0
        {
            next = Cursor { state: 2,
                            ..Default::default() };
            *cursor = next;
            return Ok(Step::Done);
        }
        if next.node >= self.total
           || next.previous >= self.total
           || next.node == next.previous
           || next.traversed >= self.total
           || next.traversed >= MAX_NODES
           || (next.traversed == 0 && (next.node != self.first || next.previous != 0))
           || (next.traversed != 0 && next.previous == 0)
        {
            return Err(ScanError::InvalidCursor);
        }
        let mut buffer = Vec::new();
        buffer.try_reserve_exact(self.node_size).map_err(|_| ScanError::NoMemory)?;
        buffer.resize(self.node_size, 0);
        self.read(disk, u64::from(next.node) * self.node_size as u64, &mut buffer)?;
        if buffer[8] != 0xff || be32(&buffer, 4)? != next.previous
        {
            return Err(Error::Malformed("invalid incremental catalog leaf chain").into());
        }
        let forward = be32(&buffer, 0)?;
        if forward >= self.total
           || forward == next.node
           || (forward != 0 && forward == next.previous)
        {
            return Err(Error::Malformed("catalog scan leaf cycle or invalid link").into());
        }
        let count = usize::from(be16(&buffer, 10)?);
        let table = self.node_size
                        .checked_sub((count + 1) * 2)
                        .ok_or(Error::Malformed("invalid scan record count"))?;
        if next.index as usize > count
        {
            return Err(ScanError::InvalidCursor);
        }
        let mut start = usize::from(be16(&buffer, self.node_size - 2)?);
        if start < 14 || start > table
        {
            return Err(Error::Malformed("invalid scan record offset").into());
        }
        // Validate the complete offset table even when resuming in its middle.
        for index in 0..count
        {
            let end = usize::from(be16(&buffer, self.node_size - (index + 2) * 2)?);
            if end <= start || end > table
            {
                return Err(Error::Malformed("invalid scan record bounds").into());
            }
            start = end;
        }
        let begin = next.index as usize;
        let end = (begin + RECORD_BUDGET).min(count);
        for index in begin..end
        {
            let start = usize::from(be16(&buffer, self.node_size - (index + 1) * 2)?);
            let finish = usize::from(be16(&buffer, self.node_size - (index + 2) * 2)?);
            let record = &buffer[start..finish];
            let (record_parent, _, data) = catalog_record(record)?;
            next.index = (index + 1) as u32;
            if record_parent == parent
            {
                if data.first() == Some(&4)
                {
                    return Err(ScanError::InvalidCursor);
                }
                if let Some(entry) = parse_entry(record)?
                {
                    for (physical, fork) in [(entry.data_allocated, &entry.data),
                                             (entry.resource_allocated, &entry.resources)]
                    {
                        if let Some(fork) = fork
                           && (physical < fork.length
                               || !physical.is_multiple_of(self.allocation_size)
                               || physical > self.allocation_bytes)
                        {
                            return Err(Error::Malformed("invalid scanned physical fork length").into());
                        }
                    }
                    *cursor = next;
                    return Ok(Step::Entry(entry));
                }
            }
            else if record_parent > parent
            {
                *cursor = Cursor { state: 2,
                                   ..Default::default() };
                return Ok(Step::Done);
            }
        }
        if end == count
        {
            next.previous = next.node;
            next.node = forward;
            next.index = 0;
            next.traversed =
                next.traversed.checked_add(1).ok_or(Error::Malformed("scan traversal overflow"))?;
            if forward == 0
            {
                next = Cursor { state: 2,
                                ..Default::default() };
            }
        }
        let done = next.state == 2;
        *cursor = next;
        Ok(if done { Step::Done } else { Step::Pending })
    }
}
