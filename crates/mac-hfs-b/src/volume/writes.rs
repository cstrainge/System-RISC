//! Both forks use copy-on-write payload blocks and bounded metadata commits.
use super::*;
fn append(out: &mut Vec<Extent>, e: Extent, limit: usize) -> Result<()>
{
    if e.count == 0
    {
        return Ok(());
    }
    if let Some(last) = out.last_mut()
       && last.start.checked_add(last.count) == Some(e.start)
    {
        last.count = last.count.checked_add(e.count).ok_or(Error::Full)?;
        return Ok(());
    }
    if out.len() >= limit
    {
        return Err(Error::Budget);
    }
    out.try_reserve(1).map_err(|_| Error::NoMemory)?;
    out.push(e);
    Ok(())
}
fn range(extents: &[Extent], from: u64, to: u64, out: &mut Vec<Extent>, limit: usize)
         -> Result<()>
{
    let mut position = 0;
    for e in extents
    {
        let end = position + u64::from(e.count);
        let a = position.max(from);
        let b = end.min(to);
        if a < b
        {
            append(out,
                   Extent { start: e.start + (a - position) as u32,
                            count: (b - a) as u32 },
                   limit)?;
        }
        position = end;
    }
    Ok(())
}
fn physical(extents: &[Extent], index: u64) -> Option<u32>
{
    let mut position = 0;
    for e in extents
    {
        if index < position + u64::from(e.count)
        {
            return Some(e.start + (index - position) as u32);
        }
        position += u64::from(e.count);
    }
    None
}
impl<D: WritableBlockDevice> Transaction<'_, '_, D>
{
    fn replace_fork(&mut self, entry: &mut Entry, kind: ForkKind, old: &[Extent], new: &[Extent],
                    length: u64)
                    -> Result<()>
    {
        let mut logical = 0;
        for (i, e) in old.iter().enumerate()
        {
            if i >= 8
            {
                self.remove(1, &overflow_key(entry.id, kind, logical))?;
            }
            logical += u64::from(e.count);
        }
        let mut fork = Fork { length,
                              ..Default::default() };
        for (i, &e) in new.iter().enumerate()
        {
            if i < 8
            {
                fork.extents[i] = e;
            }
            else
            {
                let mut value = [0; 8];
                put32(&mut value, 0, e.start);
                put32(&mut value, 4, e.count);
                self.put(1, &overflow_key(entry.id, kind, fork.blocks), &value)?;
            }
            fork.blocks += u64::from(e.count);
        }
        *entry.fork_mut(kind) = fork;
        self.entry(entry)
    }

    fn free_range(&mut self, extents: &[Extent], from: u64, to: u64) -> Result<()>
    {
        let mut position = 0;
        for e in extents
        {
            let end = position + u64::from(e.count);
            for i in position.max(from)..end.min(to)
            {
                self.release(e.start + (i - position) as u32)?;
            }
            position = end;
        }
        Ok(())
    }
}
impl<'a, D: WritableBlockDevice> Volume<'a, D>
{
    /// Bounded random-access writes to either fork, preserving the other fork.
    /// Gaps and newly exposed bytes are zero-filled. Large writes stream through
    /// repeated calls; the limit bounds work per call, not total fork length.
    pub fn write_fork(&mut self, id: u32, kind: ForkKind, offset: u64, data: &[u8],
                      when: Timestamp)
                      -> Result<Entry>
    {
        let mut e = self.entry(id)?;
        if e.directory
        {
            return Err(Error::Invalid);
        }
        if e.flags & 1 != 0
        {
            return Err(Error::Protected);
        }
        if data.is_empty()
        {
            return Ok(e);
        }
        let old_length = e.fork(kind).length;
        let old_blocks = e.fork(kind).blocks;
        let end = offset.checked_add(data.len() as u64).ok_or(Error::Invalid)?;
        let first = offset.min(old_length) / 4096;
        let last = end.div_ceil(4096);
        if last > u64::from(self.header.blocks)
           || last - first > (self.budget.io_bytes / 4096) as u64
        {
            return Err(Error::Budget);
        }
        let old = self.extent_list(id, kind, e.fork(kind))?;
        let limit = self.budget.extents;
        let mut new = Vec::new();
        range(&old, 0, first, &mut new, limit)?;
        let mut tx = self.transaction()?;
        for index in first..last
        {
            let mut page = if let Some(block) = physical(&old, index)
            {
                tx.volume.allocated(block)?;
                read_page(tx.volume.disk, tx.volume.partition, block)?
            }
            else
            {
                [0; 4096]
            };
            let base = index * 4096;
            if old_length >= base && old_length < base + 4096
            {
                page[(old_length - base) as usize..].fill(0);
            }
            let a = base.max(offset);
            let b = (base + 4096).min(end);
            if a < b
            {
                page[(a - base) as usize..(b - base) as usize].copy_from_slice(&data[(a - offset)
                                                                                     as usize
                                                                                     ..(b - offset)
                                                                                       as usize]);
            }
            let block = tx.allocate()?;
            write_page(tx.volume.disk, tx.volume.partition, block, &page)?;
            append(&mut new,
                   Extent { start: block,
                            count: 1 },
                   limit)?;
        }
        range(&old, last, old_blocks, &mut new, limit)?;
        // Payload becomes durable while still unreachable from the catalog.
        tx.volume.disk.flush()?;
        tx.free_range(&old, first, last.min(old_blocks))?;
        e.modified = when;
        tx.replace_fork(&mut e, kind, &old, &new, old_length.max(end))?;
        tx.header.modified = when.0;
        tx.commit()?;
        Ok(e)
    }

    /// Change logical fork length. Growth is zero-filled and bounded by the
    /// supplied I/O budget; callers grow large files in bounded steps.
    pub fn set_fork_len(&mut self, id: u32, kind: ForkKind, length: u64, when: Timestamp)
                        -> Result<Entry>
    {
        let mut e = self.entry(id)?;
        let old_length = e.fork(kind).length;
        if e.directory
        {
            return Err(Error::Invalid);
        }
        if e.flags & 1 != 0
        {
            return Err(Error::Protected);
        }
        if length == old_length
        {
            return Ok(e);
        }
        if length > old_length
        {
            let n = usize::try_from(length - old_length).map_err(|_| Error::Budget)?;
            if n > self.budget.io_bytes
            {
                return Err(Error::Budget);
            }
            let mut zeros = Vec::new();
            zeros.try_reserve_exact(n).map_err(|_| Error::NoMemory)?;
            zeros.resize(n, 0);
            return self.write_fork(id, kind, old_length, &zeros, when);
        }
        let old = self.extent_list(id, kind, e.fork(kind))?;
        let old_blocks = e.fork(kind).blocks;
        let keep = length.div_ceil(4096);
        let limit = self.budget.extents;
        let mut new = Vec::new();
        let partial = !length.is_multiple_of(4096);
        range(&old, 0, keep - u64::from(partial), &mut new, limit)?;
        let mut tx = self.transaction()?;
        if partial
        {
            let block = physical(&old, keep - 1).ok_or(Error::Corrupt)?;
            tx.volume.allocated(block)?;
            let mut page = read_page(tx.volume.disk, tx.volume.partition, block)?;
            page[(length % 4096) as usize..].fill(0);
            let replacement = tx.allocate()?;
            write_page(tx.volume.disk, tx.volume.partition, replacement, &page)?;
            append(&mut new,
                   Extent { start: replacement,
                            count: 1 },
                   limit)?;
            tx.volume.disk.flush()?;
        }
        tx.free_range(&old, keep - u64::from(partial), old_blocks)?;
        e.modified = when;
        tx.replace_fork(&mut e, kind, &old, &new, length)?;
        tx.header.modified = when.0;
        tx.commit()?;
        Ok(e)
    }

    /// Remove a file or empty directory. Nonempty trees require explicit child
    /// deletion, allowing tools/services to bound traversal and cancellation.
    pub fn remove(&mut self, id: u32, when: Timestamp) -> Result<()>
    {
        let mut e = self.entry(id)?;
        if id == 2 || id == self.header.blessed || e.flags & 1 != 0
        {
            return Err(Error::Protected);
        }
        if e.directory && e.children != 0
        {
            return Err(Error::Protected);
        }
        let mut parent = self.entry(e.parent)?;
        if parent.flags & 1 != 0
        {
            return Err(Error::Protected);
        }
        let data = self.extent_list(id, ForkKind::Data, &e.data)?;
        let resources = self.extent_list(id, ForkKind::Resource, &e.resources)?;
        let mut tx = self.transaction()?;
        tx.free_range(&data, 0, e.data.blocks)?;
        tx.free_range(&resources, 0, e.resources.blocks)?;
        tx.replace_fork(&mut e, ForkKind::Data, &data, &[], 0)?;
        tx.replace_fork(&mut e, ForkKind::Resource, &resources, &[], 0)?;
        tx.remove(0, &id_key(id))?;
        tx.remove(0, &name_key(e.parent, &e.name)?)?;
        for kind in 0..=1
        {
            match tx.remove(2, &attr_key(id, kind))
            {
                Ok(()) | Err(Error::NotFound) =>
                {}
                Err(e) => return Err(e)
            }
        }
        parent.children = parent.children.checked_sub(1).ok_or(Error::Corrupt)?;
        parent.modified = when;
        tx.entry(&parent)?;
        tx.header.modified = when.0;
        tx.commit()
    }
}
