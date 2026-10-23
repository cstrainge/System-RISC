use alloc::{string::String, vec::Vec};

use crate::{copy,
            layout::*,
            names,
            time::Timestamp,
            tree::{self, Node, Page, Store},
            *};
mod writes;
mod moves;
mod stream;
pub use stream::{STAGING_DIRECTORY, StagedFile, StreamAbort, StreamProperties, StreamSpec};
pub use writes::DataSave;
mod check;
pub use check::CheckReport;

/// Explicit per-operation limits, independent of disk/file sizes.
#[derive(Clone, Copy, Debug)]
pub struct Budget
{
    pub metadata_pages: usize,
    pub io_bytes:       usize,
    pub extents:        usize
}
impl Default for Budget
{
    fn default() -> Self
    {
        Self { metadata_pages: 128,
               io_bytes:       1024 * 1024,
               extents:        4096 }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry
{
    pub id:          u32,
    pub parent:      u32,
    pub name:        String,
    pub directory:   bool,
    pub flags:       u16,
    pub children:    u32,
    pub created:     Timestamp,
    pub modified:    Timestamp,
    pub backup:      Option<Timestamp>,
    pub finder_info: [u8; 32],
    pub data:        Fork,
    pub resources:   Fork
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkKind
{
    Data,
    Resource
}
impl ForkKind
{
    fn index(self) -> u8
    {
        match self
        {
            Self::Data => 0,
            Self::Resource => 1
        }
    }
}
impl Entry
{
    pub fn fork(&self, kind: ForkKind) -> &Fork
    {
        match kind
        {
            ForkKind::Data => &self.data,
            ForkKind::Resource => &self.resources
        }
    }

    fn fork_mut(&mut self, kind: ForkKind) -> &mut Fork
    {
        match kind
        {
            ForkKind::Data => &mut self.data,
            ForkKind::Resource => &mut self.resources
        }
    }

    fn encode(&self) -> Result<Vec<u8>>
    {
        let mut b = Vec::new();
        b.try_reserve_exact(234 + self.name.len()).map_err(|_| Error::NoMemory)?;
        b.resize(234 + self.name.len(), 0);
        put32(&mut b, 0, self.id);
        put32(&mut b, 4, self.parent);
        put16(&mut b, 8, self.flags);
        b[10] = u8::from(self.directory);
        b[11] = u8::from(self.backup.is_some());
        put32(&mut b, 12, self.children);
        put64(&mut b, 16, self.created.0);
        put64(&mut b, 24, self.modified.0);
        put64(&mut b, 32, self.backup.unwrap_or_default().0);
        b[40..72].copy_from_slice(&self.finder_info);
        self.data.encode(&mut b[72..152]);
        self.resources.encode(&mut b[152..232]);
        put16(&mut b, 232, self.name.len() as u16);
        b[234..].copy_from_slice(self.name.as_bytes());
        Ok(b)
    }

    fn decode(b: &[u8], h: &Header) -> Result<Self>
    {
        let n = u16_at(b, 232)? as usize;
        if b.len() != 234 + n || n > 255 || b[10] > 1 || b[11] > 1
        {
            return Err(Error::Corrupt);
        }
        let raw = core::str::from_utf8(&b[234..]).map_err(|_| Error::Corrupt)?;
        let name = names::normalize(raw)?;
        if name != raw
        {
            return Err(Error::Corrupt);
        }
        let e = Self { id: u32_at(b, 0)?,
                       parent: u32_at(b, 4)?,
                       name,
                       directory: b[10] != 0,
                       flags: u16_at(b, 8)?,
                       children: u32_at(b, 12)?,
                       created: Timestamp(u64_at(b, 16)?),
                       modified: Timestamp(u64_at(b, 24)?),
                       backup: if b[11] != 0 { Some(Timestamp(u64_at(b, 32)?)) } else { None },
                       finder_info: b[40..72].try_into().unwrap(),
                       data: Fork::decode(&b[72..152], h)?,
                       resources: Fork::decode(&b[152..232], h)? };
        if (e.id != 2 && e.id < 16)
           || e.id >= h.next_id
           || e.parent == 0
           || (e.id == 2 && e.parent != 1)
           || (e.id != 2 && e.parent < 2)
           || (e.directory && (e.data != Fork::default() || e.resources != Fork::default()))
           || (!e.directory && e.children != 0)
           || (b[11] == 0 && u64_at(b, 32)? != 0)
        {
            return Err(Error::Corrupt);
        }
        Ok(e)
    }
}
fn id_key(id: u32) -> [u8; 5]
{
    let mut k = [0; 5];
    k[1..].copy_from_slice(&id.to_be_bytes());
    k
}
fn name_key(parent: u32, name: &str) -> Result<Vec<u8>>
{
    let normalized = names::normalize(name)?;
    let mut k = Vec::new();
    k.try_reserve_exact(5 + normalized.len()).map_err(|_| Error::NoMemory)?;
    k.push(1);
    k.extend_from_slice(&parent.to_be_bytes());
    k.extend_from_slice(normalized.as_bytes());
    if k.len() > tree::MAX_KEY
    {
        return Err(Error::Budget);
    }
    Ok(k)
}
fn overflow_key(id: u32, kind: ForkKind, logical: u64) -> [u8; 13]
{
    let mut k = [0; 13];
    k[..4].copy_from_slice(&id.to_be_bytes());
    k[4] = kind.index();
    k[5..].copy_from_slice(&logical.to_be_bytes());
    k
}
fn attr_key(id: u32, kind: u8) -> [u8; 5]
{
    let mut k = [0; 5];
    k[..4].copy_from_slice(&id.to_be_bytes());
    k[4] = kind;
    k
}
fn read_page(d: &mut impl BlockDevice, p: Partition, id: u32) -> Result<Page>
{
    if u64::from(id) >= p.sectors / 8
    {
        return Err(Error::Corrupt);
    }
    let mut page = [0; BLOCK_SIZE];
    for (i, chunk) in page.as_chunks_mut::<512>().0.iter_mut().enumerate()
    {
        d.read_sector(p.start + u64::from(id) * 8 + i as u64, chunk)?;
    }
    Ok(page)
}
fn write_page(d: &mut impl WritableBlockDevice, p: Partition, id: u32, page: &Page) -> Result<()>
{
    if u64::from(id) >= p.sectors / 8
    {
        return Err(Error::Corrupt);
    }
    for (i, chunk) in page.as_chunks::<512>().0.iter().enumerate()
    {
        d.write_sector(p.start + u64::from(id) * 8 + i as u64, chunk)?;
    }
    Ok(())
}
fn headers(d: &mut impl WritableBlockDevice, p: Partition, h: &Header) -> Result<()>
{
    let b = h.encode()?;
    d.write_sector(p.start + 2, &b)?;
    d.write_sector(p.start + p.sectors - 2, &b)?;
    d.flush()?;
    Ok(())
}

pub struct Volume<'a, D: BlockDevice>
{
    disk:      &'a mut D,
    partition: Partition,
    header:    Header,
    budget:    Budget
}
/// A bounded name-ordered scan snapshot. A mutation invalidates it explicitly.
pub struct CatalogScanner
{
    partition: Partition,
    revision:  u64,
    budget:    Budget
}
enum MoveOrigin
{
    Keep,
    Save,
    Clear
}
impl CatalogScanner
{
    pub fn next(&self, disk: &mut impl BlockDevice, parent: u32, after: Option<u32>)
                -> Result<Option<Entry>>
    {
        let mut volume = Volume::mount(disk, self.partition, self.budget)?;
        if volume.header.revision != self.revision
        {
            return Err(Error::Stale);
        }
        let bookmark = if let Some(id) = after
        {
            let e = volume.entry(id)?;
            if e.parent != parent
            {
                return Err(Error::Invalid);
            }
            Some(e.name)
        }
        else
        {
            None
        };
        volume.next_child(parent, bookmark.as_deref())
    }
}
impl<'a, D: BlockDevice> Volume<'a, D>
{
    pub fn header(&self) -> &Header
    {
        &self.header
    }

    pub fn catalog_scanner(&self) -> CatalogScanner
    {
        CatalogScanner { partition: self.partition,
                         revision:  self.header.revision,
                         budget:    self.budget }
    }

    pub fn mount(disk: &'a mut D, partition: Partition, budget: Budget) -> Result<Self>
    {
        partition.validate(disk)?;
        if partition.sectors < 128 || !partition.sectors.is_multiple_of(8)
        {
            return Err(Error::Invalid);
        }
        let mut b = [0; 512];
        disk.read_sector(partition.start + 2, &mut b)?;
        let header = Header::decode(&b)?;
        if header.sectors != partition.sectors
        {
            return Err(Error::Corrupt);
        }
        if header.flags != 0
        {
            return Err(Error::Dirty);
        }
        let mut alternate = [0; 512];
        disk.read_sector(partition.start + partition.sectors - 2, &mut alternate)?;
        if b != alternate
        {
            return Err(Error::Corrupt);
        }
        Ok(Self { disk,
                  partition,
                  header,
                  budget })
    }

    pub fn entry(&mut self, id: u32) -> Result<Entry>
    {
        let root = self.header.roots[0];
        let b = tree::get(self, root, 0, &id_key(id))?.ok_or(Error::NotFound)?;
        let e = Entry::decode(&b, &self.header)?;
        if e.id != id
        {
            return Err(Error::Corrupt);
        }
        Ok(e)
    }

    pub fn lookup(&mut self, parent: u32, name: &str) -> Result<Entry>
    {
        if !self.entry(parent)?.directory
        {
            return Err(Error::Invalid);
        }
        let key = name_key(parent, name)?;
        let root = self.header.roots[0];
        let record = tree::get_record(self, root, 0, &key)?.ok_or(Error::NotFound)?;
        let e = self.entry(u32_at(&record.value, 0)?)?;
        if record.value.len() != 4 || e.parent != parent || name_key(parent, &e.name)? != record.key
        {
            return Err(Error::Corrupt);
        }
        Ok(e)
    }

    /// One name-ordered child, with an exclusive normalized-name bookmark.
    pub fn next_child(&mut self, parent: u32, after: Option<&str>) -> Result<Option<Entry>>
    {
        if !self.entry(parent)?.directory
        {
            return Err(Error::Invalid);
        }
        let mut prefix = [1; 5];
        prefix[1..].copy_from_slice(&parent.to_be_bytes());
        let key = if let Some(name) = after { name_key(parent, name)? } else { copy(&prefix)? };
        let root = self.header.roots[0];
        let Some(r) = tree::seek(self, root, 0, &key, after.is_some())?
        else
        {
            return Ok(None);
        };
        if !r.key.starts_with(&prefix)
        {
            return Ok(None);
        }
        let e = self.entry(u32_at(&r.value, 0)?)?;
        if r.value.len() != 4 || name_key(parent, &e.name)? != r.key || e.parent != parent
        {
            return Err(Error::Corrupt);
        }
        Ok(Some(e))
    }

    pub fn child(&mut self, parent: u32, index: u32) -> Result<Option<Entry>>
    {
        let mut after = None;
        for i in 0..=index
        {
            let e = self.next_child(parent, after.as_deref())?;
            if i == index || e.is_none()
            {
                return Ok(e);
            }
            after = e.map(|e| e.name);
        }
        Ok(None)
    }

    pub fn blessed_folder(&mut self) -> Result<Entry>
    {
        let e = self.entry(self.header.blessed)?;
        if !e.directory || e.id == ROOT_ID
        {
            return Err(Error::Corrupt);
        }
        Ok(e)
    }

    fn extent_list(&mut self, id: u32, kind: ForkKind, fork: &Fork) -> Result<Vec<Extent>>
    {
        let mut extents = Vec::new();
        extents.try_reserve_exact(8).map_err(|_| Error::NoMemory)?;
        extents.extend(fork.extents.iter().filter(|e| e.count != 0).copied());
        let mut logical = extents.iter().map(|e| u64::from(e.count)).sum::<u64>();
        while logical < fork.blocks
        {
            if extents.len() >= self.budget.extents
            {
                return Err(Error::Budget);
            }
            let root = self.header.roots[1];
            let b =
                tree::get(self, root, 1, &overflow_key(id, kind, logical))?.ok_or(Error::Corrupt)?;
            let e = Extent { start: u32_at(&b, 0)?,
                             count: u32_at(&b, 4)? };
            if b.len() != 8
               || e.count == 0
               || !self.header.valid_data_block(e.start)
               || e.start.checked_add(e.count).is_none_or(|end| end > self.header.blocks - 1)
            {
                return Err(Error::Corrupt);
            }
            logical = logical.checked_add(u64::from(e.count)).ok_or(Error::Corrupt)?;
            extents.try_reserve(1).map_err(|_| Error::NoMemory)?;
            extents.push(e);
        }
        if logical != fork.blocks
        {
            return Err(Error::Corrupt);
        }
        for (i, e) in extents.iter().enumerate()
        {
            if extents[..i].iter().any(|v|v.start<u32::try_from(u64::from(e.start)+u64::from(e.count)).unwrap_or(u32::MAX)
                && e.start<v.start+v.count) {return Err(Error::Corrupt);}
        }
        Ok(extents)
    }

    pub fn read_fork(&mut self, id: u32, kind: ForkKind, offset: u64, output: &mut [u8])
                     -> Result<usize>
    {
        if output.len() > self.budget.io_bytes
        {
            return Err(Error::Budget);
        }
        let entry = self.entry(id)?;
        if entry.directory
        {
            return Err(Error::Invalid);
        }
        let fork = entry.fork(kind);
        if offset > fork.length
        {
            return Err(Error::Invalid);
        }
        let length = output.len().min((fork.length - offset).min(usize::MAX as u64) as usize);
        if length == 0
        {
            return Ok(0);
        }
        let extents = self.extent_list(id, kind, fork)?;
        let mut logical = 0u64;
        let mut done = 0usize;
        for e in extents
        {
            let end = logical + u64::from(e.count) * 4096;
            while offset + (done as u64) < end && done < length
            {
                let position = offset + done as u64;
                if position < logical
                {
                    return Err(Error::Corrupt);
                }
                let within = position - logical;
                let block = e.start + (within / 4096) as u32;
                self.allocated(block)?;
                let page = read_page(self.disk, self.partition, block)?;
                let at = (within % 4096) as usize;
                let n = (4096 - at).min(length - done);
                output[done..done + n].copy_from_slice(&page[at..at + n]);
                done += n;
            }
            logical = end;
            if done == length
            {
                return Ok(done);
            }
        }
        Err(Error::Corrupt)
    }

    fn allocated(&mut self, id: u32) -> Result<()>
    {
        if !self.header.valid_data_block(id)
        {
            return Err(Error::Corrupt);
        }
        let mut b = [0; 512];
        self.disk.read_sector(self.partition.start + 8 + u64::from(id / 4096), &mut b)?;
        if b[(id % 4096 / 8) as usize] & (0x80 >> (id % 8)) == 0
        {
            return Err(Error::Corrupt);
        }
        Ok(())
    }

    pub fn comment(&mut self, id: u32) -> Result<String>
    {
        self.entry(id)?;
        let root = self.header.roots[2];
        let b = tree::get(self, root, 2, &attr_key(id, 0))?.unwrap_or_default();
        String::from_utf8(b).map_err(|_| Error::Corrupt)
    }
}
impl<D: BlockDevice> Store for Volume<'_, D>
{
    fn read_page(&mut self, id: u32) -> Result<Page>
    {
        self.allocated(id)?;
        read_page(self.disk, self.partition, id)
    }

    fn write_page(&mut self, _: u32, _: Page) -> Result<()>
    {
        Err(Error::ReadOnly)
    }

    fn allocate(&mut self) -> Result<u32>
    {
        Err(Error::ReadOnly)
    }

    fn release(&mut self, _: u32) -> Result<()>
    {
        Err(Error::ReadOnly)
    }
}
struct Change
{
    id:  u32,
    old: Page,
    new: Page
}
struct Transaction<'v, 'd, D: WritableBlockDevice>
{
    volume:  &'v mut Volume<'d, D>,
    header:  Header,
    changes: Vec<Change>
}
impl<D: WritableBlockDevice> Transaction<'_, '_, D>
{
    fn page(&mut self, id: u32) -> Result<Page>
    {
        if let Some(c) = self.changes.iter().find(|c| c.id == id)
        {
            return Ok(c.new);
        }
        read_page(self.volume.disk, self.volume.partition, id)
    }

    fn stage(&mut self, id: u32, new: Page) -> Result<()>
    {
        if let Some(c) = self.changes.iter_mut().find(|c| c.id == id)
        {
            c.new = new;
            return Ok(());
        }
        if self.changes.len() >= self.volume.budget.metadata_pages
        {
            return Err(Error::Budget);
        }
        let old = read_page(self.volume.disk, self.volume.partition, id)?;
        self.changes.try_reserve(1).map_err(|_| Error::NoMemory)?;
        self.changes.push(Change { id,
                                   old,
                                   new });
        Ok(())
    }

    fn bit(&mut self, id: u32, value: Option<bool>) -> Result<bool>
    {
        if id >= self.header.blocks
        {
            return Err(Error::Corrupt);
        }
        let block = 1 + id / 32768;
        let mut page = self.page(block)?;
        let at = (id % 32768 / 8) as usize;
        let mask = 0x80 >> (id % 8);
        let old = page[at] & mask != 0;
        if let Some(value) = value
        {
            if value
            {
                page[at] |= mask;
            }
            else
            {
                page[at] &= !mask;
            }
            self.stage(block, page)?;
        }
        Ok(old)
    }

    fn put(&mut self, kind: usize, key: &[u8], value: &[u8]) -> Result<()>
    {
        let root = self.header.roots[kind];
        self.header.roots[kind] = tree::put(self, root, kind as u8, key, value)?;
        Ok(())
    }

    fn remove(&mut self, kind: usize, key: &[u8]) -> Result<()>
    {
        let root = self.header.roots[kind];
        tree::remove(self, root, kind as u8, key)
    }

    fn entry(&mut self, e: &Entry) -> Result<()>
    {
        self.put(0, &id_key(e.id), &e.encode()?)
    }

    fn commit(mut self) -> Result<()>
    {
        self.header.revision = self.header.revision.checked_add(1).ok_or(Error::Full)?;
        let old = &self.volume.header;
        let mut dirty = Header::decode(&old.encode()?)?;
        dirty.flags |= DIRTY;
        let result = (|| {
            headers(self.volume.disk, self.volume.partition, &dirty)?;
            for c in &self.changes
            {
                write_page(self.volume.disk, self.volume.partition, c.id, &c.new)?;
            }
            self.volume.disk.flush()?;
            headers(self.volume.disk, self.volume.partition, &self.header)
        })();
        if result.is_err()
        {
            let rollback = (|| {
                for c in &self.changes
                {
                    write_page(self.volume.disk, self.volume.partition, c.id, &c.old)?;
                }
                self.volume.disk.flush()?;
                headers(self.volume.disk, self.volume.partition, old)
            })();
            if rollback.is_err()
            {
                self.volume.header.flags |= DIRTY;
                let _ = headers(self.volume.disk, self.volume.partition, &dirty);
                return Err(Error::RollbackFailed);
            }
            return Err(Error::Io);
        }
        self.volume.header = self.header;
        Ok(())
    }
}
impl<D: WritableBlockDevice> Store for Transaction<'_, '_, D>
{
    fn read_page(&mut self, id: u32) -> Result<Page>
    {
        if !self.header.valid_data_block(id) || !self.bit(id, None)?
        {
            return Err(Error::Corrupt);
        }
        self.page(id)
    }

    fn write_page(&mut self, id: u32, page: Page) -> Result<()>
    {
        if !self.header.valid_data_block(id) || !self.bit(id, None)?
        {
            return Err(Error::Corrupt);
        }
        self.stage(id, page)
    }

    fn allocate(&mut self) -> Result<u32>
    {
        if self.header.free == 0
        {
            return Err(Error::Full);
        }
        let first = self.header.bitmap_blocks + 1;
        let count = self.header.blocks - 1 - first;
        let start = self.header.hint;
        // Read each bitmap page once while searching; no I/O per occupied bit.
        let mut cache = None;
        for step in 0..count
        {
            let id =
                first + ((u64::from(start - first) + u64::from(step)) % u64::from(count)) as u32;
            let page_id = 1 + id / 32768;
            if cache.as_ref().is_none_or(|(i, _)| *i != page_id)
            {
                cache = Some((page_id, self.page(page_id)?));
            }
            let page = &mut cache.as_mut().unwrap().1;
            let at = (id % 32768 / 8) as usize;
            let mask = 0x80 >> (id % 8);
            if page[at] & mask == 0
            {
                page[at] |= mask;
                let new = *page;
                self.stage(page_id, new)?;
                self.header.free -= 1;
                self.header.hint = if id + 1 == self.header.blocks - 1 { first } else { id + 1 };
                return Ok(id);
            }
        }
        Err(Error::Corrupt)
    }

    fn release(&mut self, id: u32) -> Result<()>
    {
        if !self.header.valid_data_block(id) || !self.bit(id, Some(false))?
        {
            return Err(Error::Corrupt);
        }
        self.header.free = self.header.free.checked_add(1).ok_or(Error::Corrupt)?;
        Ok(())
    }
}

impl<'a, D: WritableBlockDevice> Volume<'a, D>
{
    /// Mutations already flush their commit. Explicit sync exposes adapter
    /// durability failures without pretending that successful writes imply it.
    pub fn sync(&mut self) -> Result<()>
    {
        if self.header.flags != 0
        {
            return Err(Error::Dirty);
        }
        self.disk.flush()?;
        Ok(())
    }

    fn transaction(&mut self) -> Result<Transaction<'_, 'a, D>>
    {
        if self.header.flags != 0
        {
            return Err(Error::Dirty);
        }
        if self.disk.is_read_only()
        {
            return Err(Error::ReadOnly);
        }
        let header = Header::decode(&self.header.encode()?)?;
        Ok(Transaction { volume: self,
                         header,
                         changes: Vec::new() })
    }

    pub fn create(&mut self, parent: u32, name: &str, directory: bool, when: Timestamp)
                  -> Result<Entry>
    {
        self.create_with_finder_info(parent, name, directory, [0; 32], when)
    }

    /// Initial Finder metadata belongs to the same catalog publication. In
    /// particular a private staging directory must never be briefly visible.
    pub fn create_with_finder_info(&mut self, parent: u32, name: &str, directory: bool,
                                   finder_info: [u8; 32], when: Timestamp)
                                   -> Result<Entry>
    {
        let name = names::normalize(name)?;
        match self.lookup(parent, &name)
        {
            Err(Error::NotFound) =>
            {}
            Ok(_) => return Err(Error::Exists),
            Err(e) => return Err(e)
        }
        let mut folder = self.entry(parent)?;
        if !folder.directory
        {
            return Err(Error::Invalid);
        }
        if folder.flags & 1 != 0
        {
            return Err(Error::Protected);
        }
        let e = Entry { id: self.header.next_id,
                        parent,
                        name,
                        directory,
                        flags: 0,
                        children: 0,
                        created: when,
                        modified: when,
                        backup: None,
                        finder_info,
                        data: Fork::default(),
                        resources: Fork::default() };
        let mut tx = self.transaction()?;
        tx.header.next_id = tx.header.next_id.checked_add(1).ok_or(Error::Full)?;
        tx.entry(&e)?;
        tx.put(0, &name_key(parent, &e.name)?, &e.id.to_be_bytes())?;
        folder.children = folder.children.checked_add(1).ok_or(Error::Full)?;
        folder.modified = when;
        tx.entry(&folder)?;
        tx.header.modified = when.0;
        tx.commit()?;
        Ok(e)
    }

    pub fn bless(&mut self, id: u32, when: Timestamp) -> Result<()>
    {
        // Zero explicitly clears the blessing without invalidating checksums.
        if id != 0 && (!self.entry(id)?.directory || id == 2)
        {
            return Err(Error::Invalid);
        }
        let mut tx = self.transaction()?;
        tx.header.blessed = id;
        tx.header.modified = when.0;
        tx.commit()
    }

    /// Restore object dates verbatim. `when` timestamps the destination volume's
    /// catalog change independently, so importing an old file cannot backdate it.
    #[allow(clippy::too_many_arguments,
            reason = "verbatim fork metadata plus a separate catalog-change clock")]
    pub fn set_metadata(&mut self, id: u32, flags: u16, finder_info: [u8; 32], created: Timestamp,
                        modified: Timestamp, backup: Option<Timestamp>, when: Timestamp)
                        -> Result<Entry>
    {
        let mut e = self.entry(id)?;
        e.flags = flags;
        e.finder_info = finder_info;
        e.created = created;
        e.modified = modified;
        e.backup = backup;
        let mut tx = self.transaction()?;
        tx.entry(&e)?;
        tx.header.modified = when.0;
        tx.commit()?;
        Ok(e)
    }

    /// Restore only an owned transaction's affected folder date. A separate
    /// volume date restores the header without conflating it with root metadata.
    /// The resident coordinator serializes competing mutations while undoing.
    pub fn restore_modified(&mut self, id: u32, modified: Timestamp,
                            volume_modified: Option<Timestamp>)
                            -> Result<Entry>
    {
        let mut entry = self.entry(id)?;
        if entry.modified == modified
           && volume_modified.is_none_or(|date| date.0 == self.header.modified)
        {
            return Ok(entry);
        }
        entry.modified = modified;
        let mut tx = self.transaction()?;
        tx.entry(&entry)?;
        if let Some(date) = volume_modified
        {
            tx.header.modified = date.0;
        }
        tx.commit()?;
        Ok(entry)
    }

    /// Publish complete copied object properties in one metadata transaction.
    /// Folder contents and both fork extents remain untouched.
    pub fn set_properties(&mut self, id: u32, properties: StreamProperties<'_>, when: Timestamp)
                          -> Result<(Entry, bool)>
    {
        if properties.comment.len() > 255
        {
            return Err(Error::Invalid);
        }
        let mut entry = self.entry(id)?;
        let comment = self.comment(id)?;
        if entry.flags == properties.flags
           && entry.finder_info == properties.finder_info
           && entry.created == properties.created
           && entry.modified == properties.modified
           && entry.backup == properties.backup
           && comment == properties.comment
        {
            return Ok((entry, false));
        }
        let unlocking = properties.flags == entry.flags & !1
                        && entry.finder_info == properties.finder_info
                        && entry.created == properties.created
                        && entry.modified == properties.modified
                        && entry.backup == properties.backup
                        && comment == properties.comment;
        if entry.flags & 1 != 0 && !unlocking
        {
            return Err(Error::Protected);
        }
        entry.flags = properties.flags;
        entry.finder_info = properties.finder_info;
        entry.created = properties.created;
        entry.modified = properties.modified;
        entry.backup = properties.backup;
        let mut tx = self.transaction()?;
        tx.entry(&entry)?;
        tx.put(2, &attr_key(id, 0), properties.comment.as_bytes())?;
        tx.header.modified = when.0;
        tx.commit()?;
        Ok((entry, true))
    }

    pub fn set_comment(&mut self, id: u32, comment: &str, when: Timestamp) -> Result<()>
    {
        let e = self.entry(id)?;
        if e.flags & 1 != 0
        {
            return Err(Error::Protected);
        }
        if comment.len() > 255
        {
            return Err(Error::Invalid);
        }
        if self.comment(id)? == comment
        {
            return Ok(());
        }
        let mut tx = self.transaction()?;
        tx.put(2, &attr_key(id, 0), comment.as_bytes())?;
        tx.header.modified = when.0;
        tx.commit()
    }

    /// Finder flags are metadata, not a modification of either fork. Preserve
    /// the object's dates while recording the volume's catalog-change time.
    pub fn set_finder_metadata(&mut self, id: u32, flags: u16, finder_info: [u8; 32],
                               when: Timestamp)
                               -> Result<Entry>
    {
        let mut e = self.entry(id)?;
        if e.flags == flags && e.finder_info == finder_info
        {
            return Ok(e);
        }
        e.flags = flags;
        e.finder_info = finder_info;
        let mut tx = self.transaction()?;
        tx.entry(&e)?;
        tx.header.modified = when.0;
        tx.commit()?;
        Ok(e)
    }

    pub fn rename(&mut self, id: u32, parent: u32, name: &str, when: Timestamp) -> Result<Entry>
    {
        self.relocate(id, parent, name, when, MoveOrigin::Keep)
    }

    pub fn move_to_trash(&mut self, id: u32, trash: u32, when: Timestamp) -> Result<Entry>
    {
        let e = self.entry(id)?;
        if e.parent == trash
        {
            return Ok(e);
        }
        let folder = self.entry(trash)?;
        if !folder.directory || folder.parent != 2 || names::comparison(&folder.name)? != "trash"
        {
            return Err(Error::Invalid);
        }
        self.relocate(id, trash, &e.name, when, MoveOrigin::Save)
    }

    pub fn put_away(&mut self, id: u32, when: Timestamp) -> Result<Entry>
    {
        let root = self.header.roots[2];
        let origin = tree::get(self, root, 2, &attr_key(id, 1))?.ok_or(Error::NotFound)?;
        if origin.len() < 6 || origin.len() != 6 + u16_at(&origin, 4)? as usize
        {
            return Err(Error::Corrupt);
        }
        let parent = u32_at(&origin, 0)?;
        let e = self.entry(id)?;
        self.relocate(id, parent, &e.name, when, MoveOrigin::Clear)
    }

    fn relocate(&mut self, id: u32, parent: u32, name: &str, when: Timestamp, origin: MoveOrigin)
                -> Result<Entry>
    {
        let mut e = self.entry(id)?;
        if id == 2 || e.flags & 1 != 0
        {
            return Err(Error::Protected);
        }
        let name = names::normalize(name)?;
        match self.lookup(parent, &name)
        {
            Ok(old) if old.id != id => return Err(Error::Exists),
            Ok(_) | Err(Error::NotFound) =>
            {}
            Err(e) => return Err(e)
        }
        let mut destination = self.entry(parent)?;
        if !destination.directory
        {
            return Err(Error::Invalid);
        }
        if destination.flags & 1 != 0
        {
            return Err(Error::Protected);
        }
        // Prevent directory cycles with a caller-bounded parent walk.
        let mut ancestor = parent;
        for depth in 0..=self.budget.extents
        {
            if ancestor == id
            {
                return Err(Error::Invalid);
            }
            if ancestor == 2
            {
                break;
            }
            if depth == self.budget.extents
            {
                return Err(Error::Budget);
            }
            ancestor = self.entry(ancestor)?.parent;
        }
        let mut source = self.entry(e.parent)?;
        if source.flags & 1 != 0
        {
            return Err(Error::Protected);
        }
        let old_key = name_key(e.parent, &e.name)?;
        let mut saved_origin = Vec::new();
        if matches!(origin, MoveOrigin::Save)
        {
            saved_origin.try_reserve_exact(6 + e.name.len()).map_err(|_| Error::NoMemory)?;
            saved_origin.extend_from_slice(&e.parent.to_be_bytes());
            saved_origin.extend_from_slice(&(e.name.len() as u16).to_be_bytes());
            saved_origin.extend_from_slice(e.name.as_bytes());
            e.finder_info[28..32].copy_from_slice(&e.parent.to_be_bytes());
        }
        e.parent = parent;
        e.name = name;
        let mut tx = self.transaction()?;
        match origin
        {
            MoveOrigin::Keep =>
            {}
            MoveOrigin::Save =>
            {
                tx.put(2, &attr_key(id, 1), &saved_origin)?;
            }
            MoveOrigin::Clear =>
            {
                tx.remove(2, &attr_key(id, 1))?;
            }
        }
        tx.remove(0, &old_key)?;
        tx.put(0, &name_key(parent, &e.name)?, &id.to_be_bytes())?;
        tx.entry(&e)?;
        if source.id != destination.id
        {
            source.children = source.children.checked_sub(1).ok_or(Error::Corrupt)?;
            destination.children = destination.children.checked_add(1).ok_or(Error::Full)?;
            source.modified = when;
            destination.modified = when;
            tx.entry(&source)?;
            tx.entry(&destination)?;
        }
        else
        {
            source.modified = when;
            tx.entry(&source)?;
        }
        tx.header.modified = when.0;
        tx.commit()?;
        Ok(e)
    }
}

pub struct FormatOptions<'a>
{
    pub name:    &'a str,
    pub created: Timestamp,
    pub uuid:    [u8; 16]
}
pub fn format(disk: &mut impl WritableBlockDevice, partition: Partition,
              options: FormatOptions<'_>, budget: Budget)
              -> Result<()>
{
    partition.validate(disk)?;
    if disk.is_read_only()
    {
        return Err(Error::ReadOnly);
    }
    if partition.sectors < 128 || !partition.sectors.is_multiple_of(8)
    {
        return Err(Error::Invalid);
    }
    let blocks = u32::try_from(partition.sectors / 8).map_err(|_| Error::Invalid)?;
    let bitmap_blocks = blocks.div_ceil(32768);
    let first = bitmap_blocks + 1;
    let h = Header { sectors: partition.sectors,
                     blocks,
                     free: blocks - first - 4,
                     bitmap_blocks,
                     roots: [first, first + 1, first + 2],
                     next_id: 16,
                     blessed: 0,
                     hint: first + 3,
                     created: options.created.0,
                     modified: options.created.0,
                     revision: 1,
                     uuid: options.uuid,
                     name: names::normalize(options.name)?,
                     finder_info: [0; 32],
                     flags: FORMATTING };
    // Incomplete formatting is durably identifiable before changing allocation.
    headers(disk, partition, &h)?;
    for i in 0..bitmap_blocks
    {
        let mut b = [0; BLOCK_SIZE];
        for bit in 0..32768u32
        {
            let id = u64::from(i) * 32768 + u64::from(bit);
            if id <= u64::from(first + 2) || id >= u64::from(blocks - 1)
            {
                b[(bit / 8) as usize] |= 0x80 >> (bit % 8);
            }
        }
        write_page(disk, partition, 1 + i, &b)?;
    }
    for (kind, &id) in h.roots.iter().enumerate()
    {
        write_page(disk, partition, id, &Node::empty(kind as u8, 0).encode(id)?)?;
    }
    disk.flush()?;
    let root = Entry { id:          2,
                       parent:      1,
                       name:        names::normalize(options.name)?,
                       directory:   true,
                       flags:       0,
                       children:    0,
                       created:     options.created,
                       modified:    options.created,
                       backup:      None,
                       finder_info: [0; 32],
                       data:        Fork::default(),
                       resources:   Fork::default() };
    let mut v = Volume { disk,
                         partition,
                         header: h,
                         budget };
    let mut header = Header::decode(&v.header.encode()?)?;
    header.flags = 0;
    let mut tx = Transaction { volume: &mut v,
                               header,
                               changes: Vec::new() };
    tx.entry(&root)?;
    tx.commit()
}
