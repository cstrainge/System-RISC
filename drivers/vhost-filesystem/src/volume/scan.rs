//! Resumable directory discovery. Persist a consumed host entry until its
//! checked metadata has been published, including across errors/cancellation.
use super::*;
impl<R: Rpc> Filesystem<R>
{
    pub(super) async fn directory_revision(&mut self, handle: u32) -> Result<[u8; 32]>
    {
        self.call(Op::Stat, handle, 0, 0, &[])
            .await?
            .body
            .as_slice()
            .try_into()
            .map_err(|_| Cause::Invalid.into())
    }

    /// At most one physical child per step. A caller can return to its event
    /// loop between steps; it need not wait for a whole directory enumeration.
    pub async fn scan_step(&mut self, volume: u32, parent: u32) -> Result<bool>
    {
        self.owner = volume;
        let mount = self.mounts.get(volume).ok_or(Cause::NotFound)?;
        if mount.ejecting
        {
            return Err(Cause::Busy.into());
        }
        if mount.invalidated
        {
            return Err(Cause::Stale.into());
        }
        let directory = mount.directories.get(parent).ok_or(Cause::NotFound)?;
        let (handle, enumeration, revision, complete) =
            (directory.handle, directory.enumeration, directory.revision, directory.complete);
        let actual = self.directory_revision(handle).await?;
        if revision.is_some_and(|revision| revision != actual)
        {
            return Err(Cause::Stale.into());
        }
        if complete
        {
            if revision.is_none()
            {
                self.mounts
                    .get_mut(volume)
                    .unwrap()
                    .directories
                    .get_mut(parent)
                    .unwrap()
                    .revision = Some(actual);
            }
            return Ok(true);
        }
        let enumeration = if let Some(enumeration) = enumeration
        {
            enumeration
        }
        else
        {
            let reply = self.call(Op::ListBegin, handle, 0, 0, &[]).await?;
            if reply.header.object == 0
            {
                return Err(Cause::Invalid.into());
            }
            let directory =
                self.mounts.get_mut(volume).unwrap().directories.get_mut(parent).unwrap();
            directory.enumeration = Some(reply.header.object);
            directory.revision = Some(actual);
            reply.header.object
        };
        if self.mounts.get(volume).unwrap().directories.get(parent).unwrap().pending.is_none()
        {
            let reply = self.call(Op::ListNext, enumeration, 0, 0, &[]).await?;
            if reply.body.is_empty()
            {
                self.close(enumeration).await?;
                let directory =
                    self.mounts.get_mut(volume).unwrap().directories.get_mut(parent).unwrap();
                directory.enumeration = None;
                directory.complete = true;
                return Ok(true);
            }
            // Persist the consumed entry before another await. Cancellation or
            // a failed metadata read must retry this child, never skip it and
            // publish an incomplete catalog as a successful enumeration.
            self.mounts.get_mut(volume).unwrap().directories.get_mut(parent).unwrap().pending =
                Some(PendingChild { name:      reply.body,
                                    kind:      reply.header.value,
                                    directory: None });
        }
        let child = self.mounts
                        .get(volume)
                        .unwrap()
                        .directories
                        .get(parent)
                        .unwrap()
                        .pending
                        .as_ref()
                        .unwrap();
        // Host components are bounded by the transport contract. Copy the small
        // name to the stack so no directory borrow crosses a lower callback.
        let mut name_bytes = [0; 255];
        let name = name_bytes.get_mut(..child.name.len()).ok_or(Cause::Invalid)?;
        name.copy_from_slice(&child.name);
        let host_kind = child.kind;
        let existing_directory = child.directory;
        if name == forks::annotation::DIRECTORY.as_bytes()
        {
            if host_kind != 2
            {
                return Err(Cause::Invalid.into());
            }
            self.mounts.get_mut(volume).unwrap().directories.get_mut(parent).unwrap().pending =
                None;
            return Ok(false);
        }
        if name == forks::ROOT_RECORD.as_bytes()
           || name == forks::DIRECTORY_RECORD.as_bytes()
           || name == b".system-risc-lock"
        {
            if host_kind != 1
            {
                return Err(Cause::Invalid.into());
            }
            self.mounts.get_mut(volume).unwrap().directories.get_mut(parent).unwrap().pending =
                None;
            return Ok(false);
        }
        let kind = match host_kind
        {
            1 => Kind::File,
            2 => Kind::Directory,
            _ => return Err(Cause::Invalid.into())
        };
        let directory = if host_kind == 2
        {
            if let Some(handle) = existing_directory
            {
                Some(handle)
            }
            else
            {
                let opened = self.call(Op::OpenDirectory, handle, 0, 0, name).await?.header.object;
                self.mounts
                    .get_mut(volume)
                    .unwrap()
                    .directories
                    .get_mut(parent)
                    .unwrap()
                    .pending
                    .as_mut()
                    .unwrap()
                    .directory = Some(opened);
                Some(opened)
            }
        }
        else
        {
            None
        };
        let mut node = if let Some(directory) = directory
        {
            self.directory_node(volume, handle, name, directory).await?
        }
        else
        {
            self.candidate(volume, handle, name).await?
        };
        node.host_name = buffer(name.len())?;
        node.host_name.copy_from_slice(name);
        node.metadata.name = mac_hfs_b::names::normalize(core::str::from_utf8(name).map_err(|_| Cause::Invalid)?).map_err(|_| Cause::Invalid)?;
        node.key = mac_hfs_b::names::comparison(&node.metadata.name).map_err(|_| Cause::Invalid)?;
        node.parent = parent;
        let result = (|| {
            let mount = self.mounts.get_mut(volume).ok_or(Cause::NotFound)?;
            if node.metadata.kind != kind || node.metadata.volume != mount.info.metadata.volume
            {
                return Err(Cause::Invalid.into());
            }
            let id = node.metadata.catalog_id;
            if mount.nodes.get(id).is_some()
            {
                return Err(Cause::Conflict.into());
            }
            let name_at =
                mount.directories
                     .get(parent)
                     .unwrap()
                     .by_name
                     .binary_search_by(|id| mount.nodes.get(*id).unwrap().key.cmp(&node.key))
                     .map_or_else(Ok, |_| Err(Cause::Conflict))?;
            mount.directories
                 .get_mut(parent)
                 .unwrap()
                 .by_name
                 .try_reserve(1)
                 .map_err(|_| Cause::NoMemory)?;
            mount.directories
                 .get_mut(parent)
                 .unwrap()
                 .children
                 .try_reserve(1)
                 .map_err(|_| Cause::NoMemory)?;
            if let Some(directory) = directory
            {
                mount.directories.insert(id, Directory::new(directory))?;
            }
            if let Err(error) = mount.nodes.insert(id, node)
            {
                if directory.is_some()
                {
                    mount.directories.remove(id);
                }
                return Err(error);
            }
            let directory = mount.directories.get_mut(parent).unwrap();
            directory.children.push(id);
            directory.by_name.insert(name_at, id);
            Ok(())
        })();
        result?;
        // The node and optional directory capability now have durable catalog
        // owners. Clear in the same synchronous step; there is no await gap.
        self.mounts.get_mut(volume).unwrap().directories.get_mut(parent).unwrap().pending = None;
        Ok(false)
    }

    pub async fn scan(&mut self, volume: u32, parent: u32) -> Result<()>
    {
        while !self.scan_step(volume, parent).await?
        {
            yield_once().await;
        }
        Ok(())
    }

    pub fn child(&self, volume: u32, parent: u32, index: usize) -> Result<&Node>
    {
        self.info(volume)?;
        let mount = self.mounts.get(volume).ok_or(Cause::NotFound)?;
        let id = *mount.directories
                       .get(parent)
                       .and_then(|directory| directory.children.get(index))
                       .ok_or(Cause::NotFound)?;
        mount.nodes.get(id).ok_or_else(|| Cause::NotFound.into())
    }

    pub(crate) fn cached_child_count(&self, volume: u32, parent: u32) -> Result<Option<u32>>
    {
        self.info(volume)?;
        let directory =
            self.mounts.get(volume).unwrap().directories.get(parent).ok_or(Cause::NotFound)?;
        Ok(directory.complete.then_some(directory.children.len() as u32))
    }

    pub async fn lookup(&mut self, volume: u32, parent: u32, name: &str) -> Result<&Node>
    {
        let folded = mac_hfs_b::names::comparison(name).map_err(|_| Cause::Invalid)?;
        self.scan(volume, parent).await?;
        let mount = self.mounts.get(volume).unwrap();
        let directory = mount.directories.get(parent).unwrap();
        let at = directory.by_name
                          .binary_search_by(|id| mount.nodes.get(*id).unwrap().key.cmp(&folded))
                          .map_err(|_| Cause::NotFound)?;
        Ok(mount.nodes.get(directory.by_name[at]).unwrap())
    }
}

impl<R: Rpc> Filesystem<R>
{
    /// Probe only the requested directory. Existing open snapshots retain their
    /// own leases; removed catalog capabilities become acknowledged orphans.
    pub async fn check_directory(&mut self, volume: u32, parent: u32) -> Result<(bool, [u8; 32])>
    {
        self.owner = volume;
        if self.writes.entries.iter().any(|(_, p)| p.volume == volume) || self.group_busy(volume)
        {
            return Err(Cause::Busy.into());
        }
        let mount = self.mounts.get(volume).ok_or(Cause::NotFound)?;
        let directory = mount.directories.get(parent).ok_or(Cause::NotFound)?;
        let (handle, previous) = (directory.handle, directory.revision);
        let actual = self.directory_revision(handle).await?;
        let changed = previous.is_some_and(|revision| revision != actual);
        let mount = self.mounts.get_mut(volume).unwrap();
        if changed
        {
            if parent == ROOT
            {
                mount.annotations = annotations::Directory::Unknown;
            }
            // Reserve before changing ownership. Descendant count is bounded by
            // MAX_NODES and this traversal performs no transport operations.
            let mut removed = Vec::new();
            removed.try_reserve_exact(mount.nodes.entries.len()).map_err(|_| Cause::NoMemory)?;
            for &id in &mount.directories.get(parent).unwrap().children
            {
                removed.push(id);
            }
            let mut at = 0;
            while let Some(&id) = removed.get(at)
            {
                if let Some(directory) = mount.directories.get(id)
                {
                    removed.extend_from_slice(&directory.children);
                }
                at += 1;
            }
            for id in removed
            {
                mount.nodes.remove(id);
                mount.directories.remove(id);
            }
            *mount.directories.get_mut(parent).unwrap() = Directory::new(handle);
            self.needs_cleanup = true;
        }
        mount.directories.get_mut(parent).unwrap().revision = Some(actual);
        Ok((changed, actual))
    }
}
