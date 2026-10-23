//! Namespace mutations retain their writer and exact source through durable
//! publication. Finish/abort report the actual result after interrupted replies.
use super::*;
pub(super) struct Rename
{
    old_parent:      u32,
    old_handle:      u32,
    cache_published: bool
}
impl<R: Rpc> Filesystem<R>
{
    pub async fn begin_rename(&mut self, volume: u32, id: u32, parent: u32, name: &str,
                              token: u32, when: u64)
                              -> Result<bool>
    {
        self.owner = volume;
        self.writable(volume)?;
        self.writes.reserve(token)?;
        if id <= ROOT || parent < ROOT
        {
            return Err(Cause::Invalid.into());
        }
        self.find(volume, id).await?;
        self.find(volume, parent).await?;
        let before = self.cached_node(volume, id)?.try_clone()?;
        if before.metadata.attributes & 1 != 0
           || u16::from_be_bytes(before.metadata.finder_info[8..10].try_into().unwrap()) & 0x1000
              != 0
        {
            return Err(Cause::Protected.into());
        }
        if self.is_open(volume, id)
        {
            return Err(Cause::Busy.into());
        }
        let normalized = mac_hfs_b::names::normalize(name).map_err(|_| Cause::InvalidName)?;
        forks::host_component(&normalized).map_err(|_| Cause::InvalidName)?;
        if before.parent == parent && before.metadata.name == normalized
        {
            return Ok(false);
        }
        let key = mac_hfs_b::names::comparison(&normalized).map_err(|_| Cause::InvalidName)?;
        self.scan(volume, before.parent).await?;
        self.scan(volume, parent).await?;
        // Prevent folder cycles and mutation of an ancestor with an active
        // snapshot. All ancestry is catalog-local after bounded discovery.
        let mut ancestor = parent;
        for depth in 0..MAX_NODES
        {
            if ancestor == id
            {
                return Err(Cause::Invalid.into());
            }
            if ancestor == ROOT
            {
                break;
            }
            ancestor = self.cached_node(volume, ancestor)?.parent;
            if depth == MAX_NODES - 1
            {
                return Err(Cause::Invalid.into());
            }
        }
        let mount = self.mounts.get_mut(volume).unwrap();
        let target = mount.directories.get(parent).ok_or(Cause::Invalid)?;
        if target.by_name
                 .iter()
                 .any(|other| *other != id && mount.nodes.get(*other).unwrap().key == key)
        {
            return Err(Cause::Conflict.into());
        }
        let old_handle = mount.directories.get(before.parent).ok_or(Cause::NotFound)?.handle;
        let new_handle = target.handle;
        mount.directories
             .get_mut(parent)
             .unwrap()
             .children
             .try_reserve(1)
             .map_err(|_| Cause::NoMemory)?;
        mount.directories
             .get_mut(parent)
             .unwrap()
             .by_name
             .try_reserve(1)
             .map_err(|_| Cause::NoMemory)?;
        let mut node = before.try_clone()?;
        node.parent = parent;
        node.metadata.name = normalized;
        node.host_name = buffer(node.metadata.name.len())?;
        node.host_name.copy_from_slice(node.metadata.name.as_bytes());
        node.key = key;
        let mut payload = buffer(2 + before.host_name.len() + node.host_name.len())?;
        payload[..2].copy_from_slice(&(before.host_name.len() as u16).to_le_bytes());
        payload[2..2 + before.host_name.len()].copy_from_slice(&before.host_name);
        payload[2 + before.host_name.len()..].copy_from_slice(&node.host_name);
        let lock = self.writer_lock(volume).await?;
        let mut expected = 0;
        let prepared: Result<_> =
            async {
                for (folder, handle) in [(before.parent, old_handle), (parent, new_handle)]
                {
                    let actual = self.directory_revision(handle).await?;
                    if self.mounts.get(volume).unwrap().directories.get(folder).unwrap().revision
                       != Some(actual)
                    {
                        return Err(Cause::Stale.into());
                    }
                }
                let source = if before.metadata.kind == Kind::File
                {
                    let (opened, actual) = if before.plain
                    {
                        self.open_plain(volume, old_handle, &before.host_name).await?
                    }
                    else
                    {
                        self.open_file_record(volume, old_handle, &before.host_name).await?
                    };
                    expected = opened;
                    if actual.metadata != before.metadata
                       || before.source.is_some_and(|source| actual.source != Some(source))
                    {
                        return Err(Cause::Stale.into());
                    }
                    opened
                }
                else
                {
                    self.mounts
                        .get(volume)
                        .unwrap()
                        .directories
                        .get(id)
                        .ok_or(Cause::NotFound)?
                        .handle
                };
                let intent =
                    self.call(Op::BeginRename, old_handle, new_handle as u64, source, &payload)
                        .await?
                        .header
                        .object;
                Ok(intent)
            }.await;
        let stage = match prepared
        {
            Ok(stage) => stage,
            Err(mut error) =>
            {
                if expected != 0
                   && let Err(cleanup) = self.close(expected).await
                {
                    error.cleanup = Some(cleanup.cause);
                }
                if let Err(cleanup) = self.release_writer(lock).await
                {
                    error.cleanup = Some(cleanup.cause);
                }
                return Err(error);
            }
        };
        self.writes
            .insert(token,
                    write::Pending { volume,
                                     parent,
                                     parent_handle: new_handle,
                                     stage,
                                     expected,
                                     lock: if self.batch_lock.is_some_and(|(_, held)| held == lock)
                                     {
                                         0
                                     }
                                     else
                                     {
                                         lock
                                     },
                                     cnid: id,
                                     node: Some(node),
                                     header: [0; forks::HEADER_BYTES],
                                     data_chunks: 0,
                                     resource_chunks: 0,
                                     header_written: true,
                                     flushed: true,
                                     published: false,
                                     identity_bound: true,
                                     previous_object: None,
                                     aborting: false,
                                     replacing: true,
                                     revision_updated: false,
                                     parent_updated: true,
                                     volume_updated: false,
                                     when,
                                     locked: true,
                                     metadata_name: None,
                                     source_guard: 0,
                                     root_metadata: None,
                                     revision_directory: None,
                                     notification: None,
                                     notified: true,
                                     removing: false,
                                     rename: Some(Rename { old_parent: before.parent,
                                                           old_handle,
                                                           cache_published: false }) })
            .unwrap_or_else(|_| {
                unreachable!("namespace writer admission reserved before host I/O")
            });
        Ok(true)
    }

    async fn publish_rename_cache(&mut self, volume: u32, token: u32) -> Result<()>
    {
        let p = self.writes.get(token).ok_or(Cause::NotFound)?;
        let r = p.rename.as_ref().ok_or(Cause::Invalid)?;
        if r.cache_published
        {
            return Ok(());
        }
        let (old, parent, id, handle) = (r.old_parent, p.parent, p.cnid, p.parent_handle);
        let node = p.node.as_ref().ok_or(Cause::Invalid)?;
        let mut physical = [0; 255];
        let name = physical.get_mut(..node.host_name.len()).ok_or(Cause::Invalid)?;
        name.copy_from_slice(&node.host_name);
        let described = self.call(Op::Describe, handle, 0, 0, name).await?;
        let source = wire::Description::decode(&described.body).map_err(|_| Cause::Invalid)?;
        // Admission retained both directories and reserved destination capacity.
        // Compute the insertion index before changing either cache.
        let desired = self.writes.get(token).unwrap().node.as_ref().unwrap();
        let mount = self.mounts.get(volume).unwrap();
        let at =
            mount.directories
                 .get(parent)
                 .unwrap()
                 .by_name
                 .iter()
                 .filter(|entry| {
                     **entry != id && mount.nodes.get(**entry).unwrap().key < desired.key
                 })
                 .count();
        let mut node = self.writes.get_mut(token).unwrap().node.take().unwrap();
        node.source = Some(Source { identity: source.identity,
                                    revision: source.revision });
        let mount = self.mounts.get_mut(volume).unwrap();
        let previous = mount.directories.get_mut(old).unwrap();
        previous.children.retain(|entry| *entry != id);
        previous.by_name.retain(|entry| *entry != id);
        *mount.nodes.get_mut(id).unwrap() = node;
        let next = mount.directories.get_mut(parent).unwrap();
        next.children.push(id);
        next.by_name.insert(at, id);
        self.writes.get_mut(token).unwrap().rename.as_mut().unwrap().cache_published = true;
        Ok(())
    }

    pub(super) async fn finish_rename(&mut self, volume: u32, token: u32) -> Result<u32>
    {
        let p = self.writes.get(token).filter(|p| p.volume == volume).ok_or(Cause::NotFound)?;
        let stage = p.stage;
        if !p.published
        {
            self.call(Op::FinishRename, stage, 0, 0, &[]).await?;
            self.writes.get_mut(token).unwrap().published = true;
        }
        self.publish_rename_cache(volume, token).await?;
        self.update_volume_after_publication(token).await?;
        let p = self.writes.get(token).unwrap();
        let r = p.rename.as_ref().unwrap();
        let dirs = [(r.old_parent, r.old_handle), (p.parent, p.parent_handle)];
        let when = p.when;
        for (index, (id, handle)) in dirs.into_iter().enumerate()
        {
            if index == 1 && id == dirs[0].0
            {
                continue;
            }
            self.stamp_directory(volume, id, when).await?;
            let revision = self.directory_revision(handle).await?;
            self.mounts.get_mut(volume).unwrap().directories.get_mut(id).unwrap().revision =
                Some(revision);
        }
        self.cleanup_write(token).await?;
        Ok(self.writes.remove(token).unwrap().cnid)
    }

    pub(super) async fn abort_rename(&mut self, volume: u32, token: u32) -> Result<bool>
    {
        let p = self.writes.get(token).filter(|p| p.volume == volume).ok_or(Cause::NotFound)?;
        if !p.published
        {
            let state = self.call(Op::RenameState, p.stage, 0, 0, &[]).await?.header.value;
            if state > 1
            {
                return Err(Cause::Invalid.into());
            }
            if state == 1
            {
                self.writes.get_mut(token).unwrap().published = true;
            }
        }
        if self.writes.get(token).unwrap().published
        {
            self.finish_rename(volume, token).await?;
            return Ok(true);
        }
        self.writes.get_mut(token).unwrap().aborting = true;
        self.cleanup_write(token).await?;
        self.writes.remove(token);
        Ok(false)
    }
}
