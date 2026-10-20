//! Bounded-path B+ tree updates. No directory-wide collection or tree rebuild.
use alloc::vec::Vec;

use crate::{Error, Result, copy, layout::*};
pub(crate) const MAX_HEIGHT: u8 = 16;
pub(crate) const MAX_KEY: usize = 1536;
pub(crate) const MAX_VALUE: usize = 1024;
pub(crate) type Page = [u8; BLOCK_SIZE];
pub(crate) trait Store
{
    fn read_page(&mut self, id: u32) -> Result<Page>;
    fn write_page(&mut self, id: u32, page: Page) -> Result<()>;
    fn allocate(&mut self) -> Result<u32>;
    fn release(&mut self, id: u32) -> Result<()>;
}
pub(crate) struct Record
{
    pub key:   Vec<u8>,
    order:     Vec<u8>,
    pub value: Vec<u8>
}
impl Record
{
    fn new(kind: u8, key: &[u8], value: &[u8]) -> Result<Self>
    {
        Ok(Self { key:   copy(key)?,
                  order: order_key(kind, key)?,
                  value: copy(value)? })
    }
}
/// Comparison keys are transient. Disk keys retain NFC display spelling.
fn order_key(kind: u8, key: &[u8]) -> Result<Vec<u8>>
{
    if kind == 0 && key.first() == Some(&1) && key.len() >= 5
    {
        let name = core::str::from_utf8(&key[5..]).map_err(|_| Error::Corrupt)?;
        let folded = crate::names::comparison(name)?;
        let mut out = copy(&key[..5])?;
        out.try_reserve(folded.len()).map_err(|_| Error::NoMemory)?;
        out.extend_from_slice(folded.as_bytes());
        Ok(out)
    }
    else
    {
        copy(key)
    }
}
pub(crate) struct Node
{
    pub kind:    u8,
    pub level:   u8,
    pub records: Vec<Record>
}
impl Node
{
    pub fn empty(kind: u8, level: u8) -> Self
    {
        Self { kind,
               level,
               records: Vec::new() }
    }

    pub fn size(&self) -> usize
    {
        34 + self.records.iter().map(|r| 6 + r.key.len() + r.value.len()).sum::<usize>()
    }

    pub fn encode(&self, id: u32) -> Result<Page>
    {
        if self.size() > 4092 || self.level >= MAX_HEIGHT
        {
            return Err(Error::Budget);
        }
        let mut b = [0; BLOCK_SIZE];
        b[..4].copy_from_slice(b"HBTN");
        b[4] = self.kind;
        b[5] = self.level;
        put16(&mut b, 6, self.records.len() as u16);
        put32(&mut b, 12, id);
        let mut at = 32;
        for (i, r) in self.records.iter().enumerate()
        {
            put16(&mut b, 4090 - i * 2, at as u16);
            put16(&mut b, at, r.key.len() as u16);
            put16(&mut b, at + 2, r.value.len() as u16);
            b[at + 4..at + 4 + r.key.len()].copy_from_slice(&r.key);
            at += 4 + r.key.len();
            b[at..at + r.value.len()].copy_from_slice(&r.value);
            at += r.value.len();
        }
        put16(&mut b, 4090 - self.records.len() * 2, at as u16);
        let checksum = crc32(&b[..4092]);
        put32(&mut b, 4092, checksum);
        Ok(b)
    }

    pub fn decode(b: &Page, id: u32, kind: u8) -> Result<Self>
    {
        let count = u16_at(b, 6)? as usize;
        if &b[..4] != b"HBTN"
           || b[4] != kind
           || b[5] >= MAX_HEIGHT
           || u32_at(b, 12)? != id
           || b[8..12].iter().chain(&b[16..32]).any(|&n| n != 0)
           || crc32(&b[..4092]) != u32_at(b, 4092)?
           || count > 676
           || u16_at(b, 4090)? != 32
           || (b[5] != 0 && count == 0)
        {
            return Err(Error::Corrupt);
        }
        let end = 4092 - (count + 1) * 2;
        let mut n = Self::empty(kind, b[5]);
        n.records.try_reserve_exact(count).map_err(|_| Error::NoMemory)?;
        for i in 0..count
        {
            let at = u16_at(b, 4090 - i * 2)? as usize;
            let next = u16_at(b, 4088 - i * 2)? as usize;
            if at < 32 || next <= at || next > end || next - at < 4
            {
                return Err(Error::Corrupt);
            }
            let k = u16_at(b, at)? as usize;
            let v = u16_at(b, at + 2)? as usize;
            if k == 0
               || k > MAX_KEY
               || v > MAX_VALUE
               || at + 4 + k + v != next
               || (n.level != 0 && v != 4)
            {
                return Err(Error::Corrupt);
            }
            let key = &b[at + 4..at + 4 + k];
            if kind == 0
            {
                if key[0] == 0 && key.len() == 5
                {
                }
                else if key[0] == 1 && (6..=260).contains(&key.len())
                {
                    let name = core::str::from_utf8(&key[5..]).map_err(|_| Error::Corrupt)?;
                    if crate::names::normalize(name)? != name
                    {
                        return Err(Error::Corrupt);
                    }
                }
                else
                {
                    return Err(Error::Corrupt);
                }
            }
            let record = Record::new(kind, key, &b[at + 4 + k..next])?;
            if n.records.last().is_some_and(|r| r.order >= record.order)
            {
                return Err(Error::Corrupt);
            }
            n.records.push(record);
        }
        let used = u16_at(b, 4090 - count * 2)? as usize;
        if used > end || b[used..end].iter().any(|&n| n != 0)
        {
            return Err(Error::Corrupt);
        }
        Ok(n)
    }

    fn child(&self, key: &[u8]) -> usize
    {
        self.records.partition_point(|r| r.order.as_slice() <= key).saturating_sub(1)
    }
}
fn node(s: &mut impl Store, id: u32, kind: u8, expected: Option<u8>) -> Result<Node>
{
    let n = Node::decode(&s.read_page(id)?, id, kind)?;
    if expected.is_some_and(|v| n.level != v)
    {
        return Err(Error::Corrupt);
    }
    Ok(n)
}
pub(crate) fn get(s: &mut impl Store, id: u32, kind: u8, key: &[u8]) -> Result<Option<Vec<u8>>>
{
    Ok(get_record(s, id, kind, key)?.map(|record| record.value))
}
pub(crate) fn get_record(s: &mut impl Store, mut id: u32, kind: u8, key: &[u8])
                         -> Result<Option<Record>>
{
    let key = order_key(kind, key)?;
    let mut expected = None;
    for _ in 0..MAX_HEIGHT
    {
        let n = node(s, id, kind, expected)?;
        if n.level == 0
        {
            let mut records = n.records;
            return Ok(records.binary_search_by(|r| r.order.cmp(&key))
                             .ok()
                             .map(|i| records.swap_remove(i)));
        }
        id = u32_at(&n.records[n.child(&key)].value, 0)?;
        expected = Some(n.level - 1);
    }
    Err(Error::Corrupt)
}
/// First record >= key, or > key if exclusive. At most two root-to-leaf paths.
pub(crate) fn seek(s: &mut impl Store, id: u32, kind: u8, key: &[u8], exclusive: bool)
                   -> Result<Option<Record>>
{
    seek_at(s, id, kind, &order_key(kind, key)?, exclusive, None, 0)
}
fn seek_at(s: &mut impl Store, id: u32, kind: u8, key: &[u8], exclusive: bool,
           expected: Option<u8>, depth: u8)
           -> Result<Option<Record>>
{
    if depth >= MAX_HEIGHT
    {
        return Err(Error::Corrupt);
    }
    let n = node(s, id, kind, expected)?;
    if n.level == 0
    {
        return Ok(n.records
                   .into_iter()
                   .find(|r| r.order.as_slice() > key || (!exclusive && r.order == key)));
    }
    let i = n.child(key);
    if let Some(r) = seek_at(s,
                             u32_at(&n.records[i].value, 0)?,
                             kind,
                             key,
                             exclusive,
                             Some(n.level - 1),
                             depth + 1)?
    {
        return Ok(Some(r));
    }
    if let Some(r) = n.records.get(i + 1)
    {
        return seek_at(s, u32_at(&r.value, 0)?, kind, &[], false, Some(n.level - 1), depth + 1);
    }
    Ok(None)
}
struct Split
{
    key: Vec<u8>,
    id:  u32
}
#[derive(Clone, Copy)]
struct Update<'a>
{
    key:   &'a [u8],
    order: &'a [u8],
    value: &'a [u8]
}
pub(crate) fn put(s: &mut impl Store, root: u32, kind: u8, key: &[u8], value: &[u8])
                  -> Result<u32>
{
    if key.is_empty() || key.len() > MAX_KEY || value.len() > MAX_VALUE
    {
        return Err(Error::Invalid);
    }
    let level = node(s, root, kind, None)?.level;
    let order = order_key(kind, key)?;
    let update = Update { key,
                          order: &order,
                          value };
    let (minimum, split) = insert(s, root, kind, update, level, 0)?;
    if let Some(split) = split
    {
        if level + 1 >= MAX_HEIGHT
        {
            return Err(Error::Budget);
        }
        let id = s.allocate()?;
        let mut n = Node::empty(kind, level + 1);
        n.records.try_reserve_exact(2).map_err(|_| Error::NoMemory)?;
        n.records.push(Record::new(kind, &minimum, &root.to_be_bytes())?);
        n.records.push(Record::new(kind, &split.key, &split.id.to_be_bytes())?);
        s.write_page(id, n.encode(id)?)?;
        Ok(id)
    }
    else
    {
        Ok(root)
    }
}
fn insert(s: &mut impl Store, id: u32, kind: u8, update: Update<'_>, level: u8, depth: u8)
          -> Result<(Vec<u8>, Option<Split>)>
{
    if depth >= MAX_HEIGHT
    {
        return Err(Error::Corrupt);
    }
    let mut n = node(s, id, kind, Some(level))?;
    let Update { key,
                 order,
                 value } = update;
    if level == 0
    {
        match n.records.binary_search_by(|r| r.order.as_slice().cmp(order))
        {
            Ok(i) => n.records[i].value = copy(value)?,
            Err(i) =>
            {
                n.records.try_reserve(1).map_err(|_| Error::NoMemory)?;
                n.records.insert(i,
                                 Record { key:   copy(key)?,
                                          order: copy(order)?,
                                          value: copy(value)? });
            }
        }
    }
    else
    {
        let i = n.child(order);
        let child = u32_at(&n.records[i].value, 0)?;
        let (minimum, split) = insert(s, child, kind, update, level - 1, depth + 1)?;
        n.records[i].order = order_key(kind, &minimum)?;
        n.records[i].key = minimum;
        if let Some(split) = split
        {
            n.records.try_reserve(1).map_err(|_| Error::NoMemory)?;
            n.records.insert(i + 1, Record::new(kind, &split.key, &split.id.to_be_bytes())?);
        }
    }
    let minimum = copy(&n.records[0].key)?;
    if n.size() <= 4092
    {
        s.write_page(id, n.encode(id)?)?;
        return Ok((minimum, None));
    }
    let total = n.size() - 34;
    let mut used = 0;
    let mut middle = 0;
    for (i, r) in n.records.iter().enumerate().take(n.records.len() - 1)
    {
        used += 6 + r.key.len() + r.value.len();
        if used + 34 <= 4092 && total - used + 34 <= 4092 && (middle == 0 || used <= total / 2)
        {
            middle = i + 1;
        }
    }
    if middle == 0
    {
        return Err(Error::Budget);
    }
    let mut right = Node::empty(kind, level);
    right.records.try_reserve_exact(n.records.len() - middle).map_err(|_| Error::NoMemory)?;
    right.records.extend(n.records.drain(middle..));
    let right_id = s.allocate()?;
    let key = copy(&right.records[0].key)?;
    s.write_page(right_id, right.encode(right_id)?)?;
    s.write_page(id, n.encode(id)?)?;
    Ok((minimum,
        Some(Split { key,
                     id: right_id })))
}
pub(crate) fn remove(s: &mut impl Store, root: u32, kind: u8, key: &[u8]) -> Result<()>
{
    let level = node(s, root, kind, None)?.level;
    delete(s, root, kind, &order_key(kind, key)?, level, 0)?;
    // Keep the stable root block, collapsing a single-child root in place.
    loop
    {
        let n = node(s, root, kind, None)?;
        if n.level == 0 || n.records.len() != 1
        {
            return Ok(());
        }
        let child = u32_at(&n.records[0].value, 0)?;
        let n = node(s, child, kind, Some(n.level - 1))?;
        s.write_page(root, n.encode(root)?)?;
        s.release(child)?;
    }
}
fn delete(s: &mut impl Store, id: u32, kind: u8, key: &[u8], level: u8, depth: u8) -> Result<bool>
{
    if depth >= MAX_HEIGHT
    {
        return Err(Error::Corrupt);
    }
    let mut n = node(s, id, kind, Some(level))?;
    if level == 0
    {
        let i = n.records
                 .binary_search_by(|r| r.order.as_slice().cmp(key))
                 .map_err(|_| Error::NotFound)?;
        n.records.remove(i);
    }
    else
    {
        let i = n.child(key);
        let child = u32_at(&n.records[i].value, 0)?;
        if delete(s, child, kind, key, level - 1, depth + 1)?
        {
            n.records.remove(i);
            s.release(child)?;
        }
    }
    let empty = n.records.is_empty();
    // An empty internal node will be removed by its parent. Its temporary page
    // is encoded as an empty leaf, also allowing a completely emptied root.
    if empty
    {
        n.level = 0;
    }
    s.write_page(id, n.encode(id)?)?;
    Ok(empty)
}
/// Full structural walk for offline validation. Normal lookup/update never
/// traverses the entire tree. The callback owns its explicit accounting budget.
pub(crate) fn walk(s: &mut impl Store, root: u32, kind: u8,
                   visit: &mut impl FnMut(u32, &Node) -> Result<()>)
                   -> Result<()>
{
    walk_at(s,
            root,
            kind,
            Bounds { expected: None,
                     lower:    None,
                     upper:    None,
                     depth:    0 },
            visit)
}
struct Bounds<'a>
{
    expected: Option<u8>,
    lower:    Option<&'a [u8]>,
    upper:    Option<&'a [u8]>,
    depth:    u8
}
fn walk_at(s: &mut impl Store, id: u32, kind: u8, bounds: Bounds<'_>,
           visit: &mut impl FnMut(u32, &Node) -> Result<()>)
           -> Result<()>
{
    let Bounds { expected,
                 lower,
                 upper,
                 depth } = bounds;
    if depth >= MAX_HEIGHT
    {
        return Err(Error::Corrupt);
    }
    let n = node(s, id, kind, expected)?;
    if n.records.iter().any(|r| {
                           lower.is_some_and(|lo| r.order.as_slice() < lo)
                           || upper.is_some_and(|hi| r.order.as_slice() >= hi)
                       })
    {
        return Err(Error::Corrupt);
    }
    visit(id, &n)?;
    if n.level != 0
    {
        for (i, r) in n.records.iter().enumerate()
        {
            let upper = n.records.get(i + 1).map(|r| r.order.as_slice()).or(upper);
            walk_at(s,
                    u32_at(&r.value, 0)?,
                    kind,
                    Bounds { expected: Some(n.level - 1),
                             lower: Some(&r.order),
                             upper,
                             depth: depth + 1 },
                    visit)?;
        }
    }
    Ok(())
}
