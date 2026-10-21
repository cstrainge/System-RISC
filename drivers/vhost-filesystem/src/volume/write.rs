//! Bounded two-fork staging. Catalog IDs are reserved durably before publication;
//! a root writer lease serializes case-folded name checks and final publication.
use super::*;

pub struct Create<'a>
{
    pub volume:          u32,
    pub parent:          u32,
    pub token:           u32,
    pub name:            &'a str,
    pub data_length:     u64,
    pub resource_length: u64,
    pub when:            u64
}
pub struct Replace
{
    pub volume:          u32,
    pub node:            u32,
    pub token:           u32,
    pub data_length:     u64,
    pub resource_length: u64,
    pub when:            u64
}
pub(super) struct Pending
{
    pub(super) volume: u32,
    parent:            u32,
    parent_handle:     u32,
    stage:             u32,
    expected:          u32,
    lock:              u32,
    cnid:              u32,
    node:              Option<Node>,
    header:            [u8; forks::HEADER_BYTES],
    data_chunks:       u64,
    resource_chunks:   u64,
    header_written:    bool,
    flushed:           bool,
    published:         bool,
    aborting:          bool,
    replacing:         bool,
    revision_updated:  bool,
    parent_updated:    bool,
    locked:            bool
}
impl Pending
{
    pub(super) fn references(&self, id: u32) -> bool
    {
        self.stage == id || self.expected == id || self.lock == id
    }
}
impl<R: Rpc> Filesystem<R>
{
    async fn writer_lock(&mut self, volume: u32) -> Result<u32>
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
        if mount.info.read_only
        {
            return Err(Cause::ReadOnly.into());
        }
        let root = mount.root;
        let reply = self.call(Op::OpenLock, root, 0, 0, b".system-risc-lock").await?;
        if reply.header.object == 0
        {
            return Err(Cause::Invalid.into());
        }
        let lock = reply.header.object;
        let result = self.call(Op::Lock, lock, 0, wire::LOCK_EXCLUSIVE, &[]).await;
        if let Err(error) = result
        {
            return self.cleanup(lock, Err(error)).await;
        }
        Ok(lock)
    }

    async fn release_writer(&mut self, lock: u32) -> Result<()>
    {
        let result = self.call(Op::Unlock, lock, 0, 0, &[]).await.map(|_| ());
        self.cleanup(lock, result).await
    }

    async fn publish_record(&mut self, parent: u32, name: &[u8], expected: u32,
                            metadata: &Metadata)
                            -> Result<()>
    {
        let (mut bytes, layout) = forks::encode_metadata(metadata, 0, 0)?;
        let header: [u8; forks::HEADER_BYTES] = bytes[..forks::HEADER_BYTES].try_into().unwrap();
        bytes[..forks::HEADER_BYTES].fill(0);
        let stage = self.call(Op::BeginWrite, parent, 0, 0, &[]).await?.header.object;
        if stage == 0
        {
            return Err(Cause::Invalid.into());
        }
        let result = async {
                         self.call(Op::SetLength, stage, layout.total, 0, &[]).await?;
                         self.call(Op::Write, stage, 0, 0, &bytes).await?;
                         self.call(Op::Write, stage, 0, 0, &header).await?;
                         self.call(Op::Flush, stage, 0, 0, &[]).await?;
                         self.call(Op::Commit, stage, 0, expected, name).await?;
                         Ok(())
                     }.await;
        if let Err(error) = result
        {
            return self.cleanup(stage, Err(error)).await;
        }
        Ok(())
    }

    async fn update_root(&mut self, volume: u32, when: u64, reserve: bool) -> Result<u32>
    {
        let mount = self.mounts.get(volume).ok_or(Cause::NotFound)?;
        let root = mount.root;
        let identity = mount.info.metadata.volume;
        let (old, node) = self.open_record(root, forks::ROOT_RECORD.as_bytes()).await?;
        let result =
            async {
                if node.metadata.kind != Kind::Root || node.metadata.volume != identity
                {
                    return Err(Cause::Stale.into());
                }
                let mut metadata = node.metadata;
                let id = metadata.next_catalog_id;
                if reserve
                {
                    metadata.next_catalog_id = id.checked_add(1).ok_or(Cause::NoMemory)?;
                }
                metadata.revision = metadata.revision.checked_add(1).ok_or(Cause::NoMemory)?;
                metadata.modified = when;
                self.publish_record(root, forks::ROOT_RECORD.as_bytes(), old, &metadata).await?;
                let revision = self.directory_revision(root).await?;
                let mount = self.mounts.get_mut(volume).unwrap();
                mount.info.metadata = metadata.clone();
                mount.nodes.get_mut(ROOT).unwrap().metadata = metadata;
                mount.directories.get_mut(ROOT).unwrap().revision = Some(revision);
                Ok(id)
            }.await;
        self.cleanup(old, result).await
    }

    async fn stage_file(&mut self, volume: u32, parent: u32, lock: u32, expected: u32, node: Node,
                        replacing: bool)
                        -> Result<Pending>
    {
        let parent_handle = self.mounts
                                .get(volume)
                                .and_then(|mount| mount.directories.get(parent))
                                .ok_or(Cause::NotFound)?
                                .handle;
        let (mut bytes, layout) = forks::encode_metadata(&node.metadata, node.layout.data.length,
                                                         node.layout.resources.length)?;
        let header = bytes[..forks::HEADER_BYTES].try_into().unwrap();
        bytes[..forks::HEADER_BYTES].fill(0);
        let stage = self.call(Op::BeginWrite, parent_handle, 0, 0, &[]).await?.header.object;
        if stage == 0
        {
            return Err(Cause::Invalid.into());
        }
        let result = async {
                         self.call(Op::SetLength, stage, layout.total, 0, &[]).await?;
                         self.call(Op::Write, stage, 0, 0, &bytes).await?;
                         Ok(())
                     }.await;
        if let Err(error) = result
        {
            return self.cleanup(stage, Err(error)).await;
        }
        let cnid = node.metadata.catalog_id;
        let revision = self.directory_revision(parent_handle).await?;
        self.mounts.get_mut(volume).unwrap().directories.get_mut(parent).unwrap().revision =
            Some(revision);
        Ok(Pending { volume,
                     parent,
                     parent_handle,
                     stage,
                     expected,
                     lock,
                     cnid,
                     node: Some(node),
                     header,
                     data_chunks: 0,
                     resource_chunks: 0,
                     header_written: false,
                     flushed: false,
                     published: false,
                     aborting: false,
                     replacing,
                     revision_updated: false,
                     parent_updated: false,
                     locked: true })
    }

    pub async fn begin_create(&mut self, request: Create<'_>) -> Result<u32>
    {
        let Create { volume,
                     parent,
                     token,
                     name,
                     data_length,
                     resource_length,
                     when } = request;
        self.writes.reserve(token)?;
        let normalized = mac_hfs_b::names::normalize(name).map_err(|_| Cause::Invalid)?;
        let key = mac_hfs_b::names::comparison(&normalized).map_err(|_| Cause::Invalid)?;
        self.scan(volume, parent).await?;
        let mount = self.mounts.get(volume).unwrap();
        if mount.directories
                .get(parent)
                .unwrap()
                .by_name
                .binary_search_by(|id| mount.nodes.get(*id).unwrap().key.cmp(&key))
                .is_ok()
        {
            return Err(Cause::Conflict.into());
        }
        // Validate lengths/name before reserving a durable catalog identity.
        let mut metadata =
            Metadata::new(Kind::File, 16, mount.info.metadata.volume, &normalized, when)?;
        let layout = Layout::new(&metadata, data_length, resource_length)?;
        let lock = self.writer_lock(volume).await?;
        let result =
            async {
                // Revalidate the directory snapshot while holding the writer lease.
                let parent_handle =
                    self.mounts.get(volume).unwrap().directories.get(parent).unwrap().handle;
                let actual = self.directory_revision(parent_handle).await?;
                let revision =
                    self.mounts.get(volume).unwrap().directories.get(parent).unwrap().revision;
                if revision != Some(actual)
                {
                    return Err(Cause::Stale.into());
                }
                let id = self.update_root(volume, when, true).await?;
                metadata.catalog_id = id;
                metadata.identity = forks::catalog_identity(metadata.volume, id)?;
                let node = Node { parent,
                                  metadata,
                                  layout,
                                  key };
                self.stage_file(volume, parent, lock, 0, node, false).await
            }.await;
        let pending = match result
        {
            Ok(pending) => pending,
            Err(mut error) =>
            {
                if let Err(cleanup) = self.release_writer(lock).await
                {
                    error.cleanup = Some(cleanup.cause);
                }
                return Err(error);
            }
        };
        let id = pending.node.as_ref().unwrap().metadata.catalog_id;
        self.writes
            .insert(token, pending)
            .unwrap_or_else(|_| unreachable!("write admission was reserved before host I/O"));
        Ok(id)
    }

    pub async fn begin_replace(&mut self, request: Replace) -> Result<()>
    {
        let Replace { volume,
                      node,
                      token,
                      data_length,
                      resource_length,
                      when } = request;
        self.writes.reserve(token)?;
        if self.is_open(volume, node)
        {
            return Err(Cause::Busy.into());
        }
        let cached = self.cached_node(volume, node)?.clone();
        if cached.metadata.kind != Kind::File
        {
            return Err(Cause::Invalid.into());
        }
        if cached.metadata.attributes & 1 != 0
        {
            return Err(Cause::ReadOnly.into());
        }
        let layout = Layout::new(&cached.metadata, data_length, resource_length)?;
        let name = forks::host_name(Kind::File, cached.metadata.identity)?;
        let parent_handle = self.mounts
                                .get(volume)
                                .unwrap()
                                .directories
                                .get(cached.parent)
                                .ok_or(Cause::NotFound)?
                                .handle;
        let lock = self.writer_lock(volume).await?;
        let result =
            async {
                let (expected, current) = self.open_record(parent_handle, &name).await?;
                if current.metadata != cached.metadata
                {
                    return self.cleanup(expected, Err(Cause::Stale.into())).await;
                }
                let mut metadata = current.metadata;
                metadata.modified = when;
                metadata.revision = metadata.revision.checked_add(1).ok_or(Cause::NoMemory)?;
                let node = Node { parent: cached.parent,
                                  metadata,
                                  layout,
                                  key: cached.key };
                if let Err(error) = self.update_root(volume, when, false).await
                {
                    return self.cleanup(expected, Err(error)).await;
                }
                match self.stage_file(volume, node.parent, lock, expected, node, true).await
                {
                    Ok(pending) => Ok(pending),
                    Err(error) => self.cleanup(expected, Err(error)).await
                }
            }.await;
        let pending = match result
        {
            Ok(pending) => pending,
            Err(mut error) =>
            {
                if let Err(cleanup) = self.release_writer(lock).await
                {
                    error.cleanup = Some(cleanup.cause);
                }
                return Err(error);
            }
        };
        self.writes
            .insert(token, pending)
            .unwrap_or_else(|_| unreachable!("write admission was reserved before host I/O"));
        Ok(())
    }

    pub async fn write_chunk(&mut self, volume: u32, token: u32, kind: Fork, bytes: &[u8])
                             -> Result<()>
    {
        self.owner = volume;
        let pending = self.writes
                          .get(token)
                          .filter(|pending| pending.volume == volume)
                          .ok_or(Cause::NotFound)?;
        if pending.header_written || pending.published || pending.aborting
        {
            return Err(Cause::Invalid.into());
        }
        let fork = pending.node.as_ref().unwrap().layout.fork(kind);
        let index = match kind
        {
            Fork::Data => pending.data_chunks,
            Fork::Resource => pending.resource_chunks
        };
        let (physical, expected, _) = fork.chunk(index)?;
        let mut encoded = [0; forks::CHUNK_BYTES + forks::HASH_BYTES as usize];
        let count = forks::encode_chunk(bytes, expected, &mut encoded)?;
        let stage = pending.stage;
        self.call(Op::Write, stage, physical, 0, &encoded[..count]).await?;
        let pending = self.writes.get_mut(token).unwrap();
        match kind
        {
            Fork::Data => pending.data_chunks += 1,
            Fork::Resource => pending.resource_chunks += 1
        };
        Ok(())
    }

    async fn update_parent_after_create(&mut self, token: u32) -> Result<()>
    {
        let pending = self.writes.get(token).unwrap();
        if pending.parent_updated
        {
            return Ok(());
        }
        if pending.replacing || pending.parent == ROOT
        {
            self.writes.get_mut(token).unwrap().parent_updated = true;
            return Ok(());
        }
        let (volume, parent, handle, cnid) =
            (pending.volume, pending.parent, pending.parent_handle, pending.cnid);
        let cached = self.cached_node(volume, parent)?.metadata.clone();
        let when = self.cached_node(volume, cnid)?.metadata.modified;
        let mut desired = cached.clone();
        desired.modified = when;
        desired.revision = desired.revision.checked_add(1).ok_or(Cause::NoMemory)?;
        let (old, actual) = self.open_record(handle, forks::DIRECTORY_RECORD.as_bytes()).await?;
        let result =
            async {
                if actual.metadata == desired
                {
                    // A previous publication may have succeeded before directory
                    // durability failed. Reassert durability without republishing.
                    self.call(Op::SyncDirectory, handle, 0, 0, &[]).await?;
                }
                else if actual.metadata == cached
                {
                    self.publish_record(handle, forks::DIRECTORY_RECORD.as_bytes(), old, &desired)
                        .await?;
                }
                else
                {
                    return Err(Cause::Stale.into());
                }
                Ok(desired)
            }.await;
        let desired = self.cleanup(old, result).await?;
        self.mounts.get_mut(volume).unwrap().nodes.get_mut(parent).unwrap().metadata = desired;
        self.writes.get_mut(token).unwrap().parent_updated = true;
        Ok(())
    }

    async fn cleanup_write(&mut self, token: u32) -> Result<()>
    {
        for which in 0..3
        {
            let pending = self.writes.get(token).ok_or(Cause::NotFound)?;
            let handle = match which
            {
                0 => pending.stage,
                1 => pending.expected,
                _ => pending.lock
            };
            if handle == 0
            {
                continue;
            }
            if which == 2 && pending.locked
            {
                self.call(Op::Unlock, handle, 0, 0, &[]).await?;
                self.writes.get_mut(token).unwrap().locked = false;
            }
            self.close(handle).await?;
            let pending = self.writes.get_mut(token).unwrap();
            match which
            {
                0 => pending.stage = 0,
                1 => pending.expected = 0,
                _ => pending.lock = 0
            }
            if which == 0
            {
                let (volume, parent, parent_handle) =
                    (pending.volume, pending.parent, pending.parent_handle);
                let revision = self.directory_revision(parent_handle).await?;
                self.mounts
                    .get_mut(volume)
                    .unwrap()
                    .directories
                    .get_mut(parent)
                    .unwrap()
                    .revision = Some(revision);
            }
        }
        Ok(())
    }

    /// A completed host commit retires its stage. Retrying after a later cleanup
    /// error never republishes the file or repeats a catalog mutation.
    pub async fn finish_write(&mut self, volume: u32, token: u32) -> Result<u32>
    {
        self.owner = volume;
        let pending = self.writes
                          .get(token)
                          .filter(|pending| pending.volume == volume)
                          .ok_or(Cause::NotFound)?;
        if pending.aborting
        {
            return Err(Cause::Invalid.into());
        }
        if !pending.published
        {
            let layout = pending.node.as_ref().unwrap().layout;
            if pending.data_chunks != layout.data.chunks
               || pending.resource_chunks != layout.resources.chunks
            {
                return Err(forks::Error::Incomplete.into());
            }
            let (stage, header, written, flushed) =
                (pending.stage, pending.header, pending.header_written, pending.flushed);
            if !written
            {
                self.call(Op::Write, stage, 0, 0, &header).await?;
                self.writes.get_mut(token).unwrap().header_written = true;
            }
            if !flushed
            {
                self.call(Op::Flush, stage, 0, 0, &[]).await?;
                self.writes.get_mut(token).unwrap().flushed = true;
            }
            let pending = self.writes.get(token).unwrap();
            let node = pending.node.as_ref().unwrap();
            let name = forks::host_name(Kind::File, node.metadata.identity)?;
            let (parent, expected, replacing) =
                (pending.parent, pending.expected, pending.replacing);
            let mount = self.mounts.get_mut(volume).unwrap();
            if !replacing
            {
                if mount.nodes.entries.len() == MAX_NODES
                {
                    return Err(Cause::NoMemory.into());
                }
                mount.nodes.entries.try_reserve(1).map_err(|_| Cause::NoMemory)?;
                let directory = mount.directories.get_mut(parent).ok_or(Cause::NotFound)?;
                directory.children.try_reserve(1).map_err(|_| Cause::NoMemory)?;
                directory.by_name.try_reserve(1).map_err(|_| Cause::NoMemory)?;
            }
            self.call(Op::Commit, stage, 0, expected, &name).await?;
            let pending = self.writes.get_mut(token).unwrap();
            pending.published = true;
            pending.stage = 0;
            let node = pending.node.take().unwrap();
            let id = node.metadata.catalog_id;
            let mount = self.mounts.get_mut(volume).unwrap();
            if replacing
            {
                *mount.nodes.get_mut(id).ok_or(Cause::Stale)? = node;
            }
            else
            {
                let name_at =
                    mount.directories
                         .get(parent)
                         .unwrap()
                         .by_name
                         .binary_search_by(|id| mount.nodes.get(*id).unwrap().key.cmp(&node.key))
                         .map_or_else(Ok, |_| Err(Cause::Conflict))?;
                mount.nodes.insert(id, node)?;
                let directory = mount.directories.get_mut(parent).unwrap();
                directory.children.push(id);
                directory.by_name.insert(name_at, id);
            }
        }
        self.update_parent_after_create(token).await?;
        let pending = self.writes.get(token).unwrap();
        if !pending.revision_updated
        {
            let (parent, parent_handle) = (pending.parent, pending.parent_handle);
            let revision = self.directory_revision(parent_handle).await?;
            self.mounts.get_mut(volume).unwrap().directories.get_mut(parent).unwrap().revision =
                Some(revision);
            self.writes.get_mut(token).unwrap().revision_updated = true;
        }
        self.cleanup_write(token).await?;
        Ok(self.writes.remove(token).unwrap().cnid)
    }

    /// Return whether cancellation observed a publication, so callers do not
    /// incorrectly report that an already committed mutation was undone.
    pub async fn abort_write(&mut self, volume: u32, token: u32) -> Result<bool>
    {
        self.owner = volume;
        let published = self.writes
                            .get(token)
                            .filter(|pending| pending.volume == volume)
                            .ok_or(Cause::NotFound)?
                            .published;
        self.writes.get_mut(token).unwrap().aborting = true;
        if published
        {
            self.update_parent_after_create(token).await?;
        }
        self.cleanup_write(token).await?;
        self.writes.remove(token);
        Ok(published)
    }
}
