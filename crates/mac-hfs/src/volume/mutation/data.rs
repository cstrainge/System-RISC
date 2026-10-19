//! Copy-on-write ordinary data forks. Payload reaches unreferenced free blocks
//! before a journaled catalog/bitmap commit. Resource fork bytes are untouched.
use super::*;
/// Checked target and file metadata for a copy-on-write ordinary data fork.
pub struct DataSave<'a>
{
    pub parent:    u32,
    pub name:      &'a [u8],
    pub expected:  u32,
    pub file_type: [u8; 4],
    pub creator:   [u8; 4],
    pub when:      u32
}
impl Catalog
{
    fn add_data_file(&mut self, spec: &DataSave<'_>, fork: &Fork, physical: u64)
                     -> MutationResult<u32>
    {
        let DataSave { parent,
                       name,
                       file_type,
                       creator,
                       when,
                       .. } = *spec;
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
        if id < 16 || self.entries.iter().any(|entry| entry.id == id)
        {
            return Err(MutationError::Unsupported);
        }
        put32(&mut self.mdb, 30, id.checked_add(1).ok_or(MutationError::Full)?);
        let mut data = [0; 102];
        data[0] = 2;
        data[4..8].copy_from_slice(&file_type);
        data[8..12].copy_from_slice(&creator);
        put32(&mut data, 20, id);
        put32(&mut data, 26, u32::try_from(fork.length).map_err(|_| MutationError::Full)?);
        put32(&mut data, 30, u32::try_from(physical).map_err(|_| MutationError::Full)?);
        put32(&mut data, 44, when);
        put32(&mut data, 48, when);
        for (i, extent) in fork.extents.iter().enumerate()
        {
            put16(&mut data, 74 + i * 4, extent.start);
            put16(&mut data, 76 + i * 4, extent.count);
        }
        let record = record(parent, name, &data)?;
        let entry =
            parse_entry(&record).map_err(|_| MutationError::Io)?.ok_or(MutationError::Io)?;
        reserve(&mut self.tree.records, 1)?;
        reserve(&mut self.entries, 1)?;
        self.tree.records.push(record);
        self.entries.push(entry);
        Ok(id)
    }
}
impl<'a, D: WritableBlockDevice> Volume<'a, D>
{
    /// expected=0 creates a new file; otherwise require the same catalog ID that
    /// the user confirmed. Existing data/resources survive every reported error.
    pub fn save_data(&mut self, spec: DataSave<'_>, data: &[u8], protected: &[u32])
                     -> MutationResult<CatalogEntry>
    {
        let DataSave { parent,
                       name,
                       expected,
                       file_type,
                       creator,
                       when } = spec;
        self.writable()?;
        validate_name(name)?;
        let mut cat = self.mutation_catalog()?;
        if !cat.entries[cat.index(parent)?].directory
        {
            return Err(MutationError::Invalid);
        }
        let old = cat.child(parent, name).cloned();
        if old.as_ref().map_or(0, |e| e.id) != expected
        {
            return Err(MutationError::Exists);
        }
        if let Some(old) = &old
        {
            self.protection(&cat, old.id, protected)?;
            if old.directory || old.flags & 1 != 0
            {
                return Err(MutationError::Protected);
            }
        }
        let (mut fork, mut additional) = self.allocate_data_fork(&mut cat, data.len() as u64)?;
        let physical = fork.blocks() * self.allocation_size;
        let mut overflow = None;
        let id = if let Some(old) = &old
        {
            fork.id = old.id;
            let mut previous = old.data.clone().ok_or(MutationError::Io)?;
            previous.length = old.data_allocated;
            self.resolve_extents(&mut previous).map_err(|_| MutationError::Io)?;
            let bitmap_start = u64::from(word(&cat.mdb, 14)?) + self.partition.start;
            for extent in &previous.extents
            {
                for block in
                    usize::from(extent.start)..usize::from(extent.start) + usize::from(extent.count)
                {
                    let byte = block / 8;
                    let sector = bitmap_start + (byte / 512) as u64;
                    let (_, bitmap, _) = additional.iter_mut()
                                                   .find(|(s, _, _)| *s == sector)
                                                   .ok_or(MutationError::Io)?;
                    if bitmap[byte % 512] & (0x80 >> (block % 8)) == 0
                    {
                        return Err(MutationError::Io);
                    }
                    bitmap[byte % 512] &= !(0x80 >> (block % 8));
                }
            }
            let free = u64::from(word(&cat.mdb, 34)?) + previous.blocks();
            put16(&mut cat.mdb, 34, u16::try_from(free).map_err(|_| MutationError::Full)?);
            if self.overflow.length != 0
            {
                let source = self.overflow.clone();
                let mut tree = self.mutation_tree(&source)?;
                for r in &tree.records
                {
                    if r.len() != 20 || r[0] != 7 || !matches!(r[1], 0 | 0xff)
                    {
                        return Err(MutationError::Io);
                    }
                }
                tree.records.retain(|r| !(r[1] == 0 && long(r, 2).ok() == Some(old.id)));
                overflow = Some(tree);
            }
            let (record, start) = cat.record_index(old.id)?;
            let bytes = &mut cat.tree.records[record];
            bytes[start + 4..start + 8].copy_from_slice(&file_type);
            bytes[start + 8..start + 12].copy_from_slice(&creator);
            put32(bytes, start + 26, u32::try_from(data.len()).map_err(|_| MutationError::Full)?);
            put32(bytes, start + 30, u32::try_from(physical).map_err(|_| MutationError::Full)?);
            put32(bytes, start + 48, when);
            bytes[start + 74..start + 86].fill(0);
            for (i, e) in fork.extents.iter().enumerate()
            {
                put16(bytes, start + 74 + i * 4, e.start);
                put16(bytes, start + 76 + i * 4, e.count);
            }
            let entry =
                parse_entry(bytes).map_err(|_| MutationError::Io)?.ok_or(MutationError::Io)?;
            let index = cat.index(old.id)?;
            cat.entries[index] = entry;
            old.id
        }
        else
        {
            cat.add_data_file(&spec, &fork, physical)?
        };
        let result = cat.entries[cat.index(id)?].clone();
        // Only currently-free blocks are written here. Failure leaves the old
        // catalog and bitmap intact; no live payload is used as scratch space.
        for (index, chunk) in data.chunks(512).enumerate()
        {
            let mut sector = [0; 512];
            sector[..chunk.len()].copy_from_slice(chunk);
            let location = self.mutation_sector(&fork, index * 512)?;
            self.disk.write_sector(location, &sector).map_err(|_| MutationError::Io)?;
        }
        self.disk.flush().map_err(|_| MutationError::Io)?;
        self.finish_mutation_staged(cat, when, &[parent], &[], overflow, &additional)?;
        Ok(result)
    }
}
