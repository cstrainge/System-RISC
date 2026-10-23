//! Bounded work scheduler: one owned service operation or one ledger transition
//! per pass. The UI never waits on an unbounded recursive filesystem call.
use super::*;
impl State
{
    pub(super) fn advance(&mut self)
    {
        if self.terminal()
        {
            return;
        }
        if self.stage == Stage::Failed
        {
            if crate::platform::ticks() < self.retry_at
            {
                return;
            }
            self.stage = self.retry_stage;
        }
        if let Some(flight) = self.io.take()
        {
            // Drain the actual receipt before interpreting Cancel. In
            // particular, a successful publication must reach our undo ledger.
            match super::super::advance(flight)
            {
                Progress::Pending(flight) =>
                {
                    self.io = Some(flight);
                    return;
                }
                Progress::Done(reply) => self.reply = Some(reply)
            }
        }
        if let Some(reply) = self.reply.take()
        {
            let tag = self.tag.take();
            let result = match tag
            {
                Some(tag) => self.accept(tag, reply),
                None => Err(BAD_ABI)
            };
            if let Err(error) = result
            {
                self.operation_failed(error);
            }
            return;
        }
        if self.cancelling && !self.committed && self.stats.phase != TRANSFER_ROLLING_BACK
        {
            if !self.effects
            {
                self.finish();
                return;
            }
            self.stats.phase = TRANSFER_ROLLING_BACK;
            self.stage = Stage::DrainWriter;
        }
        if let Err(error) = self.issue()
        {
            self.operation_failed(error);
        }
    }

    fn operation_failed(&mut self, error: Status)
    {
        if self.committed || self.stats.phase == TRANSFER_ROLLING_BACK
        {
            // Keep the ledger/leases for retry, at a bounded 250 ms cadence.
            // The primary cause and cleanup failure remain separate.
            self.stats.cleanup_error = error;
            self.retry_stage = self.stage;
            self.retry_at = crate::platform::ticks().saturating_add(2_500_000);
            self.stage = Stage::Failed;
        }
        else
        {
            self.fail(error);
        }
    }

    pub(super) fn file(&mut self, tag: Tag, operation: u32, reference: Reference, index: u32,
                       text: &str, capacity: usize)
    {
        if super::super::super::super::scan::writes_catalog(operation, index)
           || operation == super::super::super::super::properties::CREATE_PRIVATE_DIRECTORY
        {
            self.effects = true;
            self.mutation(reference.volume);
        }
        let mut call = FileCall { operation,
                                  volume: reference.volume,
                                  cnid: reference.id,
                                  index,
                                  position: self.when,
                                  ..Default::default() };
        let progress = background::file(self.internal_owner(), &mut call, text, capacity);
        self.set_io(tag, progress);
    }

    pub(super) fn properties(&mut self, tag: Tag, reference: Reference, entry: CatalogInfo,
                             comment: &str)
    {
        self.mutation(reference.volume);
        let mut call = FileCall { operation: SET_PROPERTIES,
                                  volume: reference.volume,
                                  cnid: reference.id,
                                  position: self.when,
                                  entry,
                                  ..Default::default() };
        let progress = background::file(self.internal_owner(), &mut call, comment, 0);
        self.set_io(tag, progress);
    }

    pub(super) fn private(&mut self, volume: u32, kind: &str) -> Result<(), Status>
    {
        use core::fmt::Write;
        self.private_name.clear();
        self.private_name.try_reserve(80).map_err(|_| NO_MEMORY)?;
        write!(&mut self.private_name,".System RISC {kind} {:08x}-{volume:08x}",self.handle).map_err(|_|BAD_ARGUMENT)
    }

    pub(super) fn issue(&mut self) -> Result<(), Status>
    {
        match self.stage
        {
            Stage::DestinationVolume =>
            {
                self.file(Tag::DestinationVolume, VOLUME_INFO, self.target, 0, "", 0)
            }
            Stage::DestinationCatalog =>
            {
                self.file(Tag::DestinationCatalog, CATALOG, self.target, 0, "", 0)
            }
            Stage::DestinationAncestry =>
            {
                self.file(Tag::DestinationAncestry,
                          CATALOG,
                          Reference { volume: self.target.volume,
                                      id:     self.ancestry },
                          0,
                          "",
                          0)
            }
            Stage::ScanTrash =>
            {
                let mut call = FileScanCall { operation: SCAN_DIRECTORY,
                                              volume: self.trash_volume,
                                              directory: self.trash_directory,
                                              cursor: self.trash_cursor,
                                              ..Default::default() };
                let progress = background::scan(self.internal_owner(), &mut call);
                self.set_io(Tag::ScanTrash, progress);
            }
            Stage::SourceVolume =>
            {
                self.stats.current = CatalogInfo::default();
                let source = self.roots[self.current_root].input;
                self.file(Tag::SourceVolume,
                          VOLUME_INFO,
                          Reference { volume: source.volume,
                                      id:     ROOT_ID },
                          0,
                          "",
                          0);
            }
            Stage::TrashDestination =>
            {
                let source = self.roots[self.current_root].input;
                self.file(Tag::TrashDestination,
                          TRASH_DIRECTORY,
                          Reference { volume: source.volume,
                                      id:     ROOT_ID },
                          0,
                          "",
                          0);
            }
            Stage::SourceCatalog =>
            {
                let source = self.roots[self.current_root].input;
                self.file(Tag::SourceCatalog,
                          CATALOG,
                          Reference { volume: source.volume,
                                      id:     source.cnid },
                          0,
                          "",
                          0);
            }
            Stage::SourceComment if self.mode == Mode::CountTrash =>
            {
                self.comment_ready(Vec::new())?
            }
            Stage::SourceComment =>
            {
                self.file(Tag::SourceComment,
                          COMMENT,
                          self.nodes[self.current_node].reference(),
                          0,
                          "",
                          COMMENT_MAX_BYTES)
            }
            Stage::SourceScan =>
            {
                let frame = self.frames.last().ok_or(BAD_ABI)?;
                let source = self.nodes[frame.node].reference();
                let mut call = FileScanCall { operation: SCAN_DIRECTORY,
                                              volume: source.volume,
                                              directory: source.id,
                                              cursor: frame.cursor,
                                              ..Default::default() };
                let progress = background::scan(self.internal_owner(), &mut call);
                self.set_io(Tag::SourceScan, progress);
            }
            Stage::NextRoot if self.mode.is_trash_query() => self.stage = Stage::ScanTrash,
            Stage::NextRoot =>
            {
                self.current_root += 1;
                if self.current_root < self.roots.len()
                {
                    self.stage = Stage::SourceVolume;
                }
                else
                {
                    self.current_root = 0;
                    self.stage = Stage::Collision;
                }
            }
            Stage::Collision =>
            {
                if let Some(node) = self.roots[self.current_root].node
                {
                    self.stats.current = self.nodes[node].source;
                }
                let root = &self.roots[self.current_root];
                if root.target.id == 0
                {
                    self.collision_complete()?;
                }
                else
                {
                    let name = text(&root.name)?;
                    self.file(Tag::Collision, LOOKUP, root.target, 0, &name, 0);
                }
            }
            Stage::CaptureParentStamps =>
            {
                if self.cursor == self.stamps.len()
                {
                    self.cursor = 0;
                    self.stage = if self.mode == Mode::DeleteTrash
                    {
                        Stage::PrepareSourceStage
                    }
                    else if self.mode == Mode::Trash
                    {
                        Stage::PrepareTrash
                    }
                    else if self.fast_move
                    {
                        Stage::PublishDestination
                    }
                    else
                    {
                        Stage::PrepareDestination
                    };
                    self.stats.phase =
                        if self.fast_move { TRANSFER_COMMITTING } else { TRANSFER_COPYING };
                }
                else
                {
                    self.file(Tag::CaptureStamp,
                              CATALOG,
                              self.stamps[self.cursor].reference,
                              0,
                              "",
                              0);
                }
            }
            Stage::CaptureVolumeStamp =>
            {
                self.file(Tag::CaptureVolumeStamp,
                          VOLUME_INFO,
                          self.stamps[self.cursor].reference,
                          0,
                          "",
                          0)
            }
            Stage::PrepareTrash =>
            {
                if self.cursor == self.roots.len()
                {
                    self.cursor = 0;
                    self.stage = Stage::PublishDestination;
                }
                else
                {
                    let root = &self.roots[self.cursor];
                    if root.target.id != 0
                    {
                        self.cursor += 1;
                    }
                    else
                    {
                        self.trash_dirs.try_reserve(1).map_err(|_| NO_MEMORY)?;
                        self.file(Tag::CreateTrash,
                                  TRASH_DIRECTORY,
                                  Reference { volume: root.input.volume,
                                              id:     ROOT_ID },
                                  1,
                                  "",
                                  0);
                    }
                }
            }
            Stage::PrepareDestination =>
            {
                self.private(self.target.volume, "transfer")?;
                let name = text(&self.private_name)?;
                self.file(Tag::PrepareDestination,
                          super::super::super::super::properties::CREATE_PRIVATE_DIRECTORY,
                          Reference { volume: self.target.volume,
                                      id:     ROOT_ID },
                          0,
                          &name,
                          0);
            }
            Stage::CreateNode =>
            {
                if self.current_node == self.nodes.len()
                {
                    self.cursor = self.nodes.len();
                    self.stage = Stage::PrepareProperties;
                }
                else
                {
                    self.stats.current = self.nodes[self.current_node].source;
                    if self.nodes[self.current_node].source.is_directory()
                    {
                        let name = text(self.name(self.current_node))?;
                        self.file(Tag::CreateDirectory,
                                  NEW_FOLDER,
                                  Reference { volume: self.target.volume,
                                              id:     self.destination_parent(self.current_node)? },
                                  0,
                                  &name,
                                  0);
                    }
                    else
                    {
                        self.stage = Stage::BeginFile;
                    }
                }
            }
            Stage::BeginFile =>
            {
                self.effects = true;
                let node = &self.nodes[self.current_node];
                let properties = node.properties(true, node.parent.is_none());
                let mut call = ForkWriteCall { operation: BEGIN_FORK_WRITE,
                                               volume: self.target.volume,
                                               parent:
                                                   self.destination_parent(self.current_node)?,
                                               when: self.when,
                                               data_length: node.source.data_length,
                                               resource_length: node.source.resource_length,
                                               created: properties.created,
                                               modified: properties.modified,
                                               backup: properties.backup,
                                               backup_present: properties.backup_present,
                                               attributes: properties.flags,
                                               finder_info: properties.finder_info,
                                               ..Default::default() };
                let progress = background::stream(self.internal_owner(),
                                                  &mut call,
                                                  self.name(self.current_node),
                                                  &node.comment,
                                                  &[],
                                                  0);
                self.set_io(Tag::BeginFile, progress);
            }
            Stage::OpenFork =>
            {
                if self.bytes_in_fork() == 0
                {
                    self.next_fork();
                }
                else
                {
                    let source = self.nodes[self.current_node].source;
                    let mut call = FileCall { operation: OPEN,
                                              volume: source.volume,
                                              cnid: source.id,
                                              fork: self.fork,
                                              ..Default::default() };
                    let progress = background::file(self.internal_owner(), &mut call, "", 0);
                    self.set_io(Tag::OpenFork, progress);
                }
            }
            Stage::ReadFork =>
            {
                let remaining = self.bytes_in_fork()
                                    .checked_sub(self.offset + self.chunk.len() as u64)
                                    .ok_or(BAD_ABI)?;
                if remaining == 0 && self.chunk.is_empty()
                {
                    self.stage = Stage::CloseFork;
                }
                else if self.chunk.len() == self.chunk_goal
                {
                    self.stage = Stage::AppendFork;
                }
                else
                {
                    self.stats.phase = TRANSFER_READING;
                    let mut call = FileCall { operation: READ,
                                              handle: self.reader,
                                              ..Default::default() };
                    let progress = background::file(self.internal_owner(),
                                                    &mut call,
                                                    "",
                                                    self.chunk_goal - self.chunk.len());
                    self.set_io(Tag::ReadFork, progress);
                }
            }
            Stage::AppendFork =>
            {
                self.stats.phase = TRANSFER_WRITING;
                let mut call = ForkWriteCall { operation: APPEND_FORK_WRITE,
                                               handle: self.writer,
                                               fork: self.fork,
                                               ..Default::default() };
                // Appends are sequential. Offset belongs only to the verify
                // read; FSYS correctly rejects a nonzero WriteChunk offset.
                let progress =
                    background::stream(self.internal_owner(), &mut call, "", "", &self.chunk, 0);
                self.set_io(Tag::AppendFork, progress);
            }
            Stage::VerifyFork =>
            {
                let mut call = ForkWriteCall { operation: READ_FORK_WRITE,
                                               handle: self.writer,
                                               fork: self.fork,
                                               offset: self.offset,
                                               ..Default::default() };
                let progress = background::stream(self.internal_owner(),
                                                  &mut call,
                                                  "",
                                                  "",
                                                  &[],
                                                  self.chunk.len());
                self.set_io(Tag::VerifyFork, progress);
            }
            Stage::CloseFork | Stage::DrainReader =>
            {
                if self.reader == 0
                {
                    self.stage =
                        if self.deferred_writer { Stage::RetryHandles } else { Stage::DrainGroup };
                }
                else
                {
                    let mut call = FileCall { operation: CLOSE,
                                              handle: self.reader,
                                              ..Default::default() };
                    let progress = background::file(self.internal_owner(), &mut call, "", 0);
                    self.set_io(if self.stage == Stage::CloseFork
                                {
                                    Tag::CloseFork
                                }
                                else
                                {
                                    Tag::CloseReader
                                },
                                progress);
                }
            }
            Stage::CommitFile | Stage::DrainWriter =>
            {
                if self.writer == 0
                {
                    self.stage = Stage::DrainReader;
                }
                else
                {
                    let mut call = ForkWriteCall { operation: if self.stage == Stage::CommitFile
                                                   {
                                                       FINISH_FORK_WRITE
                                                   }
                                                   else
                                                   {
                                                       ABORT_FORK_WRITE
                                                   },
                                                   handle: self.writer,
                                                   ..Default::default() };
                    let progress =
                        background::stream(self.internal_owner(), &mut call, "", "", &[], 0);
                    self.set_io(if self.stage == Stage::CommitFile
                                {
                                    Tag::CommitFile
                                }
                                else
                                {
                                    Tag::AbortWriter
                                },
                                progress);
                }
            }
            Stage::CopyNext =>
            {
                self.current_node += 1;
                self.stage = Stage::CreateNode;
            }
            Stage::PrepareProperties | Stage::FinalizeProperties =>
            {
                if self.cursor == 0
                {
                    if self.stage == Stage::PrepareProperties
                    {
                        self.current_node = 0;
                        self.stage = Stage::VerifySource;
                    }
                    else
                    {
                        self.cursor = self.nodes.len();
                        self.stage = Stage::RemoveSource;
                        self.stats.phase = TRANSFER_CLEANING_UP;
                    }
                }
                else if self.fast_move || self.mode == Mode::DeleteTrash
                {
                    self.cursor = 0;
                }
                else
                {
                    let at = self.cursor - 1;
                    let node = &self.nodes[at];
                    let provisional = self.stage == Stage::PrepareProperties;
                    let properties = self.final_properties(at, provisional);
                    let comment = text(&node.comment)?;
                    self.properties(if provisional
                                    {
                                        Tag::PrepareProperties
                                    }
                                    else
                                    {
                                        Tag::FinalizeProperties
                                    },
                                    Reference { volume: self.target.volume,
                                                id:     node.destination },
                                    properties,
                                    &comment);
                }
            }
            Stage::VerifySource =>
            {
                if self.current_node == self.nodes.len()
                {
                    self.cursor = 0;
                    self.stage = Stage::PrepareSourceStage;
                }
                else
                {
                    self.file(Tag::VerifySource,
                              CATALOG,
                              self.nodes[self.current_node].reference(),
                              0,
                              "",
                              0);
                }
            }
            Stage::PrepareSourceStage
            | Stage::QuarantineSource
            | Stage::PublishDestination
            | Stage::UnpublishDestination
            | Stage::RestoreSources
            | Stage::DrainGroup => self.publication()?,
            Stage::CommitBarrier => (), // decided under the table/Cancel guard
            Stage::RemoveSource =>
            {
                if self.mode == Mode::Copy || self.fast_move
                {
                    self.cursor = 0;
                    self.stage = Stage::CleanupDestination;
                }
                else if self.cursor == 0
                {
                    self.stage = Stage::CleanupDestination;
                }
                else
                {
                    let at = self.cursor - 1;
                    if self.nodes[at].removed
                    {
                        self.cursor -= 1;
                    }
                    else
                    {
                        self.file(Tag::RemoveSource, REMOVE, self.nodes[at].reference(), 0, "", 0);
                    }
                }
            }
            Stage::CleanupDestination | Stage::RemoveRollbackDestination =>
            {
                if self.destination_stage == 0
                {
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
                else
                {
                    self.file(Tag::CleanupDestination,
                              REMOVE,
                              Reference { volume: self.target.volume,
                                          id:     self.destination_stage },
                              0,
                              "",
                              0);
                }
            }
            Stage::CleanupSource | Stage::RemoveRollbackSources =>
            {
                if self.cursor == self.source_stages.len()
                {
                    self.cursor = 0;
                    if self.committed
                    {
                        self.finish();
                    }
                    else
                    {
                        self.stage = Stage::RestoreStamps;
                    }
                }
                else
                {
                    let stage = &self.source_stages[self.cursor];
                    if stage.removed || stage.directory == 0
                    {
                        self.cursor += 1;
                    }
                    else
                    {
                        self.file(Tag::CleanupSource,
                                  REMOVE,
                                  Reference { volume: stage.volume,
                                              id:     stage.directory },
                                  0,
                                  "",
                                  0);
                    }
                }
            }
            Stage::RestoreSourceProperties =>
            {
                if self.cursor == self.roots.len()
                {
                    self.cursor = self.nodes.len();
                    self.stage = Stage::RemoveCopies;
                }
                else if !self.roots[self.cursor].changed
                {
                    self.cursor += 1;
                }
                else
                {
                    let node = &self.nodes[self.roots[self.cursor].node.ok_or(BAD_ABI)?];
                    let properties = node.properties(false, true);
                    let comment = text(&node.comment)?;
                    self.properties(Tag::RestoreSourceProperties,
                                    node.reference(),
                                    properties,
                                    &comment);
                }
            }
            Stage::RemoveCopies =>
            {
                if self.cursor == 0
                {
                    self.stage = Stage::RemoveRollbackDestination;
                }
                else
                {
                    let node = &self.nodes[self.cursor - 1];
                    if node.destination == 0
                    {
                        self.cursor -= 1;
                    }
                    else
                    {
                        self.file(Tag::RemoveCopy,
                                  REMOVE,
                                  Reference { volume: self.target.volume,
                                              id:     node.destination },
                                  0,
                                  "",
                                  0);
                    }
                }
            }
            Stage::RestoreStamps => self.restore_stamps()?,
            Stage::RetryHandles =>
            {
                if self.writer != 0
                {
                    let mut call = ForkWriteCall { operation: ABORT_FORK_WRITE,
                                                   handle: self.writer,
                                                   ..Default::default() };
                    let progress =
                        background::stream(self.internal_owner(), &mut call, "", "", &[], 0);
                    self.set_io(Tag::AbortWriter, progress);
                }
                else if self.reader != 0
                {
                    let mut call = FileCall { operation: CLOSE,
                                              handle: self.reader,
                                              ..Default::default() };
                    let progress = background::file(self.internal_owner(), &mut call, "", 0);
                    self.set_io(Tag::CloseReader, progress);
                }
                else
                {
                    self.finish();
                }
            }
            Stage::Complete | Stage::Failed => ()
        }
        Ok(())
    }

    pub(super) fn next_fork(&mut self)
    {
        self.reader = 0;
        self.offset = 0;
        self.chunk.clear();
        if self.fork == DATA_FORK
        {
            self.fork = RESOURCE_FORK;
            self.stage = Stage::OpenFork;
        }
        else
        {
            self.stage = Stage::CommitFile;
        }
    }

    pub(super) fn final_properties(&self, at: usize, provisional: bool) -> CatalogInfo
    {
        let node = &self.nodes[at];
        let mut properties = node.properties(provisional, node.parent.is_none());
        if node.parent.is_none() && !provisional
        {
            let root = &self.roots[node.root];
            if root.input.placement != 0
            {
                let flags = u16::from_be_bytes(properties.finder_info[8..10].try_into().unwrap());
                let (flags, point) = if root.input.placement == 1
                {
                    (flags | 0x100, root.input.point)
                }
                else
                {
                    (flags & !0x100,
                     Point { h: -1,
                             v: -1 })
                };
                properties.finder_info[8..10].copy_from_slice(&flags.to_be_bytes());
                properties.finder_info[10..12].copy_from_slice(&point.v.to_be_bytes());
                properties.finder_info[12..14].copy_from_slice(&point.h.to_be_bytes());
            }
        }
        properties
    }

    pub(super) fn finish(&mut self)
    {
        self.stats.committed = self.committed;
        self.stats.phase = if self.committed
        {
            TRANSFER_COMMITTED
        }
        else if self.stats.error == OK || self.stats.error == OPERATION_CANCELLED
        {
            TRANSFER_CANCELLED
        }
        else
        {
            TRANSFER_FAILED
        };
        self.stats.cleanup_error = OK;
        if self.committed
        {
            self.stats.completed_items = self.stats.total_items;
            self.stats.completed_bytes = self.stats.total_bytes;
        }
        self.stage = Stage::Complete;
    }
}
