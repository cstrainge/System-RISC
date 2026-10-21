use super::*;
pub(super) fn native_protection<D: BlockDevice>(v: &mut mac_hfs_b::Volume<'_, D>, id: u32,
                                                protected: &[u32])
                                                -> MutationResult<()>
{
    if id == 2
    {
        return Err(MutationError::Protected);
    }
    let e = v.entry(id).map_err(mutation_error)?;
    if e.parent == v.header().blessed
    {
        let name = mac_hfs_b::names::comparison(&e.name).map_err(mutation_error)?;
        if name == "system" || name == "finder"
        {
            return Err(MutationError::Protected);
        }
    }
    for target in core::iter::once(v.header().blessed).chain(protected.iter().copied())
    {
        if target == 0
        {
            continue;
        }
        let mut current = target;
        for depth in 0..4096
        {
            if current == id
            {
                return Err(if target == v.header().blessed
                           {
                               MutationError::Protected
                           }
                           else
                           {
                               MutationError::Busy
                           });
            }
            if current == 2
            {
                break;
            }
            if depth == 4095
            {
                return Err(MutationError::NoMemory);
            }
            current = v.entry(current).map_err(mutation_error)?.parent;
        }
    }
    Ok(())
}
impl<D: WritableBlockDevice> Volume<'_, D>
{
    pub fn save_data(&mut self, spec: DataSave<'_>, data: &[u8], protected: &[u32])
                     -> MutationResult<CatalogEntry>
    {
        let entry = match &mut self.backend
        {
            Backend::Hfs(v) =>
            {
                v.save_data(mac_hfs::DataSave { parent:    spec.parent,
                                                name:      &legacy_name(spec.name)?,
                                                expected:  spec.expected,
                                                file_type: spec.file_type,
                                                creator:   spec.creator,
                                                when:      legacy_time(spec.when)? },
                            data,
                            protected)?
                 .into()
            }
            Backend::HfsB(v) =>
            {
                if spec.expected != 0
                {
                    native_protection(v, spec.expected, protected)?;
                }
                v.save_data(mac_hfs_b::DataSave {parent:spec.parent,name:core::str::from_utf8(spec.name).map_err(|_|MutationError::Invalid)?,
                    expected:spec.expected,file_type:spec.file_type,creator:spec.creator,when:Timestamp(spec.when)},data).map_err(mutation_error)?.into()
            }
        };
        self.refresh();
        Ok(entry)
    }

    pub fn create_folder(&mut self, parent: u32, name: &[u8], when: u64)
                         -> MutationResult<CatalogEntry>
    {
        let entry = match &mut self.backend
        {
            Backend::Hfs(v) =>
            {
                v.create_folder(parent, &legacy_name(name)?, legacy_time(when)?)?.into()
            }
            Backend::HfsB(v) =>
            {
                v.create(parent,
                         core::str::from_utf8(name).map_err(|_| MutationError::Invalid)?,
                         true,
                         Timestamp(when))
                 .map_err(mutation_error)?
                 .into()
            }
        };
        self.refresh();
        Ok(entry)
    }

    pub fn rename(&mut self, id: u32, name: &[u8], when: u64, protected: &[u32])
                  -> MutationResult<CatalogEntry>
    {
        let entry = match &mut self.backend
        {
            Backend::Hfs(v) =>
            {
                v.rename(id, &legacy_name(name)?, legacy_time(when)?, protected)?.into()
            }
            Backend::HfsB(v) =>
            {
                let e = v.entry(id).map_err(mutation_error)?;
                native_protection(v, id, protected)?;
                if u16::from_be_bytes(e.finder_info[8..10].try_into().unwrap()) & 0x1000 != 0
                {
                    return Err(MutationError::Protected);
                }
                v.rename(id,
                         e.parent,
                         core::str::from_utf8(name).map_err(|_| MutationError::Invalid)?,
                         Timestamp(when))
                 .map_err(mutation_error)?
                 .into()
            }
        };
        self.refresh();
        Ok(entry)
    }

    /// Move an existing identity without rebuilding it or copying fork data.
    /// Report a no-op so the resident service can keep scans and watches quiet.
    pub fn move_into(&mut self, id: u32, parent: u32, when: u64, protected: &[u32])
                     -> MutationResult<(CatalogEntry, bool)>
    {
        let before =
            self.entry(id).map_err(|_| MutationError::Io)?.ok_or(MutationError::NotFound)?;
        let changed = before.parent != parent;
        let entry = match &mut self.backend
        {
            Backend::Hfs(v) => v.move_into(id, parent, legacy_time(when)?, protected)?.into(),
            Backend::HfsB(v) =>
            {
                native_protection(v, id, protected)?;
                if before.flags & 1 != 0
                   || u16::from_be_bytes(before.finder_info[8..10].try_into().unwrap()) & 0x1000
                      != 0
                {
                    return Err(MutationError::Protected);
                }
                if changed
                {
                    v.rename(id,
                             parent,
                             core::str::from_utf8(&before.name).map_err(|_| MutationError::Io)?,
                             Timestamp(when))
                     .map_err(mutation_error)?
                     .into()
                }
                else
                {
                    before
                }
            }
        };
        self.refresh();
        Ok((entry, changed))
    }

    /// Return the unchanged entry without a write when placement already matches.
    /// The bool reports an actual metadata change to notification publishers.
    pub fn set_icon_location(&mut self, id: u32, location: Option<(i16, i16)>, when: u64)
                             -> MutationResult<(CatalogEntry, bool)>
    {
        if location == Some((-1, -1))
        {
            return Err(MutationError::Invalid);
        }
        let before =
            self.entry(id).map_err(|_| MutationError::Io)?.ok_or(MutationError::NotFound)?;
        let mut info = before.finder_info;
        let flags = u16::from_be_bytes(info[8..10].try_into().unwrap());
        info[8..10].copy_from_slice(&(if location.is_some()
                                     {
                                         flags | 0x0100
                                     }
                                     else
                                     {
                                         flags & !0x0100
                                     }).to_be_bytes());
        let (h, v) = location.unwrap_or((-1, -1));
        info[10..12].copy_from_slice(&v.to_be_bytes());
        info[12..14].copy_from_slice(&h.to_be_bytes());
        let changed = info != before.finder_info;
        let entry = match &mut self.backend
        {
            Backend::Hfs(v) => v.set_finder_info(id, info, legacy_time(when)?)?.into(),
            Backend::HfsB(v) =>
            {
                v.set_finder_metadata(id, before.flags, info, Timestamp(when))
                 .map_err(mutation_error)?
                 .into()
            }
        };
        self.refresh();
        Ok((entry, changed))
    }

    pub fn set_locked(&mut self, id: u32, locked: bool, when: u64, protected: &[u32])
                      -> MutationResult<CatalogEntry>
    {
        let entry = match &mut self.backend
        {
            Backend::Hfs(v) => v.set_locked(id, locked, legacy_time(when)?, protected)?.into(),
            Backend::HfsB(v) =>
            {
                native_protection(v, id, protected)?;
                let e = v.entry(id).map_err(mutation_error)?;
                if e.directory
                {
                    return Err(MutationError::Invalid);
                }
                v.set_finder_metadata(id,
                                      if locked { e.flags | 1 } else { e.flags & !1 },
                                      e.finder_info,
                                      Timestamp(when))
                 .map_err(mutation_error)?
                 .into()
            }
        };
        self.refresh();
        Ok(entry)
    }

    pub fn set_stationery(&mut self, id: u32, enabled: bool, when: u64, protected: &[u32])
                          -> MutationResult<CatalogEntry>
    {
        let entry = match &mut self.backend
        {
            Backend::Hfs(v) => v.set_stationery(id, enabled, legacy_time(when)?, protected)?.into(),
            Backend::HfsB(v) =>
            {
                native_protection(v, id, protected)?;
                let e = v.entry(id).map_err(mutation_error)?;
                if e.directory
                {
                    return Err(MutationError::Invalid);
                }
                if e.flags & 1 != 0
                {
                    return Err(MutationError::Protected);
                }
                let mut info = e.finder_info;
                let flags = u16::from_be_bytes(info[8..10].try_into().unwrap());
                info[8..10].copy_from_slice(&(if enabled
                                             {
                                                 flags | 0x800
                                             }
                                             else
                                             {
                                                 flags & !0x800
                                             }).to_be_bytes());
                v.set_finder_metadata(id, e.flags, info, Timestamp(when))
                 .map_err(mutation_error)?
                 .into()
            }
        };
        self.refresh();
        Ok(entry)
    }

    pub fn comment(&mut self, id: u32) -> MutationResult<Vec<u8>>
    {
        match &mut self.backend
        {
            Backend::Hfs(v) => Ok(mac_formats::text::decode(&v.comment(id)?).into_bytes()),
            Backend::HfsB(v) => Ok(v.comment(id).map_err(mutation_error)?.into_bytes())
        }
    }

    pub fn set_comment(&mut self, id: u32, comment: &[u8], when: u64, protected: &[u32])
                       -> MutationResult<()>
    {
        match &mut self.backend
        {
            Backend::Hfs(v) =>
            {
                v.set_comment(id, &legacy_name(comment)?, legacy_time(when)?, protected)?
            }
            Backend::HfsB(v) =>
            {
                native_protection(v, id, protected)?;
                v.set_comment(id,
                              core::str::from_utf8(comment).map_err(|_| MutationError::Invalid)?,
                              Timestamp(when))
                 .map_err(mutation_error)?;
            }
        }
        self.refresh();
        Ok(())
    }
}
