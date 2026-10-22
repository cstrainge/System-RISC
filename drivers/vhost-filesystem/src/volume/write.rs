//! Bounded two-fork staging. Catalog IDs are reserved durably before publication;
//! a root writer lease serializes case-folded name checks and final publication.
use super::*;

pub struct Create<'a>
{
    pub volume:          u32,
    pub parent:          u32,
    pub token:           u32,
    pub properties:      mac_filesystem_interface::FileProperties<'a>,
    pub data_length:     u64,
    pub resource_length: u64,
    pub when:            u64
}
pub struct Replace
{
    pub volume:             u32,
    pub node:               u32,
    pub token:              u32,
    pub data_length:        u64,
    pub resource_length:    u64,
    pub when:               u64,
    pub preserve_resources: bool
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
    volume_updated:    bool,
    when:              u64,
    locked:            bool
}
impl Pending
{
    pub(super) fn node_id(&self) -> u32
    {
        self.cnid
    }

    pub(super) fn references(&self, id: u32) -> bool
    {
        self.stage == id || self.expected == id || self.lock == id
    }
}
impl<R: Rpc> Filesystem<R>
{
    pub(crate) fn pending_directory(&self, volume: u32, node: u32) -> bool
    {
        self.writes.entries.iter().any(|(_, p)| {
                                      p.volume == volume
                                      && p.node.as_ref().is_some_and(|n| {
                                                            n.metadata.catalog_id == node
                                                            && n.metadata.kind == Kind::Directory
                                                        })
                                  })
    }

    pub(crate) fn pending_node(&self, volume: u32, token: u32) -> Result<&Node>
    {
        let pending =
            self.writes.get(token).filter(|p| p.volume == volume).ok_or(Cause::NotFound)?;
        if pending.aborting
        {
            return Err(Cause::Busy.into());
        }
        pending.node.as_ref().ok_or(Cause::Busy.into())
    }

    fn writable(&self, volume: u32) -> Result<()>
    {
        let mount = self.mounts.get(volume).ok_or(Cause::NotFound)?;
        if mount.info.read_only
        {
            return Err(Cause::ReadOnly.into());
        }
        if mount.ejecting
        {
            return Err(Cause::Busy.into());
        }
        if mount.invalidated
        {
            return Err(Cause::Stale.into());
        }
        Ok(())
    }

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

    async fn update_root(&mut self, volume: u32, when: Option<u64>, reserve: bool) -> Result<u32>
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
                let already = !reserve && when == Some(metadata.modified);
                if reserve
                {
                    metadata.next_catalog_id = id.checked_add(1).ok_or(Cause::NoMemory)?;
                }
                if !already
                {
                    metadata.revision = metadata.revision.checked_add(1).ok_or(Cause::NoMemory)?;
                    if let Some(when) = when
                    {
                        metadata.modified = when;
                    }
                }
                let info = metadata.try_clone()?;
                if already
                {
                    self.call(Op::SyncDirectory, root, 0, 0, &[]).await?;
                }
                else
                {
                    self.publish_record(root, forks::ROOT_RECORD.as_bytes(), old, &metadata)
                        .await?;
                }
                let revision = self.directory_revision(root).await?;
                let mount = self.mounts.get_mut(volume).unwrap();
                mount.info.metadata = info;
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
        let when = node.metadata.modified;
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
                     volume_updated: false,
                     when,
                     locked: true })
    }

    pub async fn begin_create(&mut self, request: Create<'_>) -> Result<u32>
    {
        self.begin_create_kind(request, Kind::File).await
    }

    pub async fn begin_directory(&mut self, request: Create<'_>) -> Result<u32>
    {
        if request.data_length != 0 || request.resource_length != 0
        {
            return Err(Cause::Invalid.into());
        }
        self.begin_create_kind(request, Kind::Directory).await
    }

    async fn begin_create_kind(&mut self, request: Create<'_>, kind: Kind) -> Result<u32>
    {
        let Create { volume,
                     parent,
                     token,
                     properties,
                     data_length,
                     resource_length,
                     when } = request;
        self.writable(volume)?;
        self.writes.reserve(token)?;
        properties.validate().map_err(|_| Cause::Invalid)?;
        let normalized = mac_hfs_b::names::normalize(properties.name).map_err(|_| Cause::Invalid)?;
        forks::host_component(&normalized).map_err(|_| Cause::InvalidName)?;
        let mut physical = buffer(normalized.len())?;
        physical.copy_from_slice(normalized.as_bytes());
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
            Metadata::new(kind, 16, mount.info.metadata.volume, &normalized, properties.created)?;
        metadata.modified = properties.modified;
        metadata.backup = properties.backup;
        metadata.attributes = properties.attributes;
        metadata.finder_info = properties.finder_info;
        metadata.comment
                .try_reserve_exact(properties.comment.len())
                .map_err(|_| Cause::NoMemory)?;
        metadata.comment.push_str(properties.comment);
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
                // Reserving an identity is durable bookkeeping, not a visible
                // catalog publication. An aborted create must not change dates.
                let id = self.update_root(volume, None, true).await?;
                metadata.catalog_id = id;
                metadata.identity = forks::catalog_identity(metadata.volume, id)?;
                let node = Node { parent,
                                  host_name: physical,
                                  plain: false,
                                  source: None,
                                  metadata,
                                  layout,
                                  key };
                self.stage_file(volume, parent, lock, 0, node, false).await
            }.await;
        let mut pending = match result
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
        pending.when = when;
        let id = pending.node.as_ref().unwrap().metadata.catalog_id;
        self.writes
            .insert(token, pending)
            .unwrap_or_else(|_| unreachable!("write admission was reserved before host I/O"));
        Ok(id)
    }

    pub async fn begin_replace(&mut self, request: Replace) -> Result<()>
    {
        if request.preserve_resources && request.resource_length != 0
        {
            return Err(Cause::Invalid.into());
        }
        self.writable(request.volume)?;
        let Replace { volume,
                      node,
                      token,
                      data_length,
                      resource_length,
                      when,
                      preserve_resources } = request;
        self.writes.reserve(token)?;
        if self.is_open(volume, node)
        {
            return Err(Cause::Busy.into());
        }
        self.find(volume, node).await?;
        let cached = self.cached_node(volume, node)?.try_clone()?;
        let original_resources = cached.layout.resources;
        let resource_length =
            if preserve_resources { original_resources.length } else { resource_length };
        if cached.metadata.kind != Kind::File
        {
            return Err(Cause::Invalid.into());
        }
        if cached.metadata.attributes & 1 != 0
        {
            return Err(Cause::ReadOnly.into());
        }
        let layout = Layout::new(&cached.metadata, data_length, resource_length)?;
        let mut name = buffer(cached.host_name.len())?;
        name.copy_from_slice(&cached.host_name);
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
                if current.metadata != cached.metadata || current.layout != cached.layout
                {
                    return self.cleanup(expected, Err(Cause::Stale.into())).await;
                }
                let mut metadata = current.metadata;
                metadata.modified = when;
                metadata.revision = metadata.revision.checked_add(1).ok_or(Cause::NoMemory)?;
                let node = Node { parent: cached.parent,
                                  host_name: cached.host_name,
                                  plain: false,
                                  source: None,
                                  metadata,
                                  layout,
                                  key: cached.key };
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
        if preserve_resources
        {
            self.copy_resource(volume, token, original_resources).await?;
        }
        Ok(())
    }

    async fn copy_resource(&mut self, volume: u32, token: u32, source: forks::ForkLayout)
                           -> Result<()>
    {
        let pending = self.writes
                          .get(token)
                          .filter(|pending| pending.volume == volume)
                          .ok_or(Cause::NotFound)?;
        let (expected, stage, target) =
            (pending.expected,
             pending.stage,
             pending.node.as_ref().ok_or(Cause::Invalid)?.layout.resources);
        for index in 0..source.chunks
        {
            let (offset, count, _) = source.chunk(index)?;
            let reply = self.call(Op::Read,
                                  expected,
                                  offset,
                                  (count + forks::HASH_BYTES as usize) as u32,
                                  &[])
                            .await?;
            forks::decode_chunk(&reply.body, count)?;
            let (destination, expected_count, _) = target.chunk(index)?;
            if expected_count != count
            {
                return Err(Cause::Invalid.into());
            }
            // Chunk digests cover bytes, not their file offsets. Verify once
            // and copy the complete checked chunk; do not hash it a second time.
            self.call(Op::Write, stage, destination, 0, &reply.body).await?;
            self.writes.get_mut(token).ok_or(Cause::NotFound)?.resource_chunks = index + 1;
            yield_once().await;
        }
        Ok(())
    }

    pub async fn write_chunk(&mut self, volume: u32, token: u32, kind: Fork, bytes: &[u8])
                             -> Result<()>
    {
        const {
            assert!(forks::CHUNK_BYTES == mac_filesystem_interface::WRITE_ALIGNMENT);
        }
        let pending = self.writes
                          .get(token)
                          .filter(|pending| pending.volume == volume)
                          .ok_or(Cause::NotFound)?;
        if pending.header_written || pending.published || pending.aborting
        {
            return Err(Cause::Invalid.into());
        }
        let fork = pending.node.as_ref().ok_or(Cause::Invalid)?.layout.fork(kind);
        let chunks = if kind == Fork::Data { pending.data_chunks } else { pending.resource_chunks };
        let remaining =
            fork.length
                .checked_sub(chunks.checked_mul(forks::CHUNK_BYTES as u64).ok_or(Cause::Invalid)?)
                .ok_or(Cause::Invalid)?;
        if bytes.is_empty()
           || bytes.len() > mac_filesystem_interface::MAX_TRANSFER
           || bytes.len() as u64 > remaining
           || bytes.len() as u64 != remaining && !bytes.len().is_multiple_of(forks::CHUNK_BYTES)
        {
            return Err(Cause::Invalid.into());
        }
        for chunk in bytes.chunks(forks::CHUNK_BYTES)
        {
            self.write_one(volume, token, kind, chunk).await?;
            yield_once().await;
        }
        Ok(())
    }

    async fn write_one(&mut self, volume: u32, token: u32, kind: Fork, bytes: &[u8]) -> Result<()>
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

    async fn update_volume_after_publication(&mut self, token: u32) -> Result<()>
    {
        let pending = self.writes.get(token).ok_or(Cause::NotFound)?;
        if pending.volume_updated
        {
            return Ok(());
        }
        let (volume, when) = (pending.volume, pending.when);
        if self.info(volume)?.metadata.modified != when
        {
            self.update_root(volume, Some(when), false).await?;
        }
        self.writes.get_mut(token).unwrap().volume_updated = true;
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
        let (volume, parent, handle, when) =
            (pending.volume, pending.parent, pending.parent_handle, pending.when);
        let parent_node = self.cached_node(volume, parent)?;
        if parent_node.plain
        {
            let mut physical = buffer(parent_node.host_name.len())?;
            physical.copy_from_slice(&parent_node.host_name);
            let ancestor = self.mounts
                               .get(volume)
                               .unwrap()
                               .directories
                               .get(parent_node.parent)
                               .ok_or(Cause::NotFound)?
                               .handle;
            let observed = self.plain_node(volume, ancestor, &physical).await?;
            if observed.metadata.catalog_id != parent
            {
                return Err(Cause::Stale.into());
            }
            let cached = self.mounts.get_mut(volume).unwrap().nodes.get_mut(parent).unwrap();
            cached.metadata = observed.metadata;
            cached.source = observed.source;
            self.writes.get_mut(token).unwrap().parent_updated = true;
            return Ok(());
        }
        let cached = self.cached_node(volume, parent)?.metadata.try_clone()?;
        let mut desired = cached.try_clone()?;
        desired.modified = when;
        desired.revision = desired.revision.checked_add(1).ok_or(Cause::NoMemory)?;
        let (old, mut actual) =
            self.open_record(handle, forks::DIRECTORY_RECORD.as_bytes()).await?;
        actual.metadata.name.clear();
        actual.metadata.name.try_reserve_exact(cached.name.len()).map_err(|_| Cause::NoMemory)?;
        actual.metadata.name.push_str(&cached.name);
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
            if self.is_open(volume, pending.cnid)
            {
                return Err(Cause::Busy.into());
            }
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
            let is_directory = node.metadata.kind == Kind::Directory;
            let mut name = buffer(node.host_name.len())?;
            name.copy_from_slice(&node.host_name);
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
                if is_directory
                {
                    mount.directories.reserve(node.metadata.catalog_id)?;
                }
                let directory = mount.directories.get_mut(parent).ok_or(Cause::NotFound)?;
                directory.children.try_reserve(1).map_err(|_| Cause::NoMemory)?;
                directory.by_name.try_reserve(1).map_err(|_| Cause::NoMemory)?;
            }
            let opened = if is_directory
            {
                let mut payload = buffer(name.len() + 2 + forks::DIRECTORY_RECORD.len())?;
                payload[..2].copy_from_slice(&(name.len() as u16).to_le_bytes());
                payload[2..2 + name.len()].copy_from_slice(&name);
                payload[name.len() + 2..].copy_from_slice(forks::DIRECTORY_RECORD.as_bytes());
                self.call(Op::PublishDirectory, stage, 0, 0, &payload).await?.header.object
            }
            else
            {
                self.call(Op::Commit, stage, 0, expected, &name).await?;
                0
            };
            let pending = self.writes.get_mut(token).unwrap();
            pending.published = true;
            pending.stage = 0;
            let node = pending.node.take().unwrap();
            let id = node.metadata.catalog_id;
            let mount = self.mounts.get_mut(volume).unwrap();
            if is_directory
            {
                let mut directory = Directory::new(opened);
                directory.complete = true;
                mount.directories.insert(id, directory)?;
            }
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
        self.update_volume_after_publication(token).await?;
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
            self.update_volume_after_publication(token).await?;
            self.update_parent_after_create(token).await?;
        }
        self.cleanup_write(token).await?;
        self.writes.remove(token);
        Ok(published)
    }
}
