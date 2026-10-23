use super::*;
fn native_destination<D: BlockDevice>(v: &mut mac_hfs_b::Volume<'_, D>, parent: u32,
                                      name: Option<&str>)
                                      -> MutationResult<()>
{
    if parent==2 && name.map(mac_hfs_b::names::comparison).transpose().map_err(mutation_error)?
       ==Some(mac_hfs_b::names::comparison(mac_hfs_b::STAGING_DIRECTORY).map_err(mutation_error)?)
    {
        return Err(MutationError::Protected);
    }
    match v.lookup(2, mac_hfs_b::STAGING_DIRECTORY)
    {
        Ok(entry) if entry.id == parent => Err(MutationError::Protected),
        Ok(_) | Err(mac_hfs_b::Error::NotFound) => Ok(()),
        Err(error) => Err(mutation_error(error))
    }
}
pub(super) fn native_protection<D: BlockDevice>(v: &mut mac_hfs_b::Volume<'_, D>, id: u32,
                                                protected: &[u32])
                                                -> MutationResult<()>
{
    if id == 2
    {
        return Err(MutationError::Protected);
    }
    let e = v.entry(id).map_err(mutation_error)?;
    native_destination(v, e.parent, Some(&e.name))?;
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
    pub fn move_entries(&mut self, entries: &[(u32, u32, [u8; 32], bool)], when: u64,
                        protected: &[u32])
                        -> MutationResult<bool>
    {
        let changed = match &mut self.backend
        {
            Backend::Hfs(v) => v.move_entries(entries, legacy_time(when)?, protected)?,
            Backend::HfsB(v) =>
            {
                for &(id, parent, _, _) in entries
                {
                    native_destination(v, parent, None)?;
                    let entry = v.entry(id).map_err(mutation_error)?;
                    native_destination(v, entry.parent, Some(&entry.name))?;
                    if entry.parent != parent
                    {
                        native_protection(v, id, protected)?;
                    }
                }
                v.move_entries(entries, Timestamp(when)).map_err(mutation_error)?
            }
        };
        self.refresh();
        Ok(changed)
    }

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
                native_destination(v,
                                   spec.parent,
                                   Some(core::str::from_utf8(spec.name).map_err(|_| {
                                                                           MutationError::Invalid
                                                                       })?))?;
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
                native_destination(v,
                                   parent,
                                   Some(core::str::from_utf8(name).map_err(|_| {
                                                                      MutationError::Invalid
                                                                  })?))?;
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

    pub fn create_private_directory(&mut self, parent: u32, name: &str, when: u64)
                                    -> MutationResult<CatalogEntry>
    {
        let Backend::HfsB(volume) = &mut self.backend
        else
        {
            return Err(MutationError::Unsupported);
        };
        native_destination(volume, parent, Some(name))?;
        let mut finder_info = [0; 32];
        finder_info[8..10].copy_from_slice(&0x4000u16.to_be_bytes());
        let entry =
            volume.create_with_finder_info(parent, name, true, finder_info, Timestamp(when))
                  .map_err(mutation_error)?;
        self.refresh();
        Ok(entry.into())
    }

    /// Coordinator-only rollback primitive; the resident service owns the
    /// mutation lease and does not expose this through the public trap API.
    pub fn restore_modified(&mut self, id: u32, modified: u64, volume_modified: Option<u64>)
                            -> MutationResult<CatalogEntry>
    {
        let Backend::HfsB(volume) = &mut self.backend
        else
        {
            return Err(MutationError::Unsupported);
        };
        let entry =
            volume.restore_modified(id, Timestamp(modified), volume_modified.map(Timestamp))
                  .map_err(mutation_error)?;
        self.refresh();
        Ok(entry.into())
    }

    /// Restore all object properties atomically without modifying either fork.
    pub fn set_properties(&mut self, id: u32, properties: Properties<'_>, when: u64,
                          protected: &[u32])
                          -> MutationResult<(CatalogEntry, bool)>
    {
        let Backend::HfsB(volume) = &mut self.backend
        else
        {
            return Err(MutationError::Unsupported);
        };
        native_protection(volume, id, protected)?;
        let (entry, changed) =
            volume.set_properties(id, properties, Timestamp(when)).map_err(mutation_error)?;
        self.refresh();
        Ok((entry.into(), changed))
    }

    /// Delete a file or empty directory; recursive removal is stepped by the
    /// resident transaction owner, rather than hidden in a filesystem call.
    pub fn remove(&mut self, id: u32, when: u64, protected: &[u32]) -> MutationResult<()>
    {
        let Backend::HfsB(volume) = &mut self.backend
        else
        {
            return Err(MutationError::Unsupported);
        };
        native_protection(volume, id, protected)?;
        let entry = volume.entry(id).map_err(mutation_error)?;
        if entry.directory && entry.children != 0
        {
            return Err(MutationError::NotEmpty);
        }
        if entry.flags & 1 != 0
           || u16::from_be_bytes(entry.finder_info[8..10].try_into().unwrap()) & 0x1000 != 0
        {
            return Err(MutationError::Protected);
        }
        volume.remove(id, Timestamp(when)).map_err(mutation_error)?;
        self.refresh();
        Ok(())
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
                native_destination(v,
                                   e.parent,
                                   Some(core::str::from_utf8(name).map_err(|_| {
                                                                      MutationError::Invalid
                                                                  })?))?;
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
                native_destination(v,
                                   parent,
                                   Some(core::str::from_utf8(&before.name).map_err(|_| {
                                                                              MutationError::Invalid
                                                                          })?))?;
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
                native_destination(v,
                                   before.parent,
                                   Some(core::str::from_utf8(&before.name).map_err(|_| {
                                                                              MutationError::Invalid
                                                                          })?))?;
                v.set_finder_metadata(id, before.flags, info, Timestamp(when))
                 .map_err(mutation_error)?
                 .into()
            }
        };
        self.refresh();
        Ok((entry, changed))
    }

    /// Persist only the directory window fields; leave placement, flags,
    /// comments, object dates and unrelated Finder bytes unchanged.
    pub fn set_folder_state(&mut self, id: u32, state: [u8; 32], when: u64)
                            -> MutationResult<(CatalogEntry, bool)>
    {
        let before =
            self.entry(id).map_err(|_| MutationError::Io)?.ok_or(MutationError::NotFound)?;
        if !before.directory
        {
            return Err(MutationError::Invalid);
        }
        let mut info = before.finder_info;
        info[..8].copy_from_slice(&state[..8]);
        info[14..20].copy_from_slice(&state[14..20]);
        if info == before.finder_info
        {
            return Ok((before, false));
        }
        let entry = match &mut self.backend
        {
            Backend::Hfs(v) => v.set_finder_info(id, info, legacy_time(when)?)?.into(),
            Backend::HfsB(v) =>
            {
                native_destination(v,
                                   before.parent,
                                   Some(core::str::from_utf8(&before.name).map_err(|_| {
                                                                              MutationError::Invalid
                                                                          })?))?;
                v.set_finder_metadata(id, before.flags, info, Timestamp(when))
                 .map_err(mutation_error)?
                 .into()
            }
        };
        self.refresh();
        Ok((entry, true))
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
