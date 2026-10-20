use super::{mutation::native_protection, *};
fn native_trash<D: WritableBlockDevice>(v: &mut mac_hfs_b::Volume<'_, D>, create: bool, when: u64)
                                        -> MutationResult<mac_hfs_b::Entry>
{
    match v.lookup(2, "Trash")
    {
        Ok(e) if e.directory => Ok(e),
        Ok(_) => Err(MutationError::Exists),
        Err(mac_hfs_b::Error::NotFound) if create =>
        {
            let e = v.create(2, "Trash", true, Timestamp(when)).map_err(mutation_error)?;
            let mut info = e.finder_info;
            info[8..10].copy_from_slice(&0x1000u16.to_be_bytes());
            v.set_metadata(e.id, e.flags, info, e.created, e.modified, e.backup, Timestamp(when))
             .map_err(mutation_error)
        }
        Err(e) => Err(mutation_error(e))
    }
}
fn descendants<D: BlockDevice>(v: &mut mac_hfs_b::Volume<'_, D>, root: u32)
                               -> MutationResult<Vec<mac_hfs_b::Entry>>
{
    let mut out = Vec::new();
    let mut directories = Vec::new();
    directories.try_reserve(1).map_err(|_| MutationError::NoMemory)?;
    directories.push(root);
    let mut index = 0;
    while index < directories.len()
    {
        let parent = directories[index];
        index += 1;
        let mut after = None;
        while let Some(e) = v.next_child(parent, after.as_deref()).map_err(mutation_error)?
        {
            if out.len() >= 4096
            {
                return Err(MutationError::NoMemory);
            }
            after = Some(e.name.clone());
            if e.directory
            {
                directories.try_reserve(1).map_err(|_| MutationError::NoMemory)?;
                directories.push(e.id);
            }
            out.try_reserve(1).map_err(|_| MutationError::NoMemory)?;
            out.push(e);
        }
    }
    Ok(out)
}
impl<D: WritableBlockDevice> Volume<'_, D>
{
    pub fn trash_directory(&mut self, create: bool, when: u64) -> MutationResult<CatalogEntry>
    {
        let e = match &mut self.backend
        {
            Backend::Hfs(v) => v.trash_directory(create, legacy_time(when)?)?.into(),
            Backend::HfsB(v) => native_trash(v, create, when)?.into()
        };
        self.refresh();
        Ok(e)
    }

    pub fn move_to_trash(&mut self, id: u32, when: u64, protected: &[u32])
                         -> MutationResult<CatalogEntry>
    {
        let e = match &mut self.backend
        {
            Backend::Hfs(v) => v.move_to_trash(id, legacy_time(when)?, protected)?.into(),
            Backend::HfsB(v) =>
            {
                native_protection(v, id, protected)?;
                let trash = native_trash(v, true, when)?;
                if id == trash.id
                {
                    return Err(MutationError::Protected);
                }
                v.move_to_trash(id, trash.id, Timestamp(when)).map_err(mutation_error)?.into()
            }
        };
        self.refresh();
        Ok(e)
    }

    pub fn put_away(&mut self, id: u32, when: u64, protected: &[u32])
                    -> MutationResult<CatalogEntry>
    {
        let e = match &mut self.backend
        {
            Backend::Hfs(v) => v.put_away(id, legacy_time(when)?, protected)?.into(),
            Backend::HfsB(v) =>
            {
                native_protection(v, id, protected)?;
                let trash = native_trash(v, false, when)?;
                if v.entry(id).map_err(mutation_error)?.parent != trash.id
                {
                    return Err(MutationError::Invalid);
                }
                v.put_away(id, Timestamp(when)).map_err(mutation_error)?.into()
            }
        };
        self.refresh();
        Ok(e)
    }

    pub fn trash_usage(&mut self) -> MutationResult<(u32, u64)>
    {
        match &mut self.backend
        {
            Backend::Hfs(v) => v.trash_usage(),
            Backend::HfsB(v) =>
            {
                let trash = match native_trash(v, false, 0)
                {
                    Ok(e) => e,
                    Err(MutationError::NotFound) => return Ok((0, 0)),
                    Err(e) => return Err(e)
                };
                let entries = descendants(v, trash.id)?;
                let mut bytes = 0u64;
                for e in &entries
                {
                    bytes = bytes.checked_add((e.data.blocks + e.resources.blocks) * 4096)
                                 .ok_or(MutationError::Full)?;
                }
                Ok((entries.len() as u32, bytes))
            }
        }
    }

    pub fn empty_trash(&mut self, when: u64, protected: &[u32]) -> MutationResult<u32>
    {
        let count = match &mut self.backend
        {
            Backend::Hfs(v) => v.empty_trash(legacy_time(when)?, protected)?,
            Backend::HfsB(v) =>
            {
                let trash = match native_trash(v, false, when)
                {
                    Ok(e) => e,
                    Err(MutationError::NotFound) => return Ok(0),
                    Err(e) => return Err(e)
                };
                let entries = descendants(v, trash.id)?;
                // Preflight the entire set before deleting any protected child.
                for e in &entries
                {
                    native_protection(v, e.id, protected)?;
                    if e.flags & 1 != 0
                    {
                        return Err(MutationError::Protected);
                    }
                }
                // The core batches this postorder set into one metadata commit.
                let mut ids = Vec::new();
                ids.try_reserve_exact(entries.len()).map_err(|_| MutationError::NoMemory)?;
                ids.extend(entries.iter().rev().map(|e| e.id));
                v.remove_many(&ids, Timestamp(when)).map_err(mutation_error)?;
                entries.len() as u32
            }
        };
        self.refresh();
        Ok(count)
    }
}
