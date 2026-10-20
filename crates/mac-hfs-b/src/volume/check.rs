//! Full consistency checking, deliberately separate from bounded mount/lookup.
use super::*;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CheckReport
{
    pub objects:        u64,
    pub files:          u64,
    pub directories:    u64,
    pub tree_pages:     u64,
    pub payload_blocks: u64
}
fn mark(bits: &mut [u8], id: u32) -> Result<()>
{
    let b = bits.get_mut((id / 8) as usize).ok_or(Error::Corrupt)?;
    let mask = 0x80 >> (id % 8);
    if *b & mask != 0
    {
        return Err(Error::Corrupt);
    }
    *b |= mask;
    Ok(())
}
impl<D: BlockDevice> Volume<'_, D>
{
    /// Check all reachable metadata/payload ownership against the bitmap,
    /// catalog indexes/parents/counts, fork extents and blessed directory.
    /// This reports damage; it does not silently repair or clear dirty state.
    pub fn check(&mut self) -> Result<CheckReport>
    {
        let byte_count = self.header.blocks.div_ceil(8) as usize;
        if byte_count > self.budget.io_bytes
        {
            return Err(Error::Budget);
        }
        let mut claimed = Vec::new();
        claimed.try_reserve_exact(byte_count).map_err(|_| Error::NoMemory)?;
        claimed.resize(byte_count, 0);
        mark(&mut claimed, 0)?;
        mark(&mut claimed, self.header.blocks - 1)?;
        for id in 1..=self.header.bitmap_blocks
        {
            mark(&mut claimed, id)?;
        }
        let mut report = CheckReport::default();
        let header = Header::decode(&self.header.encode()?)?;
        let object_limit = self.budget.io_bytes / 512;
        let mut objects = Vec::new();
        let mut names = Vec::new();
        let mut overflow_records = 0usize;
        let mut attributes = Vec::new();
        for kind in 0..3
        {
            tree::walk(self, header.roots[kind], kind as u8, &mut |id, node| {
                mark(&mut claimed, id)?;
                report.tree_pages += 1;
                if node.level != 0
                {
                    return Ok(());
                }
                for r in &node.records
                {
                    match kind
                    {
                        0 if r.key[0] == 0 =>
                        {
                            if objects.len() >= object_limit
                            {
                                return Err(Error::Budget);
                            }
                            let e = Entry::decode(&r.value, &header)?;
                            if id_key(e.id).as_slice() != r.key
                            {
                                return Err(Error::Corrupt);
                            }
                            objects.try_reserve(1).map_err(|_| Error::NoMemory)?;
                            objects.push(e);
                        }
                        0 =>
                        {
                            if names.len() >= object_limit
                            {
                                return Err(Error::Budget);
                            }
                            if r.value.len() != 4
                            {
                                return Err(Error::Corrupt);
                            }
                            names.try_reserve(1).map_err(|_| Error::NoMemory)?;
                            names.push((u32_at(&r.value, 0)?, copy(&r.key)?));
                        }
                        1 =>
                        {
                            if r.key.len() != 13 || r.value.len() != 8
                            {
                                return Err(Error::Corrupt);
                            }
                            overflow_records += 1;
                        }
                        _ =>
                        {
                            if attributes.len() >= object_limit * 2
                               || r.key.len() != 5
                               || r.key[4] > 1
                            {
                                return Err(Error::Corrupt);
                            }
                            attributes.try_reserve(1).map_err(|_| Error::NoMemory)?;
                            attributes.push((u32_at(&r.key, 0)?, r.key[4], copy(&r.value)?));
                        }
                    }
                }
                Ok(())
            })?;
        }
        if objects.first().is_none_or(|e| e.id != 2) || names.len() + 1 != objects.len()
        {
            return Err(Error::Corrupt);
        }
        let mut counts = Vec::new();
        counts.try_reserve_exact(objects.len()).map_err(|_| Error::NoMemory)?;
        counts.resize(objects.len(), (0u32, false, 0u8));
        for (id, key) in names
        {
            let i = objects.binary_search_by_key(&id, |e| e.id).map_err(|_| Error::Corrupt)?;
            let e = &objects[i];
            if counts[i].1 || id == 2 || name_key(e.parent, &e.name)? != key
            {
                return Err(Error::Corrupt);
            }
            counts[i].1 = true;
        }
        let mut expected_overflow = 0;
        for e in &objects
        {
            if e.id != 2
            {
                let i = objects.binary_search_by_key(&e.parent, |e| e.id)
                               .map_err(|_| Error::Corrupt)?;
                if !objects[i].directory
                {
                    return Err(Error::Corrupt);
                }
                counts[i].0 = counts[i].0.checked_add(1).ok_or(Error::Corrupt)?;
            }
            for kind in [ForkKind::Data, ForkKind::Resource]
            {
                let extents = self.extent_list(e.id, kind, e.fork(kind))?;
                expected_overflow += extents.len().saturating_sub(8);
                for extent in extents
                {
                    for id in extent.start..extent.start + extent.count
                    {
                        mark(&mut claimed, id)?;
                        report.payload_blocks += 1;
                    }
                }
            }
        }
        if expected_overflow != overflow_records
        {
            return Err(Error::Corrupt);
        }
        counts[0].2 = 2;
        // Color parent chains once; detect cycles without quadratic traversal.
        for i in 1..objects.len()
        {
            let mut current = i;
            while counts[current].2 == 0
            {
                counts[current].2 = 1;
                current = objects.binary_search_by_key(&objects[current].parent, |e| e.id)
                                 .map_err(|_| Error::Corrupt)?;
            }
            if counts[current].2 == 1
            {
                return Err(Error::Corrupt);
            }
            current = i;
            while counts[current].2 == 1
            {
                counts[current].2 = 2;
                current = objects.binary_search_by_key(&objects[current].parent, |e| e.id)
                                 .map_err(|_| Error::Corrupt)?;
            }
        }
        for (i, e) in objects.iter().enumerate()
        {
            if e.children != counts[i].0
            {
                return Err(Error::Corrupt);
            }
            if e.directory
            {
                report.directories += 1;
            }
            else
            {
                report.files += 1;
            }
        }
        for (id, kind, value) in attributes
        {
            objects.binary_search_by_key(&id, |e| e.id).map_err(|_| Error::Corrupt)?;
            if kind == 0
            {
                if value.len() > 255 || core::str::from_utf8(&value).is_err()
                {
                    return Err(Error::Corrupt);
                }
            }
            else
            {
                if value.len() < 6 || value.len() != 6 + u16_at(&value, 4)? as usize
                {
                    return Err(Error::Corrupt);
                }
                let name = core::str::from_utf8(&value[6..]).map_err(|_| Error::Corrupt)?;
                if names::normalize(name)? != name
                {
                    return Err(Error::Corrupt);
                }
            }
        }
        if self.header.blessed != 0
        {
            self.blessed_folder()?;
        }
        let mut free = 0u32;
        for sector in 0..u64::from(self.header.bitmap_blocks) * 8
        {
            let mut bitmap = [0; 512];
            self.disk.read_sector(self.partition.start + 8 + sector, &mut bitmap)?;
            for (byte, &b) in bitmap.iter().enumerate()
            {
                let at = sector * 512 + byte as u64;
                for bit in 0..8u32
                {
                    let id = at * 8 + u64::from(bit);
                    let mask = 0x80 >> bit;
                    if id >= u64::from(self.header.blocks)
                    {
                        if b & mask == 0
                        {
                            return Err(Error::Corrupt);
                        }
                        continue;
                    }
                    let expected = claimed[id as usize / 8] & mask != 0;
                    if (b & mask != 0) != expected
                    {
                        return Err(Error::Corrupt);
                    }
                    if !expected
                    {
                        free += 1;
                    }
                }
            }
        }
        if free != self.header.free
        {
            return Err(Error::Corrupt);
        }
        report.objects = objects.len() as u64;
        Ok(report)
    }
}
