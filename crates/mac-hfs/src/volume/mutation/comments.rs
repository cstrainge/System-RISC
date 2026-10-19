//! Captured System7 Desktop DB type3 comments. Unknown leaf records and user
//! header bytes remain opaque and retain their order and exact payload bytes.
use super::*;
type StagedSectors = Vec<(u64, [u8; 512], u8)>;
const DB_NAME: &[u8] = b"Desktop DB";
const NEW_DB_BYTES: usize = 64 * 1024;
fn comment_key(record: &[u8]) -> Option<u32>
{
    (record.first() == Some(&5) && record.get(1) == Some(&3) && record.len() >= 6)
        .then(|| u32::from_be_bytes(record[2..6].try_into().unwrap()))
}
fn value(record: &[u8]) -> MutationResult<&[u8]>
{
    let length = usize::from(*record.get(6).ok_or(MutationError::Io)?);
    record.get(7..7 + length).ok_or(MutationError::Io)
}
fn replace(tree: &mut Tree, id: u32, comment: &[u8]) -> MutationResult<bool>
{
    if comment.len() > 255
    {
        return Err(MutationError::Invalid);
    }
    let mut found = None;
    for (index, record) in tree.records.iter().enumerate()
    {
        desktop_key_span(record)?;
        if comment_key(record) == Some(id)
        {
            if found.is_some()
            {
                return Err(MutationError::Io);
            }
            value(record)?;
            found = Some(index);
        }
    }
    if comment.is_empty()
    {
        if let Some(index) = found
        {
            tree.records.remove(index);
            return Ok(true);
        }
        return Ok(false);
    }
    if let Some(index) = found
       && value(&tree.records[index])? == comment
    {
        return Ok(false);
    }
    let mut record = Vec::new();
    reserve(&mut record, (7 + comment.len()).next_multiple_of(2))?;
    record.extend_from_slice(&[5, 3]);
    record.extend_from_slice(&id.to_be_bytes());
    record.push(comment.len() as u8);
    record.extend_from_slice(comment);
    if !record.len().is_multiple_of(2)
    {
        record.push(0);
    }
    if let Some(index) = found
    {
        tree.records[index] = record;
    }
    else
    {
        // Captured keys order by record type, with numeric CNID for type3.
        // Never sort unknown keys or change their relative order.
        let index = tree.records
                        .iter()
                        .position(|existing| {
                            existing[1] > 3 || (comment_key(existing).is_some_and(|old| old > id))
                        })
                        .unwrap_or(tree.records.len());
        reserve(&mut tree.records, 1)?;
        tree.records.insert(index, record);
    }
    Ok(true)
}
fn empty_tree() -> Tree
{
    let mut header = [0; 512];
    header[8] = 1;
    put16(&mut header, 10, 3);
    put16(&mut header, 32, 512);
    put16(&mut header, 34, 37);
    put32(&mut header, 36, (NEW_DB_BYTES / 512) as u32);
    put32(&mut header, 46, 32768); // Captured Desktop DB clump size.
    header[120..124].copy_from_slice(&[2, 10, 1, 0]); // Captured user record prefix.
    put16(&mut header, 510, 14);
    put16(&mut header, 508, 120);
    put16(&mut header, 506, 248);
    put16(&mut header, 504, 504);
    Tree { header,
           records: Vec::new(),
           capacity: NEW_DB_BYTES / 512,
           maps: Vec::new() }
}
impl<'a, D: BlockDevice> Volume<'a, D>
{
    pub fn comment(&mut self, id: u32) -> MutationResult<Vec<u8>>
    {
        self.entry(id).map_err(|_| MutationError::Io)?.ok_or(MutationError::NotFound)?;
        let Some(db) = self.lookup(2, DB_NAME).map_err(|_| MutationError::Io)?
        else
        {
            return Ok(Vec::new());
        };
        if db.directory
        {
            return Err(MutationError::Invalid);
        }
        if &db.finder_info[..8] != b"BTFLDMGR"
        {
            return Err(MutationError::Unsupported);
        }
        let mut fork = db.data.ok_or(MutationError::Io)?;
        self.resolve_extents(&mut fork).map_err(|_| MutationError::Io)?;
        let tree = self.mutation_tree(&fork)?;
        let mut result = None;
        for record in tree.records
        {
            if comment_key(&record) == Some(id)
            {
                if result.is_some()
                {
                    return Err(MutationError::Io);
                }
                result = Some(copy(value(&record)?)?);
            }
        }
        Ok(result.unwrap_or_default())
    }
}
impl Catalog
{
    fn add_desktop_db(&mut self, fork: &Fork, physical: u64, when: u32)
                      -> MutationResult<CatalogEntry>
    {
        if self.child(2, DB_NAME).is_some()
        {
            return Err(MutationError::Exists);
        }
        let id = long(&self.mdb, 30)?;
        if id < 16 || self.entries.iter().any(|entry| entry.id == id)
        {
            return Err(MutationError::Unsupported);
        }
        put32(&mut self.mdb, 30, id.checked_add(1).ok_or(MutationError::Full)?);
        let mut data = [0; 102];
        data[0] = 2;
        data[4..12].copy_from_slice(b"BTFLDMGR");
        put16(&mut data, 12, 0x4000);
        put32(&mut data, 20, id);
        put32(&mut data, 26, NEW_DB_BYTES as u32);
        put32(&mut data, 30, u32::try_from(physical).map_err(|_| MutationError::Full)?);
        put32(&mut data, 44, when);
        put32(&mut data, 48, when);
        for (index, extent) in fork.extents.iter().enumerate()
        {
            put16(&mut data, 74 + index * 4, extent.start);
            put16(&mut data, 76 + index * 4, extent.count);
        }
        let record = record(2, DB_NAME, &data)?;
        let entry =
            parse_entry(&record).map_err(|_| MutationError::Io)?.ok_or(MutationError::Io)?;
        reserve(&mut self.tree.records, 1)?;
        self.tree.records.push(record);
        reserve(&mut self.entries, 1)?;
        self.entries.push(entry.clone());
        Ok(entry)
    }
}
impl<'a, D: WritableBlockDevice> Volume<'a, D>
{
    fn allocate_desktop_db(&mut self, cat: &mut Catalog) -> MutationResult<(Fork, StagedSectors)>
    {
        let bitmap_sector = u64::from(word(&cat.mdb, 14)?);
        let bitmap_length = (self.allocation_count as usize).div_ceil(8).next_multiple_of(512);
        if bitmap_sector < 3
           || bitmap_sector + (bitmap_length / 512) as u64 > self.allocation_start / 512
        {
            return Err(MutationError::Unsupported);
        }
        let mut bitmap = Vec::new();
        reserve(&mut bitmap, bitmap_length)?;
        bitmap.resize(bitmap_length, 0);
        read_at(self.disk, (self.partition.start + bitmap_sector) * 512, &mut bitmap).map_err(|_| MutationError::Io)?;
        let free = (0..self.allocation_count as usize).filter(|&block| {
                                                          bitmap[block / 8] & (0x80 >> (block % 8))
                                                          == 0
                                                      })
                                                      .count();
        if free != usize::from(word(&cat.mdb, 34)?)
        {
            return Err(MutationError::Io);
        }
        // Validate every referenced block before considering bitmap-free blocks.
        // A corrupt bitmap must never let a new comment file overwrite live data.
        let mut referenced = Vec::new();
        reserve(&mut referenced, self.allocation_count as usize)?;
        referenced.resize(self.allocation_count as usize, false);
        let mut mark = |fork: &Fork| -> MutationResult<()> {
            for extent in &fork.extents
            {
                for block in
                    usize::from(extent.start)..usize::from(extent.start) + usize::from(extent.count)
                {
                    if block >= referenced.len()
                       || referenced[block]
                       || bitmap[block / 8] & (0x80 >> (block % 8)) == 0
                    {
                        return Err(MutationError::Io);
                    }
                    referenced[block] = true;
                }
            }
            Ok(())
        };
        mark(&self.catalog)?;
        let mut overflow = self.overflow.clone();
        self.resolve_extents(&mut overflow).map_err(|_| MutationError::Io)?;
        mark(&overflow)?;
        for entry in &cat.entries
        {
            for (source, physical) in
                [(&entry.data, entry.data_allocated), (&entry.resources, entry.resource_allocated)]
            {
                if let Some(source) = source
                {
                    if physical < source.length || !physical.is_multiple_of(self.allocation_size)
                    {
                        return Err(MutationError::Io);
                    }
                    let mut fork = source.clone();
                    fork.length = physical;
                    self.resolve_extents(&mut fork).map_err(|_| MutationError::Io)?;
                    if fork.blocks() * self.allocation_size != physical
                    {
                        return Err(MutationError::Io);
                    }
                    mark(&fork)?;
                }
            }
        }
        let needed = (NEW_DB_BYTES as u64).div_ceil(self.allocation_size) as usize;
        if needed > free
        {
            return Err(MutationError::Full);
        }
        let mut extents: Vec<Extent> = Vec::new();
        reserve(&mut extents, 3)?;
        let mut allocated = 0;
        for block in 0..self.allocation_count as usize
        {
            if allocated == needed
            {
                break;
            }
            if bitmap[block / 8] & (0x80 >> (block % 8)) != 0
            {
                continue;
            }
            if let Some(last) =
                extents.last_mut()
                       .filter(|last| usize::from(last.start) + usize::from(last.count) == block)
            {
                last.count = last.count.checked_add(1).ok_or(MutationError::Full)?;
            }
            else
            {
                if extents.len() == 3
                {
                    return Err(MutationError::Full);
                }
                extents.push(Extent { start: block as u16,
                                      count: 1 });
            }
            bitmap[block / 8] |= 0x80 >> (block % 8);
            allocated += 1;
        }
        put16(&mut cat.mdb, 34, (free - needed) as u16);
        let fork = Fork { id: long(&cat.mdb, 30)?,
                          kind: 0,
                          length: NEW_DB_BYTES as u64,
                          extents };
        let mut staged = Vec::new();
        reserve(&mut staged, bitmap_length / 512)?;
        for (index, bytes) in bitmap.as_chunks::<512>().0.iter().enumerate()
        {
            staged.push((self.partition.start + bitmap_sector + index as u64, *bytes, 1));
        }
        Ok((fork, staged))
    }

    pub fn set_comment(&mut self, id: u32, comment: &[u8], when: u32, protected: &[u32])
                       -> MutationResult<()>
    {
        self.writable()?;
        if comment.len() > 255
        {
            return Err(MutationError::Invalid);
        }
        let mut cat = self.mutation_catalog()?;
        self.protection(&cat, id, protected)?;
        if cat.entries[cat.index(id)?].flags & 1 != 0
        {
            return Err(MutationError::Protected);
        }
        let existing = cat.child(2, DB_NAME).cloned();
        let (mut tree, fork, mut additional, created) = if let Some(db) = existing
        {
            if db.directory
            {
                return Err(MutationError::Invalid);
            }
            if &db.finder_info[..8] != b"BTFLDMGR"
            {
                return Err(MutationError::Unsupported);
            }
            if db.id == id || db.flags & 1 != 0
            {
                return Err(MutationError::Protected);
            }
            self.protection(&cat, db.id, protected)?;
            let mut fork = db.data.ok_or(MutationError::Io)?;
            self.resolve_extents(&mut fork).map_err(|_| MutationError::Io)?;
            (self.mutation_tree(&fork)?, fork, Vec::new(), false)
        }
        else
        {
            if comment.is_empty()
            {
                return Ok(());
            }
            let (fork, staged) = self.allocate_desktop_db(&mut cat)?;
            let physical = fork.blocks() * self.allocation_size;
            if physical > 256 * 1024
            {
                return Err(MutationError::Full);
            }
            cat.add_desktop_db(&fork, physical, when)?;
            (empty_tree(), fork, staged, true)
        };
        if !replace(&mut tree, id, comment)?
        {
            return Ok(());
        }
        let nodes = build_tree(&mut tree, true, true)?;
        let physical = if created { fork.blocks() * self.allocation_size } else { 0 };
        let sector_count = if created { (physical / 512) as usize } else { nodes.len() };
        reserve(&mut additional, sector_count)?;
        for index in 0..sector_count
        {
            let bytes = nodes.get(index).copied().unwrap_or([0; 512]);
            additional.push((self.mutation_sector(&fork, index * 512)?, bytes, 0));
        }
        let (record_index, start) = cat.record_index(fork.id)?;
        put32(&mut cat.tree.records[record_index], start + 48, when);
        self.finish_mutation_staged(cat,
                                    when,
                                    if created { &[2] } else { &[] },
                                    &[],
                                    None,
                                    &additional)
    }
}
