//! Empty-object removal shares the write ledger, ownership and cleanup protocol.
//! Recursive callers remove children in bounded steps before their directory.
use super::*;
impl<R: Rpc> Filesystem<R>
{
    pub(crate) fn removing(&self, volume: u32, token: u32) -> Result<bool>
    {
        Ok(self.writes.get(token).filter(|p| p.volume == volume).ok_or(Cause::NotFound)?.removing)
    }

    pub async fn begin_remove(&mut self, volume: u32, id: u32, token: u32, when: u64)
                              -> Result<()>
    {
        self.owner = volume;
        self.writable(volume)?;
        self.writes.reserve(token)?;
        if id <= ROOT
        {
            return Err(Cause::Protected.into());
        }
        self.find(volume, id).await?;
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
        self.scan(volume, before.parent).await?;
        let is_directory = before.metadata.kind == Kind::Directory;
        if is_directory
        {
            self.scan(volume, id).await?;
            if !self.mounts.get(volume).unwrap().directories.get(id).unwrap().children.is_empty()
            {
                return Err(Cause::NotEmpty.into());
            }
        }
        let parent = self.mounts
                         .get(volume)
                         .unwrap()
                         .directories
                         .get(before.parent)
                         .ok_or(Cause::NotFound)?
                         .handle;
        let lock = self.writer_lock(volume).await?;
        let mut expected = 0;
        let prepared: Result<(u32, Option<wire::ObjectIdentity>)> =
            async {
                let actual = self.directory_revision(parent).await?;
                if self.mounts.get(volume).unwrap().directories.get(before.parent).unwrap().revision
                   != Some(actual)
                {
                    return Err(Cause::Stale.into());
                }
                let source = if is_directory
                {
                    self.mounts
                        .get(volume)
                        .unwrap()
                        .directories
                        .get(id)
                        .ok_or(Cause::NotFound)?
                        .handle
                }
                else
                {
                    let (opened, actual) = if before.plain
                    {
                        self.open_plain(volume, parent, &before.host_name).await?
                    }
                    else
                    {
                        self.open_file_record(volume, parent, &before.host_name).await?
                    };
                    expected = opened;
                    if actual.metadata != before.metadata || actual.source != before.source
                    {
                        return Err(Cause::Stale.into());
                    }
                    opened
                };
                let described = self.call(Op::DescribeObject, source, 0, 0, &[]).await?;
                let object =
                    wire::Description::decode(&described.body).map_err(|_| Cause::Invalid)?
                                                              .identity;
                if before.source.and_then(|s| s.identity) != object
                {
                    return Err(Cause::Stale.into());
                }
                let record = if is_directory && !before.plain
                {
                    forks::DIRECTORY_RECORD.as_bytes()
                }
                else
                {
                    &[]
                };
                let mut payload = buffer(2 + before.host_name.len() + record.len())?;
                payload[..2].copy_from_slice(&(before.host_name.len() as u16).to_le_bytes());
                payload[2..2 + before.host_name.len()].copy_from_slice(&before.host_name);
                payload[2 + before.host_name.len()..].copy_from_slice(record);
                let stage =
                    self.call(Op::BeginRemove, parent, 0, source, &payload).await?.header.object;
                Ok((stage, object))
            }.await;
        let (stage, object) = match prepared
        {
            Ok(value) => value,
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
                                     removing: true,
                                     rename: None,
                                     parent: before.parent,
                                     parent_handle: parent,
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
                                     node: Some(before),
                                     header: [0; forks::HEADER_BYTES],
                                     data_chunks: 0,
                                     resource_chunks: 0,
                                     header_written: true,
                                     flushed: true,
                                     published: false,
                                     identity_bound: true,
                                     previous_object: object,
                                     aborting: false,
                                     replacing: false,
                                     revision_updated: false,
                                     parent_updated: false,
                                     volume_updated: false,
                                     when,
                                     locked: true,
                                     metadata_name: None,
                                     source_guard: 0,
                                     root_metadata: None,
                                     revision_directory: Some(self.cached_node(volume, id)?
                                                                  .parent),
                                     notification: None,
                                     notified: true })
            .unwrap_or_else(|_| unreachable!("write admission reserved before host I/O"));
        Ok(())
    }

    pub(super) async fn finish_remove(&mut self, volume: u32, token: u32) -> Result<u32>
    {
        let p = self.writes
                    .get(token)
                    .filter(|p| p.volume == volume && p.removing)
                    .ok_or(Cause::NotFound)?;
        if p.aborting
        {
            return Err(Cause::Invalid.into());
        }
        let (stage, id, parent, object) = (p.stage, p.cnid, p.parent, p.previous_object);
        if !p.published
        {
            let state = self.call(Op::RemoveState, stage, 0, 0, &[]).await?.header.value;
            if state <= wire::REMOVE_QUARANTINED
            {
                self.call(Op::QuarantineRemove, stage, 0, 0, &[]).await?;
            }
            else if state != wire::REMOVE_COMMITTING && state != wire::REMOVE_REMOVED
            {
                return Err(Cause::Invalid.into());
            }
            self.call(Op::CommitRemove, stage, 0, 0, &[]).await?;
            self.writes.get_mut(token).unwrap().published = true;
        }
        if let Some(object) = object
        {
            self.retire_file_annotation(volume, object, id).await?;
        }
        // Close a removed directory before releasing its cached capability.
        if let Some(directory) = self.mounts.get(volume).unwrap().directories.get(id)
        {
            let handle = directory.handle;
            self.close(handle).await?;
            self.mounts.get_mut(volume).unwrap().directories.remove(id);
        }
        let mount = self.mounts.get_mut(volume).unwrap();
        mount.nodes.remove(id);
        // Physical bindings are not live handles; drop only this deleted ID.
        mount.bindings.retain(|b| b.catalog_id() != id);
        let directory = mount.directories.get_mut(parent).ok_or(Cause::NotFound)?;
        directory.children.retain(|child| *child != id);
        directory.by_name.retain(|child| *child != id);
        self.update_volume_after_publication(token).await?;
        self.update_parent_after_create(token).await?;
        self.cleanup_write(token).await?;
        self.writes.remove(token);
        Ok(id)
    }

    pub(super) async fn abort_remove(&mut self, volume: u32, token: u32) -> Result<bool>
    {
        let p = self.writes
                    .get(token)
                    .filter(|p| p.volume == volume && p.removing)
                    .ok_or(Cause::NotFound)?;
        let (stage, published) = (p.stage, p.published);
        if p.aborting
        {
            self.cleanup_write(token).await?;
            self.writes.remove(token);
            return Ok(false);
        }
        if published
        {
            self.finish_remove(volume, token).await?;
            return Ok(true);
        }
        let state = self.call(Op::RemoveState, stage, 0, 0, &[]).await?.header.value;
        if state == wire::REMOVE_COMMITTING || state == wire::REMOVE_REMOVED
        {
            self.finish_remove(volume, token).await?;
            return Ok(true);
        }
        if state > wire::REMOVE_ABORTED
        {
            return Err(Cause::Invalid.into());
        }
        let removed = self.call(Op::AbortRemove, stage, 0, 0, &[]).await?.header.value;
        if removed != 0
        {
            return Err(Cause::Invalid.into());
        }
        // Quarantine/restore changes ctime. Rebind our cached revision before
        // releasing the writer, so the next click/open/retry needs no refresh.
        let pending = self.writes.get(token).unwrap();
        let (id, parent, is_directory) = (pending.cnid,
                                          pending.parent_handle,
                                          pending.node.as_ref().unwrap().metadata.kind
                                          == Kind::Directory);
        let prior = pending.node.as_ref().unwrap();
        let mut name = buffer(prior.host_name.len())?;
        name.copy_from_slice(&prior.host_name);
        let parent_id = prior.parent;
        let parent_modified = self.cached_node(volume, parent_id)?.metadata.modified;
        let restored = if is_directory
        {
            let directory =
                self.mounts.get(volume).unwrap().directories.get(id).ok_or(Cause::NotFound)?.handle;
            self.directory_node(volume, parent, &name, directory).await?
        }
        else
        {
            self.candidate(volume, parent, &name).await?
        };
        let prior = self.writes.get(token).unwrap().node.as_ref().unwrap();
        if restored.metadata.catalog_id != id
           || restored.source.and_then(|s| s.identity) != prior.source.and_then(|s| s.identity)
        {
            return Err(Cause::Stale.into());
        }
        self.mounts.get_mut(volume).unwrap().nodes.get_mut(id).ok_or(Cause::NotFound)?.source =
            restored.source;
        self.stamp_directory(volume, parent_id, parent_modified).await?;
        self.writes.get_mut(token).unwrap().aborting = true;
        self.cleanup_write(token).await?;
        self.writes.remove(token);
        Ok(false)
    }
}
