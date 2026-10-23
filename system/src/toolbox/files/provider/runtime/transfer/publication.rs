//! Publication and its exact reverse share the same resident group protocol.
//! Staged destinations are verified before any source can become disposable.
use fs::IconMovePhase;

use super::*;
impl State
{
    pub(super) fn plan_source_stages(&mut self) -> Result<(), Status>
    {
        // Roots from one folder already have distinct names. They share one
        // quarantine directory, rather than creating a directory per icon.
        let mut parents = Vec::<(u32, u32, usize)>::new();
        parents.try_reserve_exact(self.roots.len()).map_err(|_| NO_MEMORY)?;
        self.source_stages.try_reserve_exact(self.roots.len()).map_err(|_| NO_MEMORY)?;
        for (at, root) in self.roots.iter_mut().enumerate()
        {
            let key = (root.input.volume, root.input.expected_parent);
            let stage = match parents.binary_search_by_key(&key, |row| (row.0, row.1))
            {
                Ok(index) => parents[index].2,
                Err(index) =>
                {
                    let stage = self.source_stages.len();
                    self.source_stages.push(SourceStage { root:      at,
                                                          unlocked:  false,
                                                          volume:    key.0,
                                                          directory: 0,
                                                          removed:   false });
                    parents.insert(index, (key.0, key.1, stage));
                    stage
                }
            };
            root.source_stage = Some(stage);
        }
        Ok(())
    }

    pub(super) fn publication(&mut self) -> Result<(), Status>
    {
        if self.group.is_some()
        {
            let group = self.group.as_ref().unwrap();
            let cancel = self.stage == Stage::DrainGroup;
            let (operation, tag) = if group.handle == 0
            {
                if cancel
                {
                    self.group = None;
                    self.cursor = 0;
                    self.stage = Stage::UnpublishDestination;
                    return Ok(());
                }
                (BEGIN_ICON_GROUP, Tag::BeginGroup)
            }
            else if cancel
            {
                (CANCEL_ICON_GROUP, Tag::CancelGroup)
            }
            else
            {
                (STEP_ICON_GROUP, Tag::StepGroup)
            };
            let mut call = IconGroupCall { operation,
                                           volume: group.volume,
                                           handle: group.handle,
                                           count: if operation == BEGIN_ICON_GROUP
                                           {
                                               group.rows.len() as u32
                                           }
                                           else
                                           {
                                               0
                                           },
                                           when: self.when,
                                           ..Default::default() };
            self.effects = true;
            self.mutation(call.volume);
            let group = self.group.as_ref().unwrap();
            let progress =
                background::group(self.internal_owner(),
                                  &mut call,
                                  if operation == BEGIN_ICON_GROUP { &group.rows } else { &[] });
            self.set_io(tag, progress);
            return Ok(());
        }
        if self.stats.phase != TRANSFER_ROLLING_BACK
        {
            self.stats.phase = TRANSFER_COMMITTING;
        }
        match self.stage
        {
            Stage::PrepareSourceStage =>
            {
                if self.mode == Mode::Copy
                {
                    self.cursor = 0;
                    self.stage = Stage::PublishDestination;
                }
                else if self.cursor == self.source_stages.len()
                {
                    self.cursor = 0;
                    self.stage = Stage::QuarantineSource;
                }
                else
                {
                    let stage = &self.source_stages[self.cursor];
                    let (volume, root) = (stage.volume, stage.root);
                    let mut kind = String::new();
                    kind.try_reserve(40).map_err(|_| NO_MEMORY)?;
                    use core::fmt::Write;
                    write!(&mut kind, "source-{root}").map_err(|_| BAD_ARGUMENT)?;
                    self.private(volume, &kind)?;
                    let name = text(&self.private_name)?;
                    self.file(Tag::PrepareSourceStage,
                              super::super::super::super::properties::CREATE_PRIVATE_DIRECTORY,
                              Reference { volume,
                                          id: ROOT_ID },
                              0,
                              &name,
                              0);
                }
            }
            Stage::DrainGroup =>
            {
                self.cursor = 0;
                self.stage = Stage::UnpublishDestination;
            }
            Stage::QuarantineSource =>
            {
                self.prepare_group(GroupPurpose::Quarantine,
                                   if self.mode == Mode::DeleteTrash
                                   {
                                       Stage::CommitBarrier
                                   }
                                   else
                                   {
                                       Stage::PublishDestination
                                   })?
            }
            Stage::PublishDestination =>
            {
                self.prepare_group(if self.fast_move
                                   {
                                       GroupPurpose::FastMove
                                   }
                                   else
                                   {
                                       GroupPurpose::Publish
                                   },
                                   Stage::CommitBarrier)?
            }
            Stage::UnpublishDestination =>
            {
                self.prepare_group(GroupPurpose::Unpublish, Stage::RestoreSources)?
            }
            Stage::RestoreSources =>
            {
                self.prepare_group(GroupPurpose::Restore, Stage::RestoreSourceProperties)?
            }
            _ => return Err(BAD_ABI)
        }
        Ok(())
    }

    fn prepare_group(&mut self, purpose: GroupPurpose, next: Stage) -> Result<(), Status>
    {
        let selected = |root: &Root| {
            match purpose
            {
                GroupPurpose::FastMove | GroupPurpose::Publish => !root.published,
                GroupPurpose::Quarantine => !root.quarantined,
                GroupPurpose::Unpublish => root.published && !self.fast_move,
                GroupPurpose::Restore => root.quarantined || self.fast_move && root.published
            }
        };
        let Some(first) = self.roots.iter().position(selected)
        else
        {
            self.cursor = 0;
            self.stage = next;
            return Ok(());
        };
        let volume = if matches!(purpose, GroupPurpose::Publish | GroupPurpose::Unpublish)
        {
            self.target.volume
        }
        else
        {
            self.roots[first].input.volume
        };
        let mut roots = Vec::new();
        let mut rows = Vec::new();
        roots.try_reserve_exact(GROUP_CHUNK).map_err(|_| NO_MEMORY)?;
        rows.try_reserve_exact(GROUP_CHUNK).map_err(|_| NO_MEMORY)?;
        for (at, root) in self.roots.iter().enumerate()
        {
            if !selected(root)
            {
                continue;
            }
            let row_volume = if matches!(purpose, GroupPurpose::Publish | GroupPurpose::Unpublish)
            {
                self.target.volume
            }
            else
            {
                root.input.volume
            };
            if row_volume != volume
            {
                continue;
            }
            let node = &self.nodes[root.node.ok_or(BAD_ABI)?];
            let stage = || {
                self.source_stages
                    .get(root.source_stage.ok_or(BAD_ABI)?)
                    .map(|stage| stage.directory)
                    .filter(|id| *id > ROOT_ID)
                    .ok_or(BAD_ABI)
            };
            let (cnid, expected_parent, parent, placement, point) = match purpose
            {
                GroupPurpose::FastMove =>
                {
                    (node.source.id,
                     root.input.expected_parent,
                     root.target.id,
                     root.input.placement | if self.mode == Mode::Trash { 4 } else { 0 },
                     root.input.point)
                }
                GroupPurpose::Publish =>
                {
                    (node.destination,
                     self.destination_stage,
                     root.target.id,
                     root.input.placement,
                     root.input.point)
                }
                GroupPurpose::Quarantine =>
                {
                    (node.source.id, root.input.expected_parent, stage()?, 0, Point::default())
                }
                GroupPurpose::Unpublish =>
                {
                    (node.destination, root.target.id, self.destination_stage, 0, Point::default())
                }
                GroupPurpose::Restore =>
                {
                    let source = &node.source;
                    let point = source.icon_location();
                    let positioned = source.finder_flags() & 0x100 != 0
                                     && point
                                        != (Point { h: -1,
                                                    v: -1 });
                    (source.id,
                     if self.fast_move { root.target.id } else { stage()? },
                     root.input.expected_parent,
                     if positioned { 1 } else { 2 },
                     point)
                }
            };
            roots.push(at);
            rows.push(IconMove { cnid,
                                 expected_parent,
                                 parent,
                                 placement,
                                 point });
            if rows.len() == GROUP_CHUNK
            {
                break;
            }
        }
        self.group = Some(Group { handle: 0,
                                  purpose,
                                  roots,
                                  rows,
                                  volume });
        Ok(())
    }

    pub(super) fn group_ready(&mut self, tag: Tag, reply: Reply) -> Result<(), Status>
    {
        let Reply::Group { handle,
                           progress } = reply
        else
        {
            return Err(BAD_ABI);
        };
        let group = self.group.as_mut().ok_or(BAD_ABI)?;
        if handle == 0
           || (group.handle != 0 && group.handle != handle)
           || progress.total as usize != group.rows.len()
        {
            return Err(BAD_ABI);
        }
        group.handle = handle;
        if !progress.terminal()
        {
            return Ok(());
        }
        let group = self.group.take().unwrap();
        if progress.phase == IconMovePhase::Committed
        {
            // This remains reversible until the transfer commit barrier, even
            // if Stop raced with the group's own commit acknowledgement.
            for root in group.roots
            {
                match group.purpose
                {
                    GroupPurpose::FastMove | GroupPurpose::Publish =>
                    {
                        self.roots[root].published = true;
                        if self.fast_move
                        {
                            self.roots[root].changed = true;
                        }
                    }
                    GroupPurpose::Quarantine =>
                    {
                        self.roots[root].quarantined = true;
                        self.roots[root].changed = true;
                    }
                    GroupPurpose::Unpublish => self.roots[root].published = false,
                    GroupPurpose::Restore =>
                    {
                        self.roots[root].quarantined = false;
                        if self.fast_move
                        {
                            self.roots[root].published = false;
                        }
                    }
                }
            }
        }
        else if tag != Tag::CancelGroup
        {
            return Err(OPERATION_CANCELLED);
        }
        Ok(())
    }

    pub(super) fn restore_stamps(&mut self) -> Result<(), Status>
    {
        // Remove newly-created empty Trash folders before restoring their
        // parent/volume dates, so cleanup itself cannot re-date the volume.
        if let Some(stage) = self.trash_dirs.iter().find(|s| !s.removed)
        {
            let reference = Reference { volume: stage.volume,
                                        id:     stage.directory };
            if !stage.unlocked
            {
                self.properties(Tag::UnlockTrash,
                                reference,
                                CatalogInfo { created: self.when,
                                              modified: self.when,
                                              ..Default::default() },
                                "");
            }
            else
            {
                self.file(Tag::RemoveTrash, REMOVE, reference, 0, "", 0);
            }
            return Ok(());
        }
        // Source-only Copy is permitted from readonly volumes: they were never
        // mutated and need no restoration writes.
        while self.cursor < self.stamps.len()
        {
            let stamp = &self.stamps[self.cursor];
            if self.mutated.binary_search(&stamp.reference.volume).is_err()
            {
                self.cursor += 1;
                continue;
            }
            let mut call =
                FileCall { operation: super::super::super::super::properties::RESTORE_MODIFIED,
                           volume: stamp.reference.volume,
                           cnid: stamp.reference.id,
                           position: self.when,
                           length: stamp.modified,
                           index: u32::from(stamp.volume_modified.is_some()),
                           entry: CatalogInfo { modified: stamp.volume_modified.unwrap_or(0),
                                                ..Default::default() },
                           ..Default::default() };
            let progress = background::file(self.internal_owner(), &mut call, "", 0);
            self.set_io(Tag::RestoreStamp, progress);
            return Ok(());
        }
        if self.reader != 0 || self.writer != 0
        {
            self.undo_complete = true;
            self.stage = Stage::RetryHandles;
        }
        else
        {
            self.finish();
        }
        Ok(())
    }
}
