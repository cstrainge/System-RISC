//! Both forks use copy-on-write payload blocks and bounded metadata commits.
use super::*;
/// Whole-data-fork publication for document transactions. Larger files use the
/// ordinary streaming interface; this operation has the caller's I/O budget.
pub struct DataSave<'a>
{
    pub parent:    u32,
    pub name:      &'a str,
    pub expected:  u32,
    pub file_type: [u8; 4],
    pub creator:   [u8; 4],
    pub when:      Timestamp
}
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
    pub(super) fn replace_fork(&mut self, entry: &mut Entry, kind: ForkKind, old: &[Extent],
                               new: &[Extent], length: u64)
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

    pub(super) fn free_range(&mut self, extents: &[Extent], from: u64, to: u64) -> Result<()>
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
    pub fn save_data(&mut self, spec: DataSave<'_>, data: &[u8]) -> Result<Entry>
    {
        if data.len() > self.budget.io_bytes
        {
            return Err(Error::Budget);
        }
        let name = names::normalize(spec.name)?;
        let mut parent = self.entry(spec.parent)?;
        if !parent.directory
        {
            return Err(Error::Invalid);
        }
        if parent.flags & 1 != 0
        {
            return Err(Error::Protected);
        }
        let (mut entry, created) = match self.lookup(spec.parent, &name)
        {
            Ok(e) =>
            {
                if spec.expected == 0
                {
                    return Err(Error::Exists);
                }
                if e.id != spec.expected || e.directory
                {
                    return Err(Error::Invalid);
                }
                if e.flags & 1 != 0
                {
                    return Err(Error::Protected);
                }
                (e, false)
            }
            Err(Error::NotFound) if spec.expected == 0 =>
            {
                (Entry { id: self.header.next_id,
                         parent: spec.parent,
                         name,
                         directory: false,
                         flags: 0,
                         children: 0,
                         created: spec.when,
                         modified: spec.when,
                         backup: None,
                         finder_info: [0; 32],
                         data: Fork::default(),
                         resources: Fork::default() },
                 true)
            }
            Err(e) => return Err(e)
        };
        let old = self.extent_list(entry.id, ForkKind::Data, &entry.data)?;
        let limit = self.budget.extents;
        let mut new = Vec::new();
        let mut tx = self.transaction()?;
        if created
        {
            tx.header.next_id = tx.header.next_id.checked_add(1).ok_or(Error::Full)?;
        }
        for chunk in data.chunks(4096)
        {
            let mut page = [0; 4096];
            page[..chunk.len()].copy_from_slice(chunk);
            let block = tx.allocate()?;
            write_page(tx.volume.disk, tx.volume.partition, block, &page)?;
            append(&mut new,
                   Extent { start: block,
                            count: 1 },
                   limit)?;
        }
        tx.volume.disk.flush()?;
        tx.free_range(&old, 0, entry.data.blocks)?;
        entry.modified = spec.when;
        if created
        {
            entry.finder_info[..4].copy_from_slice(&spec.file_type);
            entry.finder_info[4..8].copy_from_slice(&spec.creator);
            tx.put(0, &name_key(spec.parent, &entry.name)?, &entry.id.to_be_bytes())?;
            parent.children = parent.children.checked_add(1).ok_or(Error::Full)?;
        }
        tx.replace_fork(&mut entry, ForkKind::Data, &old, &new, data.len() as u64)?;
        parent.modified = spec.when;
        tx.entry(&parent)?;
        tx.header.modified = spec.when.0;
        tx.commit()?;
        Ok(entry)
    }

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
        self.remove_many(&[id], when)
    }

    /// Atomically remove an explicit set, including all children of any listed
    /// directory. Preflight and metadata staging are bounded by caller budgets.
    pub fn remove_many(&mut self, ids: &[u32], when: Timestamp) -> Result<()>
    {
        if ids.is_empty()
        {
            return Ok(());
        }
        if ids.len() > self.budget.io_bytes / 512
        {
            return Err(Error::Budget);
        }
        let mut sorted = Vec::new();
        sorted.try_reserve_exact(ids.len()).map_err(|_| Error::NoMemory)?;
        sorted.extend_from_slice(ids);
        sorted.sort_unstable();
        if sorted.windows(2).any(|w| w[0] == w[1])
        {
            return Err(Error::Invalid);
        }
        let mut prepared = Vec::new();
        let mut parents = Vec::new();
        prepared.try_reserve_exact(ids.len()).map_err(|_| Error::NoMemory)?;
        parents.try_reserve_exact(ids.len()).map_err(|_| Error::NoMemory)?;
        let mut total_extents = 0;
        for &id in ids
        {
            let e = self.entry(id)?;
            if id == 2 || id == self.header.blessed || e.flags & 1 != 0
            {
                return Err(Error::Protected);
            }
            let data = self.extent_list(id, ForkKind::Data, &e.data)?;
            let resources = self.extent_list(id, ForkKind::Resource, &e.resources)?;
            total_extents += data.len() + resources.len();
            if total_extents > self.budget.extents
            {
                return Err(Error::Budget);
            }
            parents.push(e.parent);
            prepared.push((e, data, resources));
        }
        parents.sort_unstable();
        let removed_children =
            |id: u32| parents.partition_point(|&v| v <= id) - parents.partition_point(|&v| v < id);
        for (e, _, _) in &prepared
        {
            if e.directory && removed_children(e.id) != e.children as usize
            {
                return Err(Error::Protected);
            }
        }
        let mut updates = Vec::new();
        for (i, &id) in parents.iter().enumerate()
        {
            if (i > 0 && parents[i - 1] == id) || sorted.binary_search(&id).is_ok()
            {
                continue;
            }
            let mut parent = self.entry(id)?;
            if parent.flags & 1 != 0
            {
                return Err(Error::Protected);
            }
            parent.children =
                parent.children.checked_sub(removed_children(id) as u32).ok_or(Error::Corrupt)?;
            parent.modified = when;
            updates.try_reserve(1).map_err(|_| Error::NoMemory)?;
            updates.push(parent);
        }
        let mut tx = self.transaction()?;
        for (e, data, resources) in prepared
        {
            for (kind, extents) in [(ForkKind::Data, data), (ForkKind::Resource, resources)]
            {
                tx.free_range(&extents, 0, e.fork(kind).blocks)?;
                let mut logical = 0;
                for (i, extent) in extents.iter().enumerate()
                {
                    if i >= 8
                    {
                        tx.remove(1, &overflow_key(e.id, kind, logical))?;
                    }
                    logical += u64::from(extent.count);
                }
            }
            tx.remove(0, &id_key(e.id))?;
            tx.remove(0, &name_key(e.parent, &e.name)?)?;
            for kind in 0..=1
            {
                match tx.remove(2, &attr_key(e.id, kind))
                {
                    Ok(()) | Err(Error::NotFound) =>
                    {}
                    Err(e) => return Err(e)
                }
            }
        }
        for parent in updates
        {
            tx.entry(&parent)?;
        }
        tx.header.modified = when.0;
        tx.commit()
    }
}
