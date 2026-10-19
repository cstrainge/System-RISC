//! Bounded durable metadata mutations; file payload sectors are never overwritten.
use core::cmp::Ordering;

use super::*;
use crate::WritableBlockDevice;
mod comments;
mod data;
pub use data::DataSave;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationError
{
    Invalid,
    ReadOnly,
    NotFound,
    Exists,
    Protected,
    Busy,
    Full,
    NoMemory,
    Unsupported,
    Io,
    RollbackFailed
}
pub type MutationResult<T> = core::result::Result<T, MutationError>;
const TREE_MAX: usize = 512 * 1024;
const MAX_CHANGES: usize = 2048;
fn reserve<T>(v: &mut Vec<T>, count: usize) -> MutationResult<()>
{
    v.try_reserve_exact(count).map_err(|_| MutationError::NoMemory)
}
fn copy(bytes: &[u8]) -> MutationResult<Vec<u8>>
{
    let mut out = Vec::new();
    reserve(&mut out, bytes.len())?;
    out.extend_from_slice(bytes);
    Ok(out)
}
fn word(bytes: &[u8], at: usize) -> MutationResult<u16>
{
    be16(bytes, at).map_err(|_| MutationError::Io)
}
fn long(bytes: &[u8], at: usize) -> MutationResult<u32>
{
    be32(bytes, at).map_err(|_| MutationError::Io)
}
fn put16(bytes: &mut [u8], at: usize, value: u16)
{
    bytes[at..at + 2].copy_from_slice(&value.to_be_bytes());
}
fn put32(bytes: &mut [u8], at: usize, value: u32)
{
    bytes[at..at + 4].copy_from_slice(&value.to_be_bytes());
}
fn key(record: &[u8]) -> MutationResult<(u32, &[u8], usize)>
{
    let (p, n, _) = catalog_record(record).map_err(|_| MutationError::Io)?;
    Ok((p, n, (record[0] as usize + 2) & !1))
}
fn record(parent: u32, name: &[u8], data: &[u8]) -> MutationResult<Vec<u8>>
{
    let start = (name.len() + 8) & !1;
    let mut r = Vec::new();
    reserve(&mut r, start + data.len())?;
    r.resize(start, 0);
    r[0] = (6 + name.len()) as u8;
    put32(&mut r, 2, parent);
    r[6] = name.len() as u8;
    r[7..7 + name.len()].copy_from_slice(name);
    r.extend_from_slice(data);
    Ok(r)
}
struct Tree
{
    header:   [u8; 512],
    records:  Vec<Vec<u8>>,
    capacity: usize,
    maps:     Vec<[u8; 512]>
}
impl<'a, D: BlockDevice> Volume<'a, D>
{
    fn mutation_tree(&mut self, fork: &Fork) -> MutationResult<Tree>
    {
        if fork.length < 512 || fork.length > 65536 * 512 || !fork.length.is_multiple_of(512)
        {
            return Err(MutationError::Unsupported);
        }
        let mut header = [0; 512];
        self.read_fork(fork, 0, &mut header).map_err(|_| MutationError::Io)?;
        let capacity = fork.length as usize / 512;
        if header[8] != 1
           || word(&header, 10)? != 3
           || word(&header, 32)? != 512
           || long(&header, 36)? as usize != capacity
           || capacity > 65536
           || word(&header, 510)? != 14
           || word(&header, 508)? != 120
           || word(&header, 506)? != 248
           || word(&header, 504)? != 504
        {
            return Err(MutationError::Unsupported);
        }
        let mut maps = Vec::new();
        let mut map_id = long(&header, 0)?;
        while map_id != 0
        {
            if map_id as usize != maps.len() + 1 || map_id as usize >= capacity || maps.len() >= 64
            {
                return Err(MutationError::Unsupported);
            }
            let mut map = [0; 512];
            self.read_fork(fork, map_id as u64 * 512, &mut map).map_err(|_| MutationError::Io)?;
            if map[8] != 2
               || word(&map, 10)? != 1
               || long(&map, 4)? != maps.len() as u32
               || word(&map, 510)? != 14
               || word(&map, 508)? < 14
               || word(&map, 508)? > 508
            {
                return Err(MutationError::Unsupported);
            }
            map_id = long(&map, 0)?;
            reserve(&mut maps, 1)?;
            maps.push(map);
        }
        let coverage =
            256 + maps.iter().map(|m| word(m, 508).unwrap() as usize - 14).sum::<usize>();
        if coverage * 8 < capacity
        {
            return Err(MutationError::Unsupported);
        }
        let mut records = Vec::new();
        let mut current = long(&header, 24)?;
        let mut previous = 0;
        let mut total_bytes = 0;
        for _ in 0..capacity
        {
            if current == 0
            {
                if previous != long(&header, 28)? || records.len() != long(&header, 20)? as usize
                {
                    return Err(MutationError::Io);
                }
                return Ok(Tree { header,
                                 records,
                                 capacity,
                                 maps });
            }
            if current as usize >= capacity
            {
                return Err(MutationError::Io);
            }
            let mut node = [0; 512];
            self.read_fork(fork, current as u64 * 512, &mut node).map_err(|_| MutationError::Io)?;
            if node[8] != 255 || node[9] != 1 || long(&node, 4)? != previous
            {
                return Err(MutationError::Io);
            }
            let count = word(&node, 10)? as usize;
            if count == 0 || count > 248
            {
                return Err(MutationError::Io);
            }
            let table = 512 - (count + 1) * 2;
            let mut start = word(&node, 510)? as usize;
            for i in 0..count
            {
                let end = word(&node, 512 - (i + 2) * 2)? as usize;
                if start < 14 || end <= start || end > table
                {
                    return Err(MutationError::Io);
                }
                total_bytes += end - start;
                if total_bytes > TREE_MAX || records.len() >= 8192
                {
                    return Err(MutationError::Full);
                }
                reserve(&mut records, 1)?;
                records.push(copy(&node[start..end])?);
                start = end;
            }
            previous = current;
            current = long(&node, 0)?;
        }
        Err(MutationError::Io)
    }

    fn mutation_catalog(&mut self) -> MutationResult<Catalog>
    {
        let mut mdb = [0; 512];
        self.disk.read_sector(self.partition.start + 2, &mut mdb).map_err(|_| MutationError::Io)?;
        let catalog = self.catalog.clone();
        let tree = self.mutation_tree(&catalog)?;
        let mut entries = Vec::new();
        reserve(&mut entries, tree.records.len())?;
        for r in &tree.records
        {
            if let Some(e) = parse_entry(r).map_err(|_| MutationError::Io)?
            {
                if entries.iter().any(|old: &CatalogEntry| old.id == e.id)
                {
                    return Err(MutationError::Io);
                }
                entries.push(e);
            }
        }
        Ok(Catalog { tree,
                     entries,
                     mdb })
    }

    pub fn trash_usage(&mut self) -> MutationResult<(u32, u64)>
    {
        let Some(trash) = self.lookup(2, b"Trash").map_err(|_| MutationError::Io)?
        else
        {
            return Ok((0, 0));
        };
        if !trash.directory
        {
            return Err(MutationError::Exists);
        }
        let cat = self.mutation_catalog()?;
        let mut count = 0u32;
        let mut bytes = 0u64;
        for entry in &cat.entries
        {
            if entry.id != trash.id && cat.below(entry.id, trash.id)?
            {
                count = count.checked_add(1).ok_or(MutationError::Full)?;
                if !entry.directory
                {
                    let (r, start) = cat.record_index(entry.id)?;
                    bytes = bytes.checked_add(long(&cat.tree.records[r], start + 30)? as u64
                                              + long(&cat.tree.records[r], start + 40)? as u64)
                                 .ok_or(MutationError::Full)?;
                }
            }
        }
        Ok((count, bytes))
    }

    fn mutation_sector(&self, fork: &Fork, offset: usize) -> MutationResult<u64>
    {
        let mut skip = offset as u64;
        for e in &fork.extents
        {
            let bytes = e.count as u64 * self.allocation_size;
            if skip < bytes
            {
                return Ok(self.partition.start
                          + self.allocation_start / 512
                          + e.start as u64 * (self.allocation_size / 512)
                          + skip / 512);
            }
            skip -= bytes;
        }
        Err(MutationError::Io)
    }
}
// Apple's classic RelString weights, preserving accent-sensitive HFS ordering.
// Source: apple-oss-distributions/hfs/core/UCStringCompareData.h gCompareTable.
const WEIGHTS: [u16; 128] =
    [0x4108, 0x410c, 0x4310, 0x4502, 0x4e0a, 0x4f08, 0x5508, 0x4182, 0x4104, 0x4186, 0x4108,
     0x410a, 0x410c, 0x4310, 0x4502, 0x4584, 0x4586, 0x4588, 0x4982, 0x4984, 0x4986, 0x4988,
     0x4e0a, 0x4f82, 0x4f84, 0x4f86, 0x4f08, 0x4f0a, 0x5582, 0x5584, 0x5586, 0x5508, 0xa000,
     0xa100, 0xa200, 0xa300, 0xa400, 0xa500, 0xa600, 0x5382, 0xa800, 0xa900, 0xaa00, 0xab00,
     0xac00, 0xad00, 0x4114, 0x4f0e, 0xb000, 0xb100, 0xb200, 0xb300, 0xb400, 0xb500, 0xb600,
     0xb700, 0xb800, 0xb900, 0xba00, 0x4192, 0x4f92, 0xbd00, 0x4114, 0x4f0e, 0xc000, 0xc100,
     0xc200, 0xc300, 0xc400, 0xc500, 0xc600, 0x2206, 0x2208, 0xc900, 0x2000, 0x4104, 0x410a,
     0x4f0a, 0x4f14, 0x4f14, 0xd000, 0xd100, 0x2202, 0x2204, 0x2702, 0x2704, 0xd600, 0xd700,
     0x5988, 0xd900, 0xda00, 0xdb00, 0xdc00, 0xdd00, 0xde00, 0xdf00, 0xe000, 0xe100, 0xe200,
     0xe300, 0xe400, 0xe500, 0xe600, 0xe700, 0xe800, 0xe900, 0xea00, 0xeb00, 0xec00, 0xed00,
     0xee00, 0xef00, 0xf000, 0xf100, 0xf200, 0xf300, 0xf400, 0xf500, 0xf600, 0xf700, 0xf800,
     0xf900, 0xfa00, 0xfb00, 0xfc00, 0xfd00, 0xfe00, 0xff00];
fn weight(c: u8) -> u16
{
    match c
    {
        b'a'..=b'z' => (c - 32) as u16 * 256,
        0x60 => 0x4180,
        128..=255 => WEIGHTS[(c - 128) as usize],
        _ => (c as u16) * 256
    }
}
fn name_order(a: &[u8], b: &[u8]) -> Ordering
{
    a.iter().map(|&c| weight(c)).cmp(b.iter().map(|&c| weight(c)))
}
fn order(a: &[u8], b: &[u8], catalog: bool) -> Ordering
{
    if catalog
    {
        let (p, a, _) = key(a).expect("prevalidated key");
        let (q, b, _) = key(b).expect("prevalidated key");
        p.cmp(&q).then_with(|| name_order(a, b))
    }
    else
    {
        long(a, 2).unwrap()
                  .cmp(&long(b, 2).unwrap())
                  .then_with(|| a[1].cmp(&b[1]))
                  .then_with(|| word(a, 6).unwrap().cmp(&word(b, 6).unwrap()))
    }
}
struct Level
{
    id:  u32,
    key: Vec<u8>
}
fn desktop_key_span(record: &[u8]) -> MutationResult<usize>
{
    let length = usize::from(*record.first().ok_or(MutationError::Io)?);
    let span = (length + 2) & !1;
    if length == 0 || length > 37 || span > record.len()
    {
        return Err(MutationError::Unsupported);
    }
    Ok(span)
}
fn build_tree(tree: &mut Tree, catalog: bool, desktop: bool) -> MutationResult<Vec<[u8; 512]>>
{
    for r in &tree.records
    {
        if desktop
        {
            desktop_key_span(r)?;
        }
        else if catalog
        {
            key(r)?;
        }
        else if r.len() != 20 || r[0] != 7
        {
            return Err(MutationError::Io);
        }
    }
    if !desktop
    {
        tree.records.sort_unstable_by(|a, b| order(a, b, catalog));
    }
    if !desktop && tree.records.windows(2).any(|r| order(&r[0], &r[1], catalog) == Ordering::Equal)
    {
        return Err(MutationError::Exists);
    }
    let mut nodes = Vec::new();
    reserve(&mut nodes, tree.capacity.min(TREE_MAX / 512))?;
    nodes.push(tree.header);
    nodes.extend_from_slice(&tree.maps);
    let mut level = Vec::new();
    let mut at = 0;
    let mut height = 1;
    let first = if tree.records.is_empty() { 0 } else { nodes.len() as u32 };
    let mut last = 0;
    while at < tree.records.len()
    {
        let mut end = at;
        let mut used = 14;
        while end < tree.records.len() && used + tree.records[end].len() + 2 * (end - at + 2) <= 512
        {
            used += tree.records[end].len();
            end += 1;
        }
        if end == at
        {
            return Err(MutationError::Full);
        }
        if nodes.len() >= tree.capacity || nodes.len() >= TREE_MAX / 512
        {
            return Err(MutationError::Full);
        }
        let id = nodes.len() as u32;
        let mut node = [0; 512];
        node[8] = 255;
        node[9] = 1;
        put16(&mut node, 10, (end - at) as u16);
        put32(&mut node, 4, last);
        if last != 0
        {
            put32(&mut nodes[last as usize], 0, id);
        }
        last = id;
        let mut offset = 14;
        for (i, r) in tree.records[at..end].iter().enumerate()
        {
            put16(&mut node, 512 - (i + 1) * 2, offset as u16);
            node[offset..offset + r.len()].copy_from_slice(r);
            offset += r.len();
        }
        put16(&mut node, 512 - (end - at + 1) * 2, offset as u16);
        let key_bytes = if desktop
        {
            desktop_key_span(&tree.records[at])?
        }
        else if catalog
        {
            key(&tree.records[at])?.2
        }
        else
        {
            8
        };
        reserve(&mut level, 1)?;
        level.push(Level { id,
                           key: copy(&tree.records[at][..key_bytes])? });
        nodes.push(node);
        at = end;
    }
    while level.len() > 1
    {
        height += 1;
        if height > 8
        {
            return Err(MutationError::Full);
        }
        let mut parents = Vec::new();
        let mut at = 0;
        let index_key = if catalog { 38 } else { 8 };
        let record_size = index_key + 4;
        while at < level.len()
        {
            let end = (at + (512 - 16) / (record_size + 2)).min(level.len());
            if end == at || nodes.len() >= tree.capacity
            {
                return Err(MutationError::Full);
            }
            let id = nodes.len() as u32;
            let mut node = [0; 512];
            node[8] = 0;
            node[9] = height;
            put16(&mut node, 10, (end - at) as u16);
            let mut offset = 14;
            for (i, child) in level[at..end].iter().enumerate()
            {
                put16(&mut node, 512 - (i + 1) * 2, offset as u16);
                node[offset..offset + child.key.len()].copy_from_slice(&child.key);
                node[offset] = if catalog { 37 } else { 7 };
                put32(&mut node, offset + index_key, child.id);
                offset += record_size;
            }
            put16(&mut node, 512 - (end - at + 1) * 2, offset as u16);
            reserve(&mut parents, 1)?;
            parents.push(Level { id,
                                 key: copy(&level[at].key)? });
            nodes.push(node);
            at = end;
        }
        level = parents;
    }
    let used = nodes.len();
    let h = &mut nodes[0];
    put16(h, 14, if level.is_empty() { 0 } else { height as u16 });
    put32(h, 16, level.first().map_or(0, |l| l.id));
    put32(h, 20, tree.records.len() as u32);
    put32(h, 24, first);
    put32(h, 28, last);
    put32(h, 40, (tree.capacity - used) as u32);
    h[248..504].fill(0);
    let mut bit_start = 0usize;
    for (node_index, node) in nodes.iter_mut().enumerate().take(tree.maps.len() + 1)
    {
        let (start, end) =
            if node_index == 0 { (248, 504) } else { (14, word(node, 508)? as usize) };
        node[start..end].fill(0);
        let coverage = (end - start) * 8;
        for bit in 0..coverage
        {
            if bit_start + bit < used
            {
                node[start + bit / 8] |= 0x80 >> (bit % 8);
            }
        }
        bit_start += coverage;
    }
    Ok(nodes)
}
struct Change
{
    sector: u64,
    old:    [u8; 512],
    new:    [u8; 512],
    phase:  u8
}
struct Transaction
{
    changes: Vec<Change>
}
impl Transaction
{
    fn new() -> Self
    {
        Self { changes: Vec::new() }
    }

    fn stage(&mut self, disk: &mut impl BlockDevice, sector: u64, new: [u8; 512], phase: u8)
             -> MutationResult<()>
    {
        if self.changes.iter().any(|c| c.sector == sector)
        {
            return Err(MutationError::Io);
        }
        let mut old = [0; 512];
        disk.read_sector(sector, &mut old).map_err(|_| MutationError::Io)?;
        if old == new
        {
            return Ok(());
        }
        if self.changes.len() == MAX_CHANGES
        {
            return Err(MutationError::Full);
        }
        reserve(&mut self.changes, 1)?;
        self.changes.push(Change { sector,
                                   old,
                                   new,
                                   phase });
        Ok(())
    }

    fn commit(mut self, disk: &mut impl WritableBlockDevice) -> MutationResult<()>
    {
        self.changes.sort_unstable_by_key(|c| c.phase);
        let mut attempted = Vec::new();
        reserve(&mut attempted, self.changes.len() + 2)?;
        let result = (|| -> Result<()> {
            // Mark both MDBs dirty before touching catalog state. A crashed partial write
            // therefore does not silently advertise a clean volume on the next boot.
            for (index, c) in self.changes.iter().enumerate().filter(|(_, c)| c.phase == 2)
            {
                let mut dirty = c.old;
                let attrs = be16(&dirty, 10)?;
                put16(&mut dirty, 10, attrs & !0x0100);
                attempted.push(index);
                disk.write_sector(c.sector, &dirty)?;
            }
            disk.flush()?;
            for phase in 0..=2
            {
                for (index, c) in self.changes.iter().enumerate().filter(|(_, c)| c.phase == phase)
                {
                    attempted.push(index);
                    disk.write_sector(c.sector, &c.new)?;
                }
                disk.flush()?;
            }
            Ok(())
        })();
        if result.is_ok()
        {
            return Ok(());
        }
        let mut failed = false;
        for &index in attempted.iter().rev()
        {
            let c = &self.changes[index];
            if disk.write_sector(c.sector, &c.old).is_err()
            {
                failed = true;
            }
        }
        if disk.flush().is_err()
        {
            failed = true;
        }
        Err(if failed { MutationError::RollbackFailed } else { MutationError::Io })
    }
}
struct Catalog
{
    tree:    Tree,
    entries: Vec<CatalogEntry>,
    mdb:     [u8; 512]
}
impl Catalog
{
    fn index(&self, id: u32) -> MutationResult<usize>
    {
        self.entries.iter().position(|e| e.id == id).ok_or(MutationError::NotFound)
    }

    fn child(&self, parent: u32, name: &[u8]) -> Option<&CatalogEntry>
    {
        self.entries.iter().find(|e| e.parent == parent && names_equal(&e.name, name))
    }

    fn below(&self, id: u32, ancestor: u32) -> MutationResult<bool>
    {
        let mut current = id;
        for _ in 0..=self.entries.len()
        {
            if current == ancestor
            {
                return Ok(true);
            }
            if current == 2
            {
                return Ok(false);
            }
            current = self.entries[self.index(current)?].parent;
        }
        Err(MutationError::Io)
    }

    fn record_index(&self, id: u32) -> MutationResult<(usize, usize)>
    {
        for (i, r) in self.tree.records.iter().enumerate()
        {
            if parse_entry(r).map_err(|_| MutationError::Io)?.is_some_and(|e| e.id == id)
            {
                return Ok((i, key(r)?.2));
            }
        }
        Err(MutationError::NotFound)
    }

    fn add_folder(&mut self, parent: u32, name: &[u8], when: u32, flags: u16)
                  -> MutationResult<u32>
    {
        validate_name(name)?;
        if !self.entries[self.index(parent)?].directory
        {
            return Err(MutationError::Invalid);
        }
        if self.child(parent, name).is_some()
        {
            return Err(MutationError::Exists);
        }
        let id = long(&self.mdb, 30)?;
        if id < 16 || self.entries.iter().any(|e| e.id == id)
        {
            return Err(MutationError::Unsupported);
        }
        put32(&mut self.mdb, 30, id.checked_add(1).ok_or(MutationError::Full)?);
        let mut data = [0; 70];
        data[0] = 1;
        put32(&mut data, 6, id);
        put32(&mut data, 10, when);
        put32(&mut data, 14, when);
        put16(&mut data, 30, flags);
        let r = record(parent, name, &data)?;
        reserve(&mut self.entries, 1)?;
        self.entries
            .push(parse_entry(&r).map_err(|_| MutationError::Io)?.ok_or(MutationError::Io)?);
        reserve(&mut self.tree.records, 2)?;
        self.tree.records.push(r);
        let mut thread = [0; 46];
        thread[0] = 3;
        put32(&mut thread, 10, parent);
        thread[14] = name.len() as u8;
        thread[15..15 + name.len()].copy_from_slice(name);
        self.tree.records.push(record(id, &[], &thread)?);
        Ok(id)
    }

    fn move_entry(&mut self, id: u32, parent: u32, put_away: u32) -> MutationResult<()>
    {
        let i = self.index(id)?;
        let e = &self.entries[i];
        if self.below(parent, id)? || !self.entries[self.index(parent)?].directory
        {
            return Err(MutationError::Invalid);
        }
        if self.child(parent, &e.name).is_some_and(|other| other.id != id)
        {
            return Err(MutationError::Exists);
        }
        let (r, start) = self.record_index(id)?;
        let mut data = copy(&self.tree.records[r][start..])?;
        let offset = if e.directory { 50 } else { 68 };
        put32(&mut data, offset, put_away);
        self.tree.records[r] = record(parent, &e.name, &data)?;
        for thread in &mut self.tree.records
        {
            let (p, n, start) = key(thread)?;
            if p == id && n.is_empty()
            {
                if thread.len() < start + 46 || !matches!(thread[start], 3 | 4)
                {
                    return Err(MutationError::Io);
                }
                put32(thread, start + 10, parent);
                thread[start + 14] = e.name.len() as u8;
                thread[start + 15..start + 46].fill(0);
                thread[start + 15..start + 15 + e.name.len()].copy_from_slice(&e.name);
            }
        }
        self.entries[i].parent = parent;
        put32(&mut self.entries[i].finder_info, 28, put_away);
        Ok(())
    }

    fn rename_entry(&mut self, id: u32, name: &[u8]) -> MutationResult<()>
    {
        validate_name(name)?;
        let index = self.index(id)?;
        let parent = self.entries[index].parent;
        let directory = self.entries[index].directory;
        if self.child(parent, name).is_some_and(|other| other.id != id)
        {
            return Err(MutationError::Exists);
        }
        let (record_index, start) = self.record_index(id)?;
        let data = copy(&self.tree.records[record_index][start..])?;
        self.tree.records[record_index] = record(parent, name, &data)?;
        let mut found_thread = false;
        for thread in &mut self.tree.records
        {
            let (thread_id, thread_name, start) = key(thread)?;
            if thread_id == id && thread_name.is_empty()
            {
                if found_thread
                   || thread.len() < start + 46
                   || thread[start] != if directory { 3 } else { 4 }
                {
                    return Err(MutationError::Io);
                }
                found_thread = true;
                put32(thread, start + 10, parent);
                thread[start + 14] = name.len() as u8;
                thread[start + 15..start + 46].fill(0);
                thread[start + 15..start + 15 + name.len()].copy_from_slice(name);
            }
        }
        // Folder threads are required by classic HFS; file threads are optional.
        if directory && !found_thread
        {
            return Err(MutationError::Io);
        }
        self.entries[index].name = copy(name)?;
        Ok(())
    }

    fn counts(&mut self, when: u32, touched: &[u32]) -> MutationResult<()>
    {
        for i in 0..self.entries.len()
        {
            if !self.entries[i].directory
            {
                continue;
            }
            let id = self.entries[i].id;
            let count = self.entries.iter().filter(|e| e.parent == id && e.id != 2).count();
            let count = u16::try_from(count).map_err(|_| MutationError::Full)?;
            let (r, start) = self.record_index(id)?;
            put16(&mut self.tree.records[r], start + 4, count);
            if touched.contains(&id)
            {
                put32(&mut self.tree.records[r], start + 14, when);
            }
        }
        put16(&mut self.mdb,12,u16::try_from(self.entries.iter().filter(|e|!e.directory&&e.parent==2).count()).map_err(|_|MutationError::Full)?);
        put16(&mut self.mdb,
              82,
              u16::try_from(self.entries
                                .iter()
                                .filter(|e| e.directory && e.parent == 2 && e.id != 2)
                                .count()).map_err(|_| MutationError::Full)?);
        put32(&mut self.mdb, 84, self.entries.iter().filter(|e| !e.directory).count() as u32);
        put32(&mut self.mdb,
              88,
              self.entries.iter().filter(|e| e.directory && e.id != 2).count() as u32);
        put32(&mut self.mdb, 6, when);
        let writes = long(&self.mdb, 70)?;
        put32(&mut self.mdb, 70, writes.wrapping_add(1));
        Ok(())
    }
}
fn validate_name(name: &[u8]) -> MutationResult<()>
{
    if name.is_empty()
       || name.len() > 31
       || name.contains(&0)
       || name.contains(&b':')
       || name == b"."
       || name == b".."
    {
        Err(MutationError::Invalid)
    }
    else
    {
        Ok(())
    }
}
impl<'a, D: WritableBlockDevice> Volume<'a, D>
{
    fn writable(&self) -> MutationResult<()>
    {
        if self.disk.is_read_only() || self.attributes & 0x8080 != 0
        {
            return Err(MutationError::ReadOnly);
        }
        if self.attributes & 0x0100 == 0
        {
            return Err(MutationError::Unsupported);
        }
        Ok(())
    }

    fn stage_tree(&mut self, tx: &mut Transaction, fork: &Fork, tree: &mut Tree, catalog: bool)
                  -> MutationResult<()>
    {
        let nodes = build_tree(tree, catalog, false)?;
        for (i, node) in nodes.into_iter().enumerate()
        {
            let sector = self.mutation_sector(fork, i * 512)?;
            tx.stage(self.disk, sector, node, 0)?;
        }
        Ok(())
    }

    fn finish_mutation(&mut self, cat: Catalog, when: u32, touched: &[u32], freed: &[u16],
                       overflow: Option<Tree>)
                       -> MutationResult<()>
    {
        self.finish_mutation_staged(cat, when, touched, freed, overflow, &[])
    }

    fn finish_mutation_staged(&mut self, mut cat: Catalog, when: u32, touched: &[u32],
                              freed: &[u16], overflow: Option<Tree>,
                              additional: &[(u64, [u8; 512], u8)])
                              -> MutationResult<()>
    {
        self.writable()?;
        cat.counts(when, touched)?;
        let mut tx = Transaction::new();
        for (sector, bytes, phase) in additional
        {
            tx.stage(self.disk, *sector, *bytes, *phase)?;
        }
        let fork = self.catalog.clone();
        self.stage_tree(&mut tx, &fork, &mut cat.tree, true)?;
        if let Some(mut tree) = overflow
        {
            let fork = self.overflow.clone();
            self.stage_tree(&mut tx, &fork, &mut tree, false)?;
        }
        if !freed.is_empty()
        {
            let start = word(&cat.mdb, 14)? as u64;
            let length = (self.allocation_count as usize).div_ceil(8);
            if start < 3 || start + (length.div_ceil(512) as u64) > self.allocation_start / 512
            {
                return Err(MutationError::Unsupported);
            }
            let mut bitmap = Vec::new();
            reserve(&mut bitmap, length.div_ceil(512) * 512)?;
            bitmap.resize(length.div_ceil(512) * 512, 0);
            read_at(self.disk,(self.partition.start+start)*512,&mut bitmap).map_err(|_|MutationError::Io)?;
            let actual = (0..self.allocation_count as usize).filter(|&b| {
                                                                bitmap[b / 8] & (0x80 >> (b % 8))
                                                                == 0
                                                            })
                                                            .count();
            if actual != word(&cat.mdb, 34)? as usize
            {
                return Err(MutationError::Io);
            }
            for &block in freed
            {
                let b = block as usize;
                if b >= self.allocation_count as usize || bitmap[b / 8] & (0x80 >> (b % 8)) == 0
                {
                    return Err(MutationError::Io);
                }
                bitmap[b / 8] &= !(0x80 >> (b % 8));
            }
            for (i, data) in bitmap.as_chunks::<512>().0.iter().enumerate()
            {
                tx.stage(self.disk, self.partition.start + start + i as u64, *data, 1)?;
            }
            put16(&mut cat.mdb,
                  34,
                  u16::try_from(actual + freed.len()).map_err(|_| MutationError::Full)?);
        }
        let mut alternate = [0; 512];
        let alt = self.partition.start + self.partition.sectors - 2;
        self.disk.read_sector(alt, &mut alternate).map_err(|_| MutationError::Io)?;
        if &alternate[..2] != b"BD"
        {
            return Err(MutationError::Unsupported);
        }
        tx.stage(self.disk, self.partition.start + 2, cat.mdb, 2)?;
        tx.stage(self.disk, alt, cat.mdb, 2)?;
        tx.commit(self.disk)?;
        self.modified = when;
        self.free_bytes = word(&cat.mdb, 34)? as u64 * self.allocation_size;
        Ok(())
    }

    fn protection(&self, cat: &Catalog, id: u32, protected: &[u32]) -> MutationResult<()>
    {
        if id == 2 || id == self.blessed_id
        {
            return Err(MutationError::Protected);
        }
        let item = &cat.entries[cat.index(id)?];
        if item.parent == self.blessed_id
           && (names_equal(&item.name, b"System") || names_equal(&item.name, b"Finder"))
        {
            return Err(MutationError::Protected);
        }
        if self.blessed_id >= 2 && cat.below(self.blessed_id, id)?
        {
            return Err(MutationError::Protected);
        }
        for &p in protected
        {
            if cat.below(p, id)?
            {
                return Err(MutationError::Busy);
            }
        }
        Ok(())
    }

    pub fn create_folder(&mut self, parent: u32, name: &[u8], when: u32)
                         -> MutationResult<CatalogEntry>
    {
        self.writable()?;
        let mut cat = self.mutation_catalog()?;
        let id = cat.add_folder(parent, name, when, 0)?;
        let entry = cat.entries[cat.index(id)?].clone();
        self.finish_mutation(cat, when, &[parent], &[], None)?;
        Ok(entry)
    }

    pub fn trash_directory(&mut self, create: bool, when: u32) -> MutationResult<CatalogEntry>
    {
        if let Some(e) = self.lookup(2, b"Trash").map_err(|_| MutationError::Io)?
        {
            return if e.directory { Ok(e) } else { Err(MutationError::Exists) };
        }
        if !create
        {
            return Err(MutationError::NotFound);
        }
        self.writable()?;
        let mut cat = self.mutation_catalog()?;
        let id = cat.add_folder(2, b"Trash", when, 0x1000)?;
        let entry = cat.entries[cat.index(id)?].clone();
        self.finish_mutation(cat, when, &[2], &[], None)?;
        Ok(entry)
    }

    /// Rename the catalog key and any existing thread, preserving CNID, raw
    /// metadata, dates and both forks. The parent directory's change date advances.
    pub fn rename(&mut self, id: u32, name: &[u8], when: u32, protected: &[u32])
                  -> MutationResult<CatalogEntry>
    {
        self.writable()?;
        validate_name(name)?;
        let mut cat = self.mutation_catalog()?;
        self.protection(&cat, id, protected)?;
        let index = cat.index(id)?;
        let entry = &cat.entries[index];
        if entry.flags & 1 != 0 || word(&entry.finder_info, 8)? & 0x1000 != 0
        {
            return Err(MutationError::Protected);
        }
        if entry.name == name
        {
            return Ok(entry.clone());
        }
        let parent = entry.parent;
        cat.rename_entry(id, name)?;
        let entry = cat.entries[index].clone();
        self.finish_mutation(cat, when, &[parent], &[], None)?;
        Ok(entry)
    }

    /// File catalog lock, independent of Finder flags. Clearing a lock is allowed.
    pub fn set_locked(&mut self, id: u32, locked: bool, when: u32, protected: &[u32])
                      -> MutationResult<CatalogEntry>
    {
        self.set_file_flag(id, locked, false, when, protected)
    }

    /// kIsStationery is a file-only Finder flag; unrelated metadata is retained.
    /// Source: Apple TN1150 Finder Info, kIsStationery0x0800.
    pub fn set_stationery(&mut self, id: u32, stationery: bool, when: u32, protected: &[u32])
                          -> MutationResult<CatalogEntry>
    {
        self.set_file_flag(id, stationery, true, when, protected)
    }

    fn set_file_flag(&mut self, id: u32, value: bool, stationery: bool, when: u32,
                     protected: &[u32])
                     -> MutationResult<CatalogEntry>
    {
        self.writable()?;
        let mut cat = self.mutation_catalog()?;
        self.protection(&cat, id, protected)?;
        let index = cat.index(id)?;
        if cat.entries[index].directory
        {
            return Err(MutationError::Invalid);
        }
        let (record_index, start) = cat.record_index(id)?;
        if stationery
        {
            if cat.entries[index].flags & 1 != 0
            {
                return Err(MutationError::Protected);
            }
            let old = word(&cat.entries[index].finder_info, 8)?;
            let updated = if value { old | 0x0800 } else { old & !0x0800 };
            if old == updated
            {
                return Ok(cat.entries[index].clone());
            }
            put16(&mut cat.entries[index].finder_info, 8, updated);
            put16(&mut cat.tree.records[record_index], start + 12, updated);
        }
        else
        {
            let old = cat.entries[index].flags;
            let updated = if value { old | 1 } else { old & !1 };
            if old == updated
            {
                return Ok(cat.entries[index].clone());
            }
            cat.entries[index].flags = updated;
            cat.tree.records[record_index][start + 2] = updated as u8;
        }
        let entry = cat.entries[index].clone();
        self.finish_mutation(cat, when, &[], &[], None)?;
        Ok(entry)
    }

    pub fn move_to_trash(&mut self, id: u32, when: u32, protected: &[u32])
                         -> MutationResult<CatalogEntry>
    {
        self.writable()?;
        let mut cat = self.mutation_catalog()?;
        self.protection(&cat, id, protected)?;
        let e = &cat.entries[cat.index(id)?];
        if e.flags & 1 != 0
        {
            return Err(MutationError::Protected);
        }
        let parent = e.parent;
        let trash = match cat.child(2, b"Trash")
        {
            Some(e) if e.directory => e.id,
            Some(_) => return Err(MutationError::Exists),
            None => cat.add_folder(2, b"Trash", when, 0x1000)?
        };
        if id == trash
        {
            return Err(MutationError::Protected);
        }
        if parent == trash
        {
            return Ok(cat.entries[cat.index(id)?].clone());
        }
        cat.move_entry(id, trash, parent)?;
        let entry = cat.entries[cat.index(id)?].clone();
        self.finish_mutation(cat, when, &[parent, trash, 2], &[], None)?;
        Ok(entry)
    }

    pub fn put_away(&mut self, id: u32, when: u32, protected: &[u32])
                    -> MutationResult<CatalogEntry>
    {
        self.writable()?;
        let mut cat = self.mutation_catalog()?;
        self.protection(&cat, id, protected)?;
        let trash = cat.child(2, b"Trash").ok_or(MutationError::NotFound)?.id;
        let e = &cat.entries[cat.index(id)?];
        if e.parent != trash
        {
            return Err(MutationError::Invalid);
        }
        let parent = long(&e.finder_info, 28)?;
        if parent < 2
        {
            return Err(MutationError::NotFound);
        }
        cat.move_entry(id, parent, parent)?;
        let entry = cat.entries[cat.index(id)?].clone();
        self.finish_mutation(cat, when, &[parent, trash], &[], None)?;
        Ok(entry)
    }

    pub fn empty_trash(&mut self, when: u32, protected: &[u32]) -> MutationResult<u32>
    {
        self.writable()?;
        let mut cat = self.mutation_catalog()?;
        let Some(trash) = cat.child(2, b"Trash")
        else
        {
            return Ok(0);
        };
        if !trash.directory
        {
            return Err(MutationError::Exists);
        }
        let trash = trash.id;
        let mut removed = Vec::new();
        reserve(&mut removed, cat.entries.len())?;
        for e in &cat.entries
        {
            if e.id != trash && cat.below(e.id, trash)?
            {
                self.protection(&cat, e.id, protected)?;
                if e.flags & 1 != 0
                {
                    return Err(MutationError::Protected);
                }
                removed.push(e.id);
            }
        }
        if removed.is_empty()
        {
            return Ok(0);
        }
        let mut overflow = if self.overflow.length != 0
        {
            let fork = self.overflow.clone();
            Some(self.mutation_tree(&fork)?)
        }
        else
        {
            None
        };
        for e in &cat.entries
        {
            if e.directory
            {
                continue;
            }
            let (r, start) = cat.record_index(e.id)?;
            let data = &cat.tree.records[r][start..];
            for (mut fork, physical) in
                [(e.data.clone().ok_or(MutationError::Io)?, long(data, 30)?),
                 (e.resources.clone().ok_or(MutationError::Io)?, long(data, 40)?)]
            {
                if (physical as u64) < fork.length
                   || !(physical as u64).is_multiple_of(self.allocation_size)
                {
                    return Err(MutationError::Io);
                }
                fork.length = physical as u64;
                self.resolve_extents(&mut fork).map_err(|_| MutationError::Io)?;
                if fork.blocks() * self.allocation_size != physical as u64
                {
                    return Err(MutationError::Io);
                }
            }
        }
        let mut allocated = Vec::new();
        reserve(&mut allocated, self.allocation_count as usize)?;
        allocated.resize(self.allocation_count as usize, false);
        let mut freed = Vec::new();
        reserve(&mut freed, self.allocation_count as usize)?;
        let mut mark = |start: u16, count: u16, release: bool| -> MutationResult<()> {
            let end = start as usize + count as usize;
            if end > allocated.len()
            {
                return Err(MutationError::Io);
            }
            for (b, used) in allocated.iter_mut().enumerate().take(end).skip(start as usize)
            {
                if *used
                {
                    return Err(MutationError::Io);
                }
                *used = true;
                if release
                {
                    freed.push(b as u16);
                }
            }
            Ok(())
        };
        for f in [&self.catalog, &self.overflow]
        {
            for e in &f.extents
            {
                mark(e.start, e.count, false)?;
            }
        }
        for e in &cat.entries
        {
            for f in [&e.data, &e.resources].into_iter().flatten()
            {
                for extent in &f.extents
                {
                    mark(extent.start, extent.count, removed.contains(&e.id))?;
                }
            }
        }
        if let Some(tree) = &mut overflow
        {
            for r in &tree.records
            {
                if r.len() != 20 || r[0] != 7 || !matches!(r[1], 0 | 0xff)
                {
                    return Err(MutationError::Io);
                }
                let id = long(r, 2)?;
                if id == 3 || id == 4
                {
                    continue;
                }
                for i in 0..3
                {
                    mark(word(r, 8 + i * 4)?, word(r, 10 + i * 4)?, removed.contains(&id))?;
                }
            }
            tree.records.retain(|r| !removed.contains(&long(r, 2).unwrap()));
        }
        cat.tree.records.retain(|r| {
                            let (p, _, _) = key(r).unwrap();
                            let id = parse_entry(r).unwrap().map(|e| e.id).unwrap_or(p);
                            !removed.contains(&id)
                        });
        cat.entries.retain(|e| !removed.contains(&e.id));
        let count = removed.len() as u32;
        self.finish_mutation(cat, when, &[trash], &freed, overflow)?;
        Ok(count)
    }
}
