//! Bounded two-fork staging. Every allocated payload block remains reachable
//! through an ordinary hidden catalog entry until one atomic publication.
use super::*;

pub const STAGING_DIRECTORY: &str = ".System RISC Transactions";
const MARKER: &str = "System RISC streamed-write staging v1";
pub const STREAM_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamAbort
{
    Pending,
    Aborted,
    Published
}

#[derive(Clone, Copy)]
pub struct StreamProperties<'a>
{
    pub flags:       u16,
    pub finder_info: [u8; 32],
    pub created:     Timestamp,
    pub modified:    Timestamp,
    pub backup:      Option<Timestamp>,
    pub comment:     &'a str
}
pub struct StreamSpec<'a>
{
    pub parent:          u32,
    pub name:            &'a str,
    /// Zero creates a file; otherwise replacement retains this exact CNID.
    pub expected:        u32,
    pub data_length:     u64,
    pub resource_length: u64,
    pub properties:      StreamProperties<'a>
}

/// The resident File Manager owns this state until publication or acknowledged
/// abort. It must retain failed cleanup for retry, rather than discarding it.
/// No payload bytes accumulate here and no live object IDs are issued here.
#[must_use = "retain staged files until publication or acknowledged abort"]
pub struct StagedFile
{
    uuid:        [u8; 16],
    stage:       u32,
    directory:   u32,
    parent:      u32,
    name:        String,
    expected:    Option<Entry>,
    flags:       u16,
    finder_info: [u8; 32],
    created:     Timestamp,
    modified:    Timestamp,
    backup:      Option<Timestamp>,
    comment:     String,
    lengths:     [u64; 2],
    written:     [u64; 2],
    published:   Option<u32>,
    aborted:     bool,
    aborting:    bool,
    cleaned:     bool
}
impl StagedFile
{
    pub fn replacement_id(&self) -> Option<u32>
    {
        self.expected.as_ref().map(|entry| entry.id)
    }

    pub fn written(&self, kind: ForkKind) -> u64
    {
        self.written[kind.index() as usize]
    }

    pub fn is_published(&self) -> bool
    {
        self.published.is_some()
    }
}
fn owned(value: &str) -> Result<String>
{
    String::from_utf8(copy(value.as_bytes())?).map_err(|_| Error::Invalid)
}
fn staging_name(id: u32) -> Result<String>
{
    const HEX: &[u8] = b"0123456789abcdef";
    let mut name = *b"pending-00000000";
    for (i, byte) in name[8..].iter_mut().enumerate()
    {
        *byte = HEX[((id >> ((7 - i) * 4)) & 15) as usize];
    }
    String::from_utf8(copy(&name)?).map_err(|_| Error::Invalid)
}
impl<D: WritableBlockDevice> Volume<'_, D>
{
    fn staging_directory(&mut self, when: Timestamp) -> Result<u32>
    {
        match self.lookup(2, STAGING_DIRECTORY)
        {
            Ok(entry) =>
            {
                if !entry.directory
                   || entry.flags != 0
                   || u16::from_be_bytes(entry.finder_info[8..10].try_into().unwrap()) != 0x4000
                   || self.comment(entry.id)? != MARKER
                {
                    return Err(Error::Protected);
                }
                return Ok(entry.id);
            }
            Err(Error::NotFound) =>
            {}
            Err(error) => return Err(error)
        }
        let mut root = self.entry(2)?;
        if root.flags & 1 != 0
        {
            return Err(Error::Protected);
        }
        let mut entry = Entry { id:          self.header.next_id,
                                parent:      2,
                                name:        owned(STAGING_DIRECTORY)?,
                                directory:   true,
                                flags:       0,
                                children:    0,
                                created:     when,
                                modified:    when,
                                backup:      None,
                                finder_info: [0; 32],
                                data:        Fork::default(),
                                resources:   Fork::default() };
        entry.finder_info[8..10].copy_from_slice(&0x4000u16.to_be_bytes());
        let mut tx = self.transaction()?;
        tx.header.next_id = tx.header.next_id.checked_add(1).ok_or(Error::Full)?;
        tx.entry(&entry)?;
        tx.put(0, &name_key(2, &entry.name)?, &entry.id.to_be_bytes())?;
        tx.put(2, &attr_key(entry.id, 0), MARKER.as_bytes())?;
        root.children = root.children.checked_add(1).ok_or(Error::Full)?;
        root.modified = when;
        tx.entry(&root)?;
        tx.header.modified = when.0;
        tx.commit()?;
        Ok(entry.id)
    }

    pub fn begin_stream(&mut self, spec: StreamSpec<'_>, when: Timestamp) -> Result<StagedFile>
    {
        let name = names::normalize(spec.name)?;
        if spec.properties.comment.len() > 255
           || spec.data_length.checked_add(spec.resource_length).is_none()
           || spec.data_length
                  .div_ceil(4096)
                  .checked_add(spec.resource_length.div_ceil(4096))
                  .is_none_or(|blocks| blocks > u64::from(self.header.blocks))
        {
            return Err(Error::Invalid);
        }
        let parent = self.entry(spec.parent)?;
        let reserved = match self.lookup(2, STAGING_DIRECTORY)
        {
            Ok(entry) => Some(entry.id),
            Err(Error::NotFound) => None,
            Err(error) => return Err(error)
        };
        if !parent.directory
           || reserved == Some(parent.id)
           || parent.id == 2 && names::comparison(&name)? == names::comparison(STAGING_DIRECTORY)?
        {
            return Err(Error::Invalid);
        }
        if parent.flags & 1 != 0
        {
            return Err(Error::Protected);
        }
        let expected = match self.lookup(spec.parent, &name)
        {
            Ok(entry) =>
            {
                if spec.expected == 0
                {
                    return Err(Error::Exists);
                }
                if entry.id != spec.expected
                {
                    return Err(Error::Stale);
                }
                if entry.directory
                {
                    return Err(Error::Invalid);
                }
                if entry.flags & 1 != 0
                {
                    return Err(Error::Protected);
                }
                Some(entry)
            }
            Err(Error::NotFound) if spec.expected == 0 => None,
            Err(Error::NotFound) => return Err(Error::Stale),
            Err(error) => return Err(error)
        };
        // Allocate all retained detail before publishing the provisional file.
        let comment = owned(spec.properties.comment)?;
        let directory = self.staging_directory(when)?;
        let stage_name = staging_name(self.header.next_id)?;
        let stage = self.create(directory, &stage_name, false, when)?;
        // The containing directory is invisible; the stage itself remains
        // unlocked so bounded writes and recovery can dispose its payload.
        Ok(StagedFile { uuid: self.header.uuid,
                        stage: stage.id,
                        directory,
                        parent: spec.parent,
                        name,
                        expected,
                        flags: spec.properties.flags,
                        finder_info: spec.properties.finder_info,
                        created: spec.properties.created,
                        modified: spec.properties.modified,
                        backup: spec.properties.backup,
                        comment,
                        lengths: [spec.data_length, spec.resource_length],
                        written: [0; 2],
                        published: None,
                        aborted: false,
                        aborting: false,
                        cleaned: false })
    }

    pub fn append_stream(&mut self, state: &mut StagedFile, kind: ForkKind, bytes: &[u8],
                         when: Timestamp)
                         -> Result<()>
    {
        if state.uuid != self.header.uuid
           || state.aborting
           || state.aborted
           || state.published.is_some()
           || bytes.len() > STREAM_CHUNK_BYTES
        {
            return Err(Error::Invalid);
        }
        let index = kind.index() as usize;
        let end = state.written[index].checked_add(bytes.len() as u64).ok_or(Error::Invalid)?;
        if end > state.lengths[index]
        {
            return Err(Error::Invalid);
        }
        let stage = self.entry(state.stage)?;
        if stage.parent != state.directory || stage.fork(kind).length != state.written[index]
        {
            return Err(Error::Stale);
        }
        self.write_fork(state.stage, kind, state.written[index], bytes, when)?;
        state.written[index] = end;
        Ok(())
    }

    pub fn read_stream(&mut self, state: &StagedFile, kind: ForkKind, offset: u64,
                       output: &mut [u8])
                       -> Result<usize>
    {
        if state.uuid != self.header.uuid
           || state.aborting
           || state.aborted
           || state.published.is_some()
           || output.len() > STREAM_CHUNK_BYTES
        {
            return Err(Error::Invalid);
        }
        self.read_fork(state.stage, kind, offset, output)
    }

    /// Publish both forks, dates, Finder metadata and comment in one catalog
    /// transaction. Replacement preserves the target's identity. Any failed
    /// transaction leaves the original and the owned stage available for retry.
    pub fn finish_stream(&mut self, state: &mut StagedFile, when: Timestamp) -> Result<Entry>
    {
        if state.uuid != self.header.uuid || state.aborting || state.aborted
        {
            return Err(Error::Invalid);
        }
        if let Some(id) = state.published
        {
            return self.entry(id);
        }
        if state.written != state.lengths
        {
            return Err(Error::Invalid);
        }
        let mut stage = self.entry(state.stage)?;
        if stage.parent != state.directory
           || stage.data.length != state.lengths[0]
           || stage.resources.length != state.lengths[1]
        {
            return Err(Error::Stale);
        }
        match (self.lookup(state.parent, &state.name), &state.expected)
        {
            (Err(Error::NotFound), None) =>
            {}
            (Ok(current), Some(expected)) if current == *expected =>
            {}
            (Ok(_), None) => return Err(Error::Exists),
            (Ok(_) | Err(Error::NotFound), Some(_)) => return Err(Error::Stale),
            (Err(error), _) => return Err(error)
        }
        let mut parent = self.entry(state.parent)?;
        if parent.flags & 1 != 0
        {
            return Err(Error::Protected);
        }
        let mut directory = self.entry(state.directory)?;
        let data = self.extent_list(stage.id, ForkKind::Data, &stage.data)?;
        let resources = self.extent_list(stage.id, ForkKind::Resource, &stage.resources)?;
        let (mut entry, old_data, old_resources) = if let Some(before) = &state.expected
        {
            (self.entry(before.id)?,
             self.extent_list(before.id, ForkKind::Data, &before.data)?,
             self.extent_list(before.id, ForkKind::Resource, &before.resources)?)
        }
        else
        {
            // Fallible clone of catalog-owned strings, before publication.
            let mut entry = self.entry(stage.id)?;
            entry.name = owned(&state.name)?;
            entry.parent = state.parent;
            (entry, Vec::new(), Vec::new())
        };
        let mut tx = self.transaction()?;
        if state.expected.is_some()
        {
            // Swap ownership of the two fork pairs atomically. Old payload
            // stays reachable in the hidden stage, then cleanup reclaims it
            // in bounded chunks. Publication never frees a whole large file.
            tx.replace_fork(&mut stage, ForkKind::Data, &data, &old_data, entry.data.length)?;
            tx.replace_fork(&mut stage,
                            ForkKind::Resource,
                            &resources,
                            &old_resources,
                            entry.resources.length)?;
            tx.remove(0, &name_key(entry.parent, &entry.name)?)?;
            entry.name = owned(&state.name)?;
        }
        else
        {
            tx.remove(0, &name_key(stage.parent, &stage.name)?)?;
            parent.children = parent.children.checked_add(1).ok_or(Error::Full)?;
            directory.children = directory.children.checked_sub(1).ok_or(Error::Corrupt)?;
        }
        entry.flags = state.flags;
        entry.finder_info = state.finder_info;
        entry.created = state.created;
        entry.modified = state.modified;
        entry.backup = state.backup;
        let existing_data = if state.expected.is_some() { &old_data } else { &data };
        let existing_resources = if state.expected.is_some() { &old_resources } else { &resources };
        tx.replace_fork(&mut entry, ForkKind::Data, existing_data, &data, state.lengths[0])?;
        tx.replace_fork(&mut entry,
                        ForkKind::Resource,
                        existing_resources,
                        &resources,
                        state.lengths[1])?;
        tx.put(0, &name_key(state.parent, &state.name)?, &entry.id.to_be_bytes())?;
        tx.put(2, &attr_key(entry.id, 0), state.comment.as_bytes())?;
        directory.modified = when;
        parent.modified = when;
        tx.entry(&directory)?;
        tx.entry(&parent)?;
        tx.header.modified = when.0;
        tx.commit()?;
        state.published = Some(entry.id);
        state.cleaned = state.expected.is_none();
        Ok(entry)
    }

    /// Reclaim at most one payload chunk. Keep the owner until Aborted; Pending
    /// requires another call and allows the scheduler to service input between
    /// chunks. A published file is never removed by cleanup.
    pub fn abort_stream(&mut self, state: &mut StagedFile, when: Timestamp) -> Result<StreamAbort>
    {
        if state.uuid != self.header.uuid
        {
            return Err(Error::Invalid);
        }
        if state.published.is_some() && state.cleaned
        {
            return Ok(StreamAbort::Published);
        }
        if state.aborted
        {
            return Ok(StreamAbort::Aborted);
        }
        state.aborting = true;
        let stage = self.entry(state.stage)?;
        if stage.parent != state.directory
        {
            return Err(Error::Stale);
        }
        if !self.reclaim_stream_payload(&stage, when)?
        {
            return Ok(StreamAbort::Pending);
        }
        state.cleaned = true;
        if state.published.is_some()
        {
            return Ok(StreamAbort::Published);
        }
        state.aborted = true;
        Ok(StreamAbort::Aborted)
    }

    fn reclaim_stream_payload(&mut self, entry: &Entry, when: Timestamp) -> Result<bool>
    {
        for kind in [ForkKind::Resource, ForkKind::Data]
        {
            let length = entry.fork(kind).length;
            if length != 0
            {
                self.set_fork_len(entry.id,
                                  kind,
                                  length.saturating_sub(STREAM_CHUNK_BYTES as u64),
                                  when)?;
                return Ok(false);
            }
        }
        self.remove(entry.id, when)?;
        Ok(true)
    }

    /// Called only during exclusive startup recovery, before applications can
    /// own streams. One bounded payload chunk is reclaimed per call; false
    /// means there are no remaining unpublished files.
    pub fn recover_stream_step(&mut self, when: Timestamp) -> Result<bool>
    {
        let directory = match self.lookup(2, STAGING_DIRECTORY)
        {
            Err(Error::NotFound) => return Ok(false),
            Err(error) => return Err(error),
            Ok(_) => self.staging_directory(when)?
        };
        let Some(entry) = self.next_child(directory, None)?
        else
        {
            return Ok(false);
        };
        if entry.directory || entry.name != staging_name(entry.id)? || entry.flags != 0
        {
            return Err(Error::Corrupt);
        }
        self.reclaim_stream_payload(&entry, when)?;
        Ok(true)
    }
}
