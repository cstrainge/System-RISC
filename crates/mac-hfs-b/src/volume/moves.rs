//! A group move/reposition publishes one metadata transaction, or changes nothing.
use super::*;
impl<D: WritableBlockDevice> Transaction<'_, '_, D>
{
    fn get_entry(&mut self, id: u32) -> Result<Entry>
    {
        let root = self.header.roots[0];
        let bytes = tree::get(self, root, 0, &id_key(id))?.ok_or(Error::NotFound)?;
        let entry = Entry::decode(&bytes, &self.header)?;
        if entry.id != id
        {
            return Err(Error::Corrupt);
        }
        Ok(entry)
    }
}
impl<D: WritableBlockDevice> Volume<'_, D>
{
    /// New parents and complete Finder info, prepared by the shared file service.
    /// Object dates/forks stay untouched. All validations precede disk publication.
    pub fn move_entries(&mut self, entries: &[(u32, u32, [u8; 32], bool)], when: Timestamp)
                        -> Result<bool>
    {
        if entries.len() > 1024
        {
            return Err(Error::Budget);
        }
        let mut tx = self.transaction()?;
        let mut changed = false;
        let mut ids = Vec::new();
        ids.try_reserve_exact(entries.len()).map_err(|_| Error::NoMemory)?;
        ids.extend(entries.iter().map(|e| e.0));
        ids.sort_unstable();
        if ids.windows(2).any(|p| p[0] == p[1])
        {
            return Err(Error::Invalid);
        }
        for &(id, parent, info, trash) in entries
        {
            if id <= 2 || parent < 2
            {
                return Err(Error::Invalid);
            }
            let mut entry = tx.get_entry(id)?;
            if parent != entry.parent
            {
                if entry.flags & 1 != 0
                   || u16::from_be_bytes(entry.finder_info[8..10].try_into().unwrap()) & 0x1000 != 0
                {
                    return Err(Error::Protected);
                }
                let mut target = tx.get_entry(parent)?;
                if !target.directory
                {
                    return Err(Error::Invalid);
                }
                if target.flags & 1 != 0
                {
                    return Err(Error::Protected);
                }
                let mut ancestor = parent;
                for depth in 0..=tx.volume.budget.extents
                {
                    if ancestor == id
                    {
                        return Err(Error::Invalid);
                    }
                    if ancestor == 2
                    {
                        break;
                    }
                    if depth == tx.volume.budget.extents
                    {
                        return Err(Error::Budget);
                    }
                    ancestor = tx.get_entry(ancestor)?.parent;
                }
                let key = name_key(parent, &entry.name)?;
                let root = tx.header.roots[0];
                if tree::get(&mut tx, root, 0, &key)?.is_some()
                {
                    return Err(Error::Exists);
                }
                if trash
                {
                    if target.parent != 2 || names::comparison(&target.name)? != "trash"
                    {
                        return Err(Error::Invalid);
                    }
                    let mut origin = Vec::new();
                    origin.try_reserve_exact(6 + entry.name.len()).map_err(|_| Error::NoMemory)?;
                    origin.extend_from_slice(&entry.parent.to_be_bytes());
                    origin.extend_from_slice(&(entry.name.len() as u16).to_be_bytes());
                    origin.extend_from_slice(entry.name.as_bytes());
                    tx.put(2, &attr_key(id, 1), &origin)?;
                }
                let mut source = tx.get_entry(entry.parent)?;
                if source.flags & 1 != 0
                {
                    return Err(Error::Protected);
                }
                tx.remove(0, &name_key(entry.parent, &entry.name)?)?;
                tx.put(0, &key, &id.to_be_bytes())?;
                source.children = source.children.checked_sub(1).ok_or(Error::Corrupt)?;
                target.children = target.children.checked_add(1).ok_or(Error::Full)?;
                source.modified = when;
                target.modified = when;
                tx.entry(&source)?;
                tx.entry(&target)?;
            }
            else if entry.finder_info == info
            {
                continue;
            }
            entry.parent = parent;
            entry.finder_info = info;
            tx.entry(&entry)?;
            changed = true;
        }
        if changed
        {
            tx.header.modified = when.0;
            tx.commit()?;
        }
        Ok(changed)
    }
}
