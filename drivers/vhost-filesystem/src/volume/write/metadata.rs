//! Metadata shares the writer ledger and acknowledged finish/abort protocol.
//! Small annotations never copy the object's fork payloads.
use forks::annotation::{self, Annotation};
use mac_filesystem_interface::{FileProperties, MetadataPatch};

use super::*;

fn changed(metadata: &mut Metadata, patch: MetadataPatch<'_>) -> Result<bool>
{
    let raw = FileProperties { name:        &metadata.name,
                               comment:     &metadata.comment,
                               created:     metadata.created,
                               modified:    metadata.modified,
                               backup:      metadata.backup,
                               attributes:  metadata.attributes,
                               finder_info: metadata.finder_info };
    let desired = patch.apply(raw).map_err(|_| Cause::Invalid)?;
    if desired == raw
    {
        return Ok(false);
    }
    let mut comment = String::new();
    comment.try_reserve_exact(desired.comment.len()).map_err(|_| Cause::NoMemory)?;
    comment.push_str(desired.comment);
    let revision = metadata.revision.checked_add(1).ok_or(Cause::NoMemory)?;
    metadata.created = desired.created;
    metadata.modified = desired.modified;
    metadata.backup = desired.backup;
    metadata.attributes = desired.attributes;
    metadata.finder_info = desired.finder_info;
    metadata.comment = comment;
    metadata.revision = revision;
    Ok(true)
}
impl<R: Rpc> Filesystem<R>
{
    /// Stage a field-selective edit. `false` means an exact no-change, with no
    /// host I/O or transaction to finish. IDs come from the resident writer table.
    pub async fn begin_metadata(&mut self, volume: u32, node: u32, token: u32,
                                patch: MetadataPatch<'_>, when: u64)
                                -> Result<bool>
    {
        self.owner = volume;
        self.writable(volume)?;
        self.writes.reserve(token)?;
        patch.validate().map_err(|_| Cause::Invalid)?;
        self.find(volume, node).await?;
        let cached = self.cached_node(volume, node)?.try_clone()?;
        let view_state_only = patch.attributes_mask == 0
                              && patch.comment.is_none()
                              && patch.created.is_none()
                              && patch.modified.is_none()
                              && patch.backup.is_none()
                              && patch.finder_mask
                                      .iter()
                                      .enumerate()
                                      .all(|(i, mask)| *mask == 0 || matches!(i,0..=7|14..=19));
        let notification = if view_state_only
        {
            None
        }
        else
        {
            let mount = self.mounts.get(volume).ok_or(Cause::NotFound)?;
            Some(if cached.parent < ROOT
                 {
                     mount.root
                 }
                 else
                 {
                     mount.directories.get(cached.parent).ok_or(Cause::NotFound)?.handle
                 })
        };
        let mut desired = cached.try_clone()?;
        if !changed(&mut desired.metadata, patch)?
        {
            return Ok(false);
        }
        // Icon placement changes Finder metadata even on a locked file; data,
        // comments, stationery and dates retain the native protection rules.
        let placement_only = patch.attributes_mask == 0
                             && patch.comment.is_none()
                             && patch.created.is_none()
                             && patch.modified.is_none()
                             && patch.backup.is_none()
                             && patch.finder_mask.iter().enumerate().all(|(i, mask)| {
                                                                        mask
                                                                        & !match i
                                                                        {
                                                                            8 => 1,
                                                                            10..=13 => 255,
                                                                            _ => 0
                                                                        }
                                                                        == 0
                                                                    });
        let unlocking = if cached.metadata.attributes & 1 != 0
        {
            // Full-property copies and the selective lock API share the same
            // rule: unlocking may change only that flag, never sneak in edits.
            let mut unlocked = cached.metadata.try_clone()?;
            unlocked.attributes &= !1;
            unlocked.revision = desired.metadata.revision;
            unlocked == desired.metadata
        }
        else
        {
            false
        };
        if cached.metadata.attributes & 1 != 0 && !placement_only && !unlocking
        {
            return Err(Cause::Protected.into());
        }
        let direct = cached.metadata.kind == Kind::Root;
        if direct
           && self.mounts
                  .get(volume)
                  .unwrap()
                  .directories
                  .get(node)
                  .is_some_and(|d| d.enumeration.is_some())
        {
            return Err(Cause::Busy.into());
        }
        let lock = self.writer_lock(volume).await?;
        // Retain acquired objects here until Pending takes their ownership. On
        // any rejected preparation every capability is explicitly closed.
        let mut acquired = [0; 3]; // expected publication, source snapshot, stage
        let prepared: Result<Pending> =
            async {
                let (parent_handle, name, bytes) = if direct
                {
                    let parent = self.mounts
                                     .get(volume)
                                     .unwrap()
                                     .directories
                                     .get(node)
                                     .ok_or(Cause::NotFound)?
                                     .handle;
                    let name =
                        if node == ROOT { forks::ROOT_RECORD } else { forks::DIRECTORY_RECORD };
                    let (expected, mut actual) = self.open_record(parent, name.as_bytes()).await?;
                    acquired[0] = expected;
                    // Physical host renames are authoritative for the visible name.
                    let mut actual_name = String::new();
                    actual_name.try_reserve_exact(desired.metadata.name.len())
                               .map_err(|_| Cause::NoMemory)?;
                    actual_name.push_str(&desired.metadata.name);
                    actual.metadata.name = actual_name;
                    if actual.metadata != cached.metadata
                    {
                        return Err(Cause::Stale.into());
                    }
                    let (encoded, _) = forks::encode_metadata(&desired.metadata, 0, 0)?;
                    let mut bytes = buffer(encoded.len())?;
                    bytes.copy_from_slice(&encoded);
                    let mut physical = buffer(name.len())?;
                    physical.copy_from_slice(name.as_bytes());
                    (parent, physical, bytes)
                }
                else
                {
                    let parent = self.mounts
                                     .get(volume)
                                     .unwrap()
                                     .directories
                                     .get(cached.parent)
                                     .ok_or(Cause::NotFound)?
                                     .handle;
                    let (source, actual) = if cached.metadata.kind == Kind::File
                    {
                        if cached.plain
                        {
                            self.open_plain(volume, parent, &cached.host_name).await?
                        }
                        else
                        {
                            self.open_file_record(volume, parent, &cached.host_name).await?
                        }
                    }
                    else
                    {
                        let directory = self.mounts
                                            .get(volume)
                                            .unwrap()
                                            .directories
                                            .get(node)
                                            .ok_or(Cause::NotFound)?
                                            .handle;
                        (0,
                         self.directory_node(volume, parent, &cached.host_name, directory).await?)
                    };
                    acquired[1] = source;
                    if actual.metadata != cached.metadata
                    {
                        return Err(Cause::Stale.into());
                    }
                    let object =
                        actual.source.and_then(|s| s.identity).ok_or(Cause::Unsupported)?;
                    let previous = self.annotation(volume, object).await?;
                    let old = previous.as_ref().map(Annotation::decode).transpose()?;
                    let merged = old.map_or(Ok(patch), |record| record.patch.merged(patch))
                                    .map_err(|_| Cause::Invalid)?;
                    let revision = old.map_or(Ok(1), |record| {
                                          record.revision.checked_add(1).ok_or(Cause::NoMemory)
                                      })?;
                    let record = Annotation { volume: desired.metadata.volume,
                                              object: *object.as_bytes(),
                                              catalog_id: node,
                                              revision,
                                              patch: merged };
                    let mut encoded = [0; annotation::BYTES];
                    record.encode(&mut encoded)?;
                    let directory =
                        self.annotation_directory(volume).await?.ok_or(Cause::Invalid)?;
                    let name = annotation::key(object.as_bytes());
                    if old.is_some()
                    {
                        acquired[0] =
                            self.call(Op::OpenRead, directory, 0, 0, &name).await?.header.object;
                        // Bind replacement to the same checked annotation revision.
                        let found =
                            self.call(Op::Read, acquired[0], 0, annotation::BYTES as u32, &[])
                                .await?;
                        if Some(found.body.as_slice()) != previous.as_ref().map(|b| b.as_slice())
                        {
                            return Err(Cause::Stale.into());
                        }
                    }
                    let mut physical = buffer(name.len())?;
                    physical.copy_from_slice(&name);
                    let mut bytes = buffer(annotation::BYTES)?;
                    bytes.copy_from_slice(&encoded);
                    (directory, physical, bytes)
                };
                // All cache/publication storage exists before the atomic host commit.
                let root_metadata =
                    if node == ROOT { Some(desired.metadata.try_clone()?) } else { None };
                let stage =
                    self.call(Op::BeginWrite, parent_handle, 0, 0, &[]).await?.header.object;
                acquired[2] = stage;
                self.call(Op::SetLength, stage, bytes.len() as u64, 0, &[]).await?;
                self.call(Op::Write, stage, 0, 0, &bytes).await?;
                Ok(Pending { volume,
                             rename: None,
                             removing: false,
                             parent: cached.parent,
                             parent_handle: if direct
                             {
                                 parent_handle
                             }
                             else
                             {
                                 notification.unwrap_or(parent_handle)
                             },
                             stage,
                             expected: acquired[0],
                             lock: if self.batch_lock.is_some_and(|(_, held)| held == lock)
                             {
                                 0
                             }
                             else
                             {
                                 lock
                             },
                             cnid: node,
                             node: Some(desired),
                             header: [0; forks::HEADER_BYTES],
                             data_chunks: 0,
                             resource_chunks: 0,
                             header_written: true,
                             flushed: false,
                             published: false,
                             identity_bound: true,
                             previous_object: None,
                             aborting: false,
                             replacing: true,
                             revision_updated: !direct && notification.is_none(),
                             parent_updated: true,
                             // A root edit already publishes the volume record.
                             // Keep its explicit dates (including rollback), and
                             // do not turn a view-state save into a date change.
                             volume_updated: view_state_only || node == ROOT,
                             when,
                             locked: true,
                             metadata_name: Some(name),
                             source_guard: acquired[1],
                             root_metadata,
                             revision_directory: if direct
                             {
                                 Some(node)
                             }
                             else
                             {
                                 notification.map(|_| cached.parent.max(ROOT))
                             },
                             notification,
                             notified: notification.is_none() })
            }.await;
        match prepared
        {
            Ok(pending) =>
            {
                self.writes
                    .insert(token, pending)
                    .unwrap_or_else(|_| unreachable!("reserved writer admission"));
                Ok(true)
            }
            Err(mut error) =>
            {
                for handle in acquired.into_iter().rev().filter(|h| *h != 0)
                {
                    if let Err(cleanup) = self.close(handle).await
                    {
                        error.cleanup = Some(cleanup.cause);
                    }
                }
                if let Err(cleanup) = self.release_writer(lock).await
                {
                    error.cleanup = Some(cleanup.cause);
                }
                Err(error)
            }
        }
    }
}
