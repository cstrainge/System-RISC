//! Owned transfer plan, publication ledger and bounded traversal state.
use super::*;
pub(super) const MAX_NODES: usize = 4096;
pub(super) const MAX_DEPTH: usize = 128;
pub(super) const GROUP_CHUNK: usize = 16;
pub(super) const CHUNK: usize = 64 * 1024;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode
{
    Copy,
    Move,
    Trash,
    DeleteTrash,
    CountTrash
}
impl Mode
{
    pub fn is_trash_query(self) -> bool
    {
        matches!(self, Self::DeleteTrash | Self::CountTrash)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Reference
{
    pub volume: u32,
    pub id:     u32
}
#[derive(Clone, Copy)]
pub(super) struct Snapshot
{
    pub phase:           u32,
    pub completed_items: u32,
    pub total_items:     u32,
    pub completed_bytes: u64,
    pub total_bytes:     u64,
    pub allocated_bytes: u64,
    pub error:           Status,
    pub cleanup_error:   Status,
    pub committed:       bool,
    pub current:         CatalogInfo
}
impl Default for Snapshot
{
    fn default() -> Self
    {
        Self { phase:           TRANSFER_PREPARING,
               completed_items: 0,
               total_items:     0,
               completed_bytes: 0,
               total_bytes:     0,
               allocated_bytes: 0,
               error:           OK,
               cleanup_error:   OK,
               committed:       false,
               current:         CatalogInfo::default() }
    }
}
impl Snapshot
{
    pub fn changed(self, old: Self) -> bool
    {
        self.phase != old.phase
        || self.completed_items != old.completed_items
        || self.total_items != old.total_items
        || self.completed_bytes != old.completed_bytes
        || self.total_bytes != old.total_bytes
        || self.error != old.error
        || self.cleanup_error != old.cleanup_error
        || self.committed != old.committed
        || self.current.volume != old.current.volume
        || self.current.id != old.current.id
    }

    pub fn fill(self, call: &mut FileTransferCall)
    {
        call.phase = self.phase;
        call.commit_decided = u32::from(self.committed);
        call.completed_items = self.completed_items;
        call.total_items = self.total_items;
        call.completed_bytes = self.completed_bytes;
        call.total_bytes = self.total_bytes;
        call.error = self.error;
        call.cleanup_error = self.cleanup_error;
        call.current = self.current;
    }

    pub fn terminal(self) -> bool
    {
        matches!(self.phase, TRANSFER_COMMITTED | TRANSFER_CANCELLED | TRANSFER_FAILED)
    }
}
pub(super) struct Root
{
    pub input:        FileTransferSource,
    pub node:         Option<usize>,
    pub source_stage: Option<usize>,
    pub target:       Reference,
    pub name:         String,
    pub name_attempt: u32,
    pub quarantined:  bool,
    pub changed:      bool,
    pub published:    bool
}
pub(super) struct Node
{
    pub source:      CatalogInfo,
    pub children:    u32,
    pub comment:     String,
    pub parent:      Option<usize>,
    pub root:        usize,
    pub destination: u32,
    pub copied:      bool,
    pub finalized:   bool,
    pub removed:     bool
}
impl Node
{
    pub fn reference(&self) -> Reference
    {
        Reference { volume: self.source.volume,
                    id:     self.source.id }
    }

    pub fn properties(&self, provisional: bool, root: bool) -> CatalogInfo
    {
        let mut info = self.source.finder_info;
        let mut attributes = self.source.flags;
        if provisional
        {
            attributes &= !FILE_LOCKED;
            let mut flags =
                u16::from_be_bytes(info[8..10].try_into().unwrap()) & !FINDER_NAME_LOCKED;
            if root
            {
                flags |= 0x4000;
            }
            info[8..10].copy_from_slice(&flags.to_be_bytes());
        }
        CatalogInfo { flags: attributes,
                      finder_info: info,
                      created: self.source.created,
                      modified: self.source.modified,
                      backup: self.source.backup,
                      backup_present: self.source.backup_present,
                      ..Default::default() }
    }
}
pub(super) struct Frame
{
    pub node:   usize,
    pub cursor: DirectoryScanCursor
}
pub(super) struct Stamp
{
    pub reference:       Reference,
    pub modified:        u64,
    pub volume_modified: Option<u64>
}
pub(super) struct SourceStage
{
    pub root:      usize,
    pub unlocked:  bool,
    pub volume:    u32,
    pub directory: u32,
    pub removed:   bool
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage
{
    DestinationVolume,
    DestinationCatalog,
    DestinationAncestry,
    ScanTrash,
    SourceVolume,
    TrashDestination,
    SourceCatalog,
    SourceComment,
    SourceScan,
    NextRoot,
    Collision,
    CaptureParentStamps,
    CaptureVolumeStamp,
    PrepareTrash,
    PrepareDestination,
    CreateNode,
    BeginFile,
    OpenFork,
    ReadFork,
    AppendFork,
    VerifyFork,
    CloseFork,
    CommitFile,
    CopyNext,
    PrepareProperties,
    VerifySource,
    PrepareSourceStage,
    QuarantineSource,
    PublishDestination,
    CommitBarrier,
    FinalizeProperties,
    RemoveSource,
    CleanupDestination,
    CleanupSource,
    Complete,
    DrainWriter,
    DrainReader,
    DrainGroup,
    UnpublishDestination,
    RestoreSources,
    RestoreSourceProperties,
    RemoveCopies,
    RemoveRollbackDestination,
    RemoveRollbackSources,
    RestoreStamps,
    RetryHandles,
    Failed
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tag
{
    TrashDestination,
    CreateTrash,
    VerifySource,
    DestinationVolume,
    DestinationCatalog,
    DestinationAncestry,
    ScanTrash,
    SourceVolume,
    SourceCatalog,
    SourceComment,
    SourceScan,
    Collision,
    CaptureStamp,
    CaptureVolumeStamp,
    PrepareDestination,
    CreateDirectory,
    BeginFile,
    OpenFork,
    ReadFork,
    AppendFork,
    VerifyFork,
    CloseFork,
    CommitFile,
    PrepareProperties,
    PrepareSourceStage,
    BeginGroup,
    StepGroup,
    CancelGroup,
    FinalizeProperties,
    RemoveSource,
    CleanupDestination,
    CleanupSource,
    AbortWriter,
    CloseReader,
    RestoreSourceProperties,
    RemoveCopy,
    RestoreStamp,
    RemoveTrash,
    UnlockTrash
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum GroupPurpose
{
    FastMove,
    Quarantine,
    Publish,
    Unpublish,
    Restore
}
pub(super) struct Group
{
    pub handle:  u32,
    pub purpose: GroupPurpose,
    pub roots:   Vec<usize>,
    pub rows:    Vec<IconMove>,
    pub volume:  u32
}
pub(super) struct State
{
    pub scope:             crate::toolbox::files::subject::Lease,
    pub handle:            u32,
    pub io:                Option<Flight>,
    pub reply:             Option<Result<Reply, Status>>,
    pub tag:               Option<Tag>,
    pub stage:             Stage,
    pub stats:             Snapshot,
    pub mode:              Mode,
    pub when:              u64,
    pub target:            Reference,
    pub roots:             Vec<Root>,
    pub trash_volume:      u32,
    pub trash_directory:   u32,
    pub trash_cursor:      DirectoryScanCursor,
    pub nodes:             Vec<Node>,
    pub seen:              Vec<(u32, u32)>,
    pub frames:            Vec<Frame>,
    pub ancestors:         Vec<u32>,
    pub names:             Vec<(u32, u32, String)>,
    pub source_stages:     Vec<SourceStage>,
    pub trash_dirs:        Vec<SourceStage>,
    pub stamps:            Vec<Stamp>,
    pub mutated:           Vec<u32>,
    pub destination_stage: u32,
    pub current_root:      usize,
    pub current_node:      usize,
    pub cursor:            usize,
    pub ancestry:          u32,
    pub fork:              u32,
    pub offset:            u64,
    pub chunk:             Vec<u8>,
    pub chunk_goal:        usize,
    pub reader:            u32,
    pub writer:            u32,
    pub group:             Option<Group>,
    pub put_away:          bool,
    pub fast_move:         bool,
    pub cancelling:        bool,
    pub committed:         bool,
    pub effects:           bool,
    pub undo_complete:     bool,
    pub deferred_writer:   bool,
    pub retry_at:          u64,
    pub retry_stage:       Stage,
    pub private_name:      String
}
impl State
{
    pub fn new(call: &FileTransferCall, sources: &[FileTransferSource],
               scope: crate::toolbox::files::subject::Lease)
               -> Result<Self, Status>
    {
        let mut roots = Vec::new();
        roots.try_reserve_exact(sources.len()).map_err(|_| NO_MEMORY)?;
        let mut mutated = Vec::new();
        mutated.try_reserve_exact(sources.len() + 1).map_err(|_| NO_MEMORY)?;
        for input in sources
        {
            roots.push(Root { input:        *input,
                              node:         None,
                              source_stage: None,
                              target:       Reference { volume: call.destination_volume,
                                                        id:     call.destination_parent },
                              name:         String::new(),
                              name_attempt: 0,
                              quarantined:  false,
                              changed:      false,
                              published:    false });
        }
        Ok(Self { scope,
                  handle: 0,
                  io: None,
                  reply: None,
                  tag: None,
                  stage: if call.mode == TRANSFER_TRASH
                  {
                      Stage::SourceVolume
                  }
                  else
                  {
                      Stage::DestinationVolume
                  },
                  stats: Snapshot::default(),
                  mode: match call.mode
                  {
                      TRANSFER_COPY => Mode::Copy,
                      TRANSFER_MOVE => Mode::Move,
                      TRANSFER_TRASH => Mode::Trash,
                      _ => return Err(BAD_ARGUMENT)
                  },
                  when: call.when,
                  target: Reference { volume: call.destination_volume,
                                      id:     call.destination_parent },
                  roots,
                  trash_volume: 0,
                  trash_directory: 0,
                  trash_cursor: DirectoryScanCursor::default(),
                  nodes: Vec::new(),
                  seen: Vec::new(),
                  frames: Vec::new(),
                  ancestors: Vec::new(),
                  names: Vec::new(),
                  source_stages: Vec::new(),
                  trash_dirs: Vec::new(),
                  stamps: Vec::new(),
                  mutated,
                  destination_stage: 0,
                  current_root: 0,
                  current_node: 0,
                  cursor: 0,
                  ancestry: call.destination_parent,
                  fork: DATA_FORK,
                  offset: 0,
                  chunk: Vec::new(),
                  chunk_goal: 0,
                  reader: 0,
                  writer: 0,
                  group: None,
                  put_away: false,
                  fast_move:
                      call.mode == TRANSFER_TRASH
                      || call.mode == TRANSFER_MOVE
                         && sources.iter().all(|source| source.volume == call.destination_volume),
                  cancelling: false,
                  committed: false,
                  effects: false,
                  undo_complete: false,
                  deferred_writer: false,
                  retry_at: 0,
                  retry_stage: Stage::Complete,
                  private_name: String::new() })
    }

    pub fn access(&self) -> crate::toolbox::files::subject::Access<'_>
    {
        crate::toolbox::files::subject::Access::Operation(&self.scope)
    }

    pub fn mutation(&mut self, volume: u32)
    {
        if let Err(at) = self.mutated.binary_search(&volume)
        {
            // Capacity for every admitted source plus destination was reserved
            // before the first I/O; a mutation never needs to grow this ledger.
            self.mutated.insert(at, volume);
        }
    }

    pub fn terminal(&self) -> bool
    {
        self.stats.terminal() && self.stage == Stage::Complete
    }

    pub fn set_io(&mut self, tag: Tag, progress: Progress)
    {
        self.tag = Some(tag);
        match progress
        {
            Progress::Pending(flight) => self.io = Some(flight),
            Progress::Done(reply) => self.reply = Some(reply)
        }
    }

    pub fn fail(&mut self, cause: Status)
    {
        if self.stats.error == OK
        {
            self.stats.error = cause;
        }
        self.cancelling = true;
    }

    pub fn name(&self, node: usize) -> &str
    {
        let entry = &self.nodes[node];
        if entry.parent.is_none() { &self.roots[entry.root].name } else { entry.source.name() }
    }

    pub fn destination_parent(&self, node: usize) -> Result<u32, Status>
    {
        Ok(match self.nodes[node].parent
        {
            Some(parent) => self.nodes[parent].destination,
            None => self.destination_stage
        })
    }

    pub fn bytes_in_fork(&self) -> u64
    {
        if self.fork == DATA_FORK
        {
            self.nodes[self.current_node].source.data_length
        }
        else
        {
            self.nodes[self.current_node].source.resource_length
        }
    }
}
pub(super) fn text(value: &str) -> Result<String, Status>
{
    let mut result = String::new();
    result.try_reserve_exact(value.len()).map_err(|_| NO_MEMORY)?;
    result.push_str(value);
    Ok(result)
}
