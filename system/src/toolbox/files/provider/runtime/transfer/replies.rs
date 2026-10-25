//! Validate every receipt before advancing its publication/undo ledger.
use super::*;
fn entry(reply: Reply) -> Result<CatalogInfo, Status>
{
    if let Reply::Entry(e) = reply { Ok(e) } else { Err(BAD_ABI) }
}
fn mounted(reply: Reply) -> Result<VolumeInfo, Status>
{
    if let Reply::Mounted(e) = reply { Ok(e) } else { Err(BAD_ABI) }
}
impl State
{
    pub(super) fn accept(&mut self, tag: Tag, reply: Result<Reply, Status>) -> Result<(), Status>
    {
        if tag == Tag::Collision
        {
            return match reply
            {
                Err(NOT_FOUND) => self.collision_complete(),
                Ok(reply) =>
                {
                    let found = entry(reply)?;
                    let root = &self.roots[self.current_root];
                    if self.mode != Mode::Copy
                       && found.volume == root.input.volume
                       && found.id == root.input.cnid
                    {
                        self.collision_complete()
                    }
                    else if self.mode == Mode::Copy
                              && root.input.volume == root.target.volume
                              && root.input.expected_parent == root.target.id
                    {
                        self.copy_name()
                    }
                    else
                    {
                        Err(ALREADY_EXISTS)
                    }
                }
                Err(error) => Err(error)
            };
        }
        if tag == Tag::TrashDestination && matches!(&reply, Err(NOT_FOUND))
        {
            if self.mode.is_trash_query()
            {
                self.committed = true;
                self.finish();
                return Ok(());
            }
            if self.put_away
            {
                return Err(NOT_FOUND);
            }
            let root = &mut self.roots[self.current_root];
            root.target = Reference { volume: root.input.volume,
                                      id:     0 };
            self.stage = Stage::SourceCatalog;
            return Ok(());
        }
        if matches!(&reply, Err(NOT_FOUND))
        {
            match tag
            {
                Tag::AbortWriter =>
                {
                    self.writer = 0;
                    self.deferred_writer = false;
                    self.stage =
                        if self.undo_complete { Stage::RetryHandles } else { Stage::DrainReader };
                    return Ok(());
                }
                Tag::CloseReader =>
                {
                    self.reader = 0;
                    self.stage = if self.undo_complete || self.deferred_writer
                    {
                        Stage::RetryHandles
                    }
                    else
                    {
                        Stage::DrainGroup
                    };
                    return Ok(());
                }
                _ => ()
            }
        }
        if let Err(error) = &reply
           && self.stage != Stage::RetryHandles
        {
            if tag == Tag::AbortWriter
            {
                // A dead destination must not retain a healthy source reader.
                // Keep the unknown writer owned while closing independent I/O.
                self.stats.cleanup_error = *error;
                self.deferred_writer = true;
                self.stage = Stage::DrainReader;
                return Ok(());
            }
            if tag == Tag::CloseReader
            {
                // A read capability has no publication to undo. Retain it for
                // acknowledged retry while independently undoing destination
                // mutations; don't freeze a healthy disk behind a dead source.
                self.stats.cleanup_error = *error;
                self.stage =
                    if self.deferred_writer { Stage::RetryHandles } else { Stage::DrainGroup };
                return Ok(());
            }
        }
        if matches!(tag, Tag::BeginGroup | Tag::StepGroup | Tag::CancelGroup)
        {
            return match reply
            {
                Ok(reply) => self.group_ready(tag, reply),
                Err(error) =>
                {
                    if let Some(group) = &self.group
                    {
                        if group.handle != 0 && files(|m| {
                                                    Ok(m.groups.completing(group.handle).is_none())
                                                }).unwrap_or(false)
                        {
                            self.group = None;
                        }
                    }
                    Err(error)
                }
            };
        }
        let reply = reply?;
        match tag
        {
            Tag::DestinationVolume =>
            {
                let volume = mounted(reply)?;
                if volume.id != self.target.volume
                {
                    return Err(BAD_ABI);
                }
                if volume.read_only != 0
                {
                    return Err(READ_ONLY);
                }
                self.stage = Stage::DestinationCatalog;
            }
            Tag::DestinationCatalog =>
            {
                let value = entry(reply)?;
                if value.volume != self.target.volume
                   || value.id != self.target.id
                   || !value.is_directory()
                {
                    return Err(BAD_ARGUMENT);
                }
                if value.flags & FILE_LOCKED != 0
                {
                    return Err(ACCESS_DENIED);
                }
                self.ancestry = value.id;
                self.stage = Stage::DestinationAncestry;
            }
            Tag::DestinationAncestry =>
            {
                let value = entry(reply)?;
                if value.volume != self.target.volume
                   || value.id != self.ancestry
                   || !value.is_directory()
                {
                    return Err(BAD_ABI);
                }
                if self.ancestors.contains(&value.id) || self.ancestors.len() == MAX_DEPTH
                {
                    return Err(CORRUPT_DATA);
                }
                self.ancestors.try_reserve(1).map_err(|_| NO_MEMORY)?;
                self.ancestors.push(value.id);
                if value.id == ROOT_ID
                {
                    self.stage = Stage::SourceVolume;
                }
                else
                {
                    self.ancestry = value.parent;
                }
            }
            Tag::SourceVolume =>
            {
                let volume = mounted(reply)?;
                if volume.id != self.roots[self.current_root].input.volume
                {
                    return Err(BAD_ABI);
                }
                if self.mode == Mode::CountTrash && volume.read_only != 0
                {
                    self.committed = true;
                    self.finish();
                    return Ok(());
                }
                if !matches!(self.mode, Mode::Copy | Mode::CountTrash) && volume.read_only != 0
                {
                    return Err(READ_ONLY);
                }
                self.stage =
                    if self.mode == Mode::Trash || self.put_away || self.mode.is_trash_query()
                    {
                        Stage::TrashDestination
                    }
                    else
                    {
                        Stage::SourceCatalog
                    };
            }
            Tag::TrashDestination =>
            {
                let value = entry(reply)?;
                if self.mode.is_trash_query()
                {
                    if value.volume != self.trash_volume || !value.is_directory()
                    {
                        return Err(BAD_ABI);
                    }
                    self.roots.clear();
                    self.trash_directory = value.id;
                    self.stage = Stage::ScanTrash;
                    return Ok(());
                }
                if self.put_away
                {
                    if value.id != self.roots[self.current_root].input.expected_parent
                    {
                        return Err(NOT_FOUND);
                    }
                    self.stage = Stage::SourceCatalog;
                    return Ok(());
                }
                let root = &mut self.roots[self.current_root];
                if value.volume != root.input.volume || !value.is_directory()
                {
                    return Err(BAD_ABI);
                }
                root.target = Reference { volume: value.volume,
                                          id:     value.id };
                self.stage = Stage::SourceCatalog;
            }
            Tag::ScanTrash =>
            {
                let Reply::Scan(call) = reply
                else
                {
                    return Err(BAD_ABI);
                };
                if call.volume != self.trash_volume || call.directory != self.trash_directory
                {
                    return Err(BAD_ABI);
                }
                self.trash_cursor = call.cursor;
                match call.step
                {
                    SCAN_PENDING => (),
                    SCAN_ENTRY =>
                    {
                        self.account_allocation(call.data_allocated, call.resource_allocated)?;
                        self.roots.try_reserve(1).map_err(|_| NO_MEMORY)?;
                        let source = call.entry;
                        self.current_root = self.roots.len();
                        self.roots.push(Root { input:
                                                   FileTransferSource { volume: self.trash_volume,
                                                                        cnid: source.id,
                                                                        expected_parent:
                                                                            self.trash_directory,
                                                                        ..Default::default() },
                                               node:         None,
                                               source_stage: None,
                                               target:       Reference { volume: 0,
                                                                         id:     0 },
                                               name:         String::new(),
                                               name_attempt: 0,
                                               quarantined:  false,
                                               published:    false,
                                               changed:      false });
                        self.add_node(source, None)?;
                    }
                    SCAN_DONE =>
                    {
                        if self.mode == Mode::CountTrash || self.roots.is_empty()
                        {
                            self.committed = true;
                            self.finish();
                        }
                        else
                        {
                            self.complete_plan()?;
                        }
                    }
                    _ => return Err(BAD_ABI)
                }
            }
            Tag::SourceCatalog =>
            {
                self.add_node(entry(reply)?, None)?;
            }
            Tag::SourceComment =>
            {
                if let Reply::Bytes(bytes) = reply
                {
                    self.comment_ready(bytes)?;
                }
                else
                {
                    return Err(BAD_ABI);
                }
            }
            Tag::SourceScan =>
            {
                if let Reply::Scan(call) = reply
                {
                    self.scan_ready(call)?;
                }
                else
                {
                    return Err(BAD_ABI);
                }
            }
            Tag::CaptureStamp =>
            {
                let value = entry(reply)?;
                let stamp = &mut self.stamps[self.cursor];
                if value.volume != stamp.reference.volume || value.id != stamp.reference.id
                {
                    return Err(BAD_ABI);
                }
                stamp.modified = value.modified;
                if value.id == ROOT_ID
                {
                    self.stage = Stage::CaptureVolumeStamp;
                }
                else
                {
                    self.cursor += 1;
                }
            }
            Tag::CaptureVolumeStamp =>
            {
                let value = mounted(reply)?;
                let stamp = &mut self.stamps[self.cursor];
                if value.id != stamp.reference.volume
                {
                    return Err(BAD_ABI);
                }
                stamp.volume_modified = Some(value.modified);
                self.cursor += 1;
                self.stage = Stage::CaptureParentStamps;
            }
            Tag::CreateTrash =>
            {
                let value = entry(reply)?;
                let volume = self.roots[self.cursor].input.volume;
                if value.volume != volume || value.parent != ROOT_ID || !value.is_directory()
                {
                    return Err(BAD_ABI);
                }
                // Capacity was reserved before the creation was issued.
                self.trash_dirs.push(SourceStage { unlocked: false,
                                                   root: usize::MAX,
                                                   volume,
                                                   directory: value.id,
                                                   removed: false });
                for root in &mut self.roots
                {
                    if root.input.volume == volume
                    {
                        root.target = Reference { volume,
                                                  id: value.id };
                    }
                }
                self.cursor += 1;
            }
            Tag::PrepareDestination =>
            {
                let value = entry(reply)?;
                if value.volume != self.target.volume
                   || value.parent != ROOT_ID
                   || !value.is_directory()
                {
                    return Err(BAD_ABI);
                }
                self.destination_stage = value.id;
                self.current_node = 0;
                self.stage = Stage::CreateNode;
            }
            Tag::CreateDirectory =>
            {
                let value = entry(reply)?;
                self.created_node(value, true)?;
                self.stage = Stage::CopyNext;
                self.stats.completed_items += 1;
            }
            Tag::BeginFile =>
            {
                let Reply::Stream { handle,
                                    completed,
                                    entry: _,
                                    bytes } = reply
                else
                {
                    return Err(BAD_ABI);
                };
                if handle == 0 || completed || !bytes.is_empty()
                {
                    return Err(BAD_ABI);
                }
                self.writer = handle;
                self.fork = DATA_FORK;
                self.offset = 0;
                self.chunk.clear();
                self.stage = Stage::OpenFork;
            }
            Tag::OpenFork =>
            {
                let Reply::Opened(handle, length) = reply
                else
                {
                    return Err(BAD_ABI);
                };
                // Retain ownership even when the source changed underneath us.
                self.reader = handle;
                if handle == 0 || length != self.bytes_in_fork()
                {
                    return Err(SCAN_STALE);
                }
                self.chunk_goal = length.min(CHUNK as u64) as usize;
                self.stage = Stage::ReadFork;
            }
            Tag::ReadFork =>
            {
                let Reply::Read { bytes,
                                  position,
                                  length } = reply
                else
                {
                    return Err(BAD_ABI);
                };
                let expected = self.offset.checked_add(self.chunk.len() as u64).ok_or(BAD_ABI)?;
                if bytes.is_empty()
                   || bytes.len() > self.chunk_goal - self.chunk.len()
                   || Some(position) != expected.checked_add(bytes.len() as u64)
                   || length != self.bytes_in_fork()
                {
                    return Err(SCAN_STALE);
                }
                self.chunk.extend_from_slice(&bytes);
            }
            Tag::AppendFork =>
            {
                let Reply::Stream { handle,
                                    completed,
                                    bytes,
                                    .. } = reply
                else
                {
                    return Err(BAD_ABI);
                };
                if handle != self.writer || completed || !bytes.is_empty()
                {
                    return Err(BAD_ABI);
                }
                self.stats.phase = TRANSFER_VERIFYING;
                self.stage = Stage::VerifyFork;
            }
            Tag::VerifyFork =>
            {
                let Reply::Stream { handle,
                                    completed,
                                    bytes,
                                    .. } = reply
                else
                {
                    return Err(BAD_ABI);
                };
                if handle != self.writer || completed || bytes != self.chunk
                {
                    return Err(CORRUPT_DATA);
                }
                self.offset += bytes.len() as u64;
                self.stats.completed_bytes += bytes.len() as u64;
                self.chunk.clear();
                self.stats.phase = TRANSFER_COPYING;
                if self.offset == self.bytes_in_fork()
                {
                    self.stage = Stage::CloseFork;
                }
                else
                {
                    self.chunk_goal =
                        (self.bytes_in_fork() - self.offset).min(CHUNK as u64) as usize;
                    self.stage = Stage::ReadFork;
                }
            }
            Tag::CloseFork =>
            {
                if !matches!(reply, Reply::Unmounted)
                {
                    return Err(BAD_ABI);
                }
                self.reader = 0;
                self.next_fork();
            }
            Tag::CloseReader =>
            {
                if !matches!(reply, Reply::Unmounted)
                {
                    return Err(BAD_ABI);
                }
                self.reader = 0;
                self.stage = if self.undo_complete || self.deferred_writer
                {
                    Stage::RetryHandles
                }
                else
                {
                    Stage::DrainGroup
                };
            }
            Tag::CommitFile | Tag::AbortWriter =>
            {
                let Reply::Stream { handle,
                                    completed,
                                    entry: value,
                                    bytes } = reply
                else
                {
                    return Err(BAD_ABI);
                };
                if handle != self.writer || !bytes.is_empty()
                {
                    return Err(BAD_ABI);
                }
                if completed
                {
                    self.writer = 0;
                    self.deferred_writer = false;
                    if value.id != 0
                    {
                        self.created_node(value, false)?;
                    }
                    if tag == Tag::AbortWriter
                    {
                        self.stage = if self.undo_complete
                        {
                            Stage::RetryHandles
                        }
                        else
                        {
                            Stage::DrainReader
                        };
                    }
                    else
                    {
                        self.stats.completed_items += 1;
                        self.stage = Stage::CopyNext;
                    }
                }
            }
            Tag::PrepareProperties | Tag::FinalizeProperties =>
            {
                let value = entry(reply)?;
                if value.id != self.nodes[self.cursor - 1].destination
                   || value.volume != self.target.volume
                {
                    return Err(BAD_ABI);
                }
                if tag == Tag::FinalizeProperties
                {
                    self.nodes[self.cursor - 1].finalized = true;
                }
                self.cursor -= 1;
            }
            Tag::VerifySource =>
            {
                let value = entry(reply)?;
                let node = &self.nodes[self.current_node];
                let source = &node.source;
                if value.volume != source.volume
                   || value.id != source.id
                   || value.parent != source.parent
                   || value.kind != source.kind
                   || value.name() != source.name()
                   || value.data_length != source.data_length
                   || value.resource_length != source.resource_length
                   || value.modified != source.modified
                   || value.created != source.created
                   || value.finder_info != source.finder_info
                   || value.flags != source.flags
                   || source.is_directory() && value.child_count != node.children
                {
                    return Err(SCAN_STALE);
                }
                self.current_node += 1;
            }
            Tag::PrepareSourceStage =>
            {
                let value = entry(reply)?;
                let volume = self.source_stages[self.cursor].volume;
                if value.volume != volume || value.parent != ROOT_ID || !value.is_directory()
                {
                    return Err(BAD_ABI);
                }
                self.source_stages[self.cursor].directory = value.id;
                self.cursor += 1;
                self.stage = Stage::PrepareSourceStage;
            }
            Tag::BeginGroup | Tag::StepGroup | Tag::CancelGroup => self.group_ready(tag, reply)?,
            Tag::RemoveSource =>
            {
                self.nodes[self.cursor - 1].removed = true;
                self.cursor -= 1;
            }
            Tag::CleanupDestination =>
            {
                self.destination_stage = 0;
                self.cursor = 0;
                self.stage = if self.committed
                {
                    Stage::CleanupSource
                }
                else
                {
                    Stage::RemoveRollbackSources
                };
            }
            Tag::CleanupSource =>
            {
                self.source_stages[self.cursor].removed = true;
                self.cursor += 1;
            }
            Tag::RestoreSourceProperties =>
            {
                entry(reply)?;
                self.roots[self.cursor].changed = false;
                self.cursor += 1;
            }
            Tag::RemoveCopy =>
            {
                self.nodes[self.cursor - 1].destination = 0;
                self.cursor -= 1;
            }
            Tag::RestoreStamp =>
            {
                self.cursor += 1;
            }
            Tag::UnlockTrash =>
            {
                entry(reply)?;
                self.trash_dirs.iter_mut().find(|s| !s.removed).ok_or(BAD_ABI)?.unlocked = true;
            }
            Tag::RemoveTrash =>
            {
                let stage = self.trash_dirs.iter_mut().find(|s| !s.removed).ok_or(BAD_ABI)?;
                stage.removed = true;
            }
            Tag::Collision => unreachable!()
        }
        Ok(())
    }

    fn created_node(&mut self, value: CatalogInfo, directory: bool) -> Result<(), Status>
    {
        if value.id <= ROOT_ID
           || value.volume != self.target.volume
           || value.is_directory() != directory
           || value.parent != self.destination_parent(self.current_node)?
        {
            return Err(BAD_ABI);
        }
        self.nodes[self.current_node].destination = value.id;
        self.nodes[self.current_node].copied = true;
        Ok(())
    }

    pub(super) fn collision_complete(&mut self) -> Result<(), Status>
    {
        let root = &self.roots[self.current_root];
        let key = mac_hfs_b::names::comparison(&root.name).map_err(|_| BAD_ARGUMENT)?;
        let candidate = (root.target.volume, root.target.id, key);
        match self.names.binary_search(&candidate)
        {
            Ok(_)
                if self.mode == Mode::Copy
                   && root.input.volume == root.target.volume
                   && root.input.expected_parent == root.target.id =>
            {
                self.copy_name()?;
                return Ok(());
            }
            Ok(_) => return Err(ALREADY_EXISTS),
            Err(at) =>
            {
                self.names.try_reserve(1).map_err(|_| NO_MEMORY)?;
                self.names.insert(at, candidate);
            }
        }
        self.current_root += 1;
        if self.current_root == self.roots.len()
        {
            self.complete_plan()?;
        }
        Ok(())
    }

    pub(super) fn complete_plan(&mut self) -> Result<(), Status>
    {
        // Reserve the complete undo topology before making any mutation.
        if self.mode == Mode::Copy || self.mode == Mode::Move && !self.fast_move
        {
            self.chunk.try_reserve_exact(CHUNK).map_err(|_| NO_MEMORY)?;
        }
        if !self.fast_move && matches!(self.mode, Mode::Move | Mode::DeleteTrash)
        {
            self.plan_source_stages()?;
            let mut protected = Vec::new();
            protected.try_reserve_exact(self.nodes.len()).map_err(|_| NO_MEMORY)?;
            protected.extend(self.nodes.iter().map(|node| (node.source.volume, node.source.id)));
            protected.sort_unstable();
            files(|m| {
                if m.handles
                    .iter()
                    .any(|(_, file)| protected.binary_search(&(file.volume, file.id)).is_ok())
                   || m.executables
                       .iter()
                       .any(|pin| protected.binary_search(&(pin.volume, pin.id)).is_ok())
                   || m.document_pins
                       .iter()
                       .any(|(_, volume, id)| protected.binary_search(&(*volume, *id)).is_ok())
                {
                    return Err(SERVICE_BUSY);
                }
                m.transfers.get_mut(self.handle).ok_or(NOT_FOUND)?.read_protected = protected;
                Ok(())
            })?;
        }
        for at in 0..self.roots.len()
        {
            let root = &self.roots[at];
            self.add_stamp(Reference { volume: root.input.volume,
                                       id:     root.input.expected_parent },
                           0)?;
            let target = self.roots[at].target;
            if target.id != 0
            {
                self.add_stamp(target, 0)?;
            }
        }
        // Root/volume dates are restored last, after all parent-date writes.
        let volumes = files(|m| {
            let entry = m.transfers.get(self.handle).ok_or(NOT_FOUND)?;
            let mut result = Vec::new();
            result.try_reserve_exact(entry.volumes.len()).map_err(|_| NO_MEMORY)?;
            result.extend_from_slice(&entry.volumes);
            Ok(result)
        })?;
        self.stamps.retain(|s| s.reference.id != ROOT_ID);
        for volume in volumes
        {
            self.stamps.try_reserve(1).map_err(|_| NO_MEMORY)?;
            self.stamps.push(Stamp { reference:       Reference { volume,
                                                                  id: ROOT_ID },
                                     modified:        0,
                                     volume_modified: None });
        }
        self.current_root = 0;
        self.cursor = 0;
        self.stage = Stage::CaptureParentStamps;
        Ok(())
    }
}
