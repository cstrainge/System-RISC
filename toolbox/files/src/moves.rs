//! Atomic same-volume icon-group moves, with source-parent revalidation.
use super::*;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Placement
{
    #[default]
    Keep,
    At(mac_abi::Point),
    Clear
}
#[derive(Clone, Copy, Debug, Default)]
pub struct IconMove
{
    pub id:              Cnid,
    pub expected_parent: Cnid,
    pub parent:          Cnid,
    pub placement:       Placement,
    pub to_trash:        bool
}
/// Stepped transaction presentation. Cancellation is complete only at Cancelled;
/// a racing commit is reported as Committed with its actual outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveProgress
{
    Applying
    {
        completed: u32,
        total:     u32
    },
    Undoing
    {
        remaining: u32,
        total:     u32
    },
    Committed
    {
        changed: bool
    },
    Cancelled
}
#[must_use = "dropping a group requests owned cleanup; cancel_step confirms its completion"]
pub struct IconMoveGroup<'a>
{
    client:   Client<'a>,
    handle:   u32,
    progress: MoveProgress,
    finished: bool
}
impl IconMoveGroup<'_>
{
    pub fn progress(&self) -> MoveProgress
    {
        self.progress
    }

    pub fn step(&mut self) -> Result<MoveProgress>
    {
        self.request(mac_abi::files::STEP_ICON_GROUP)
    }

    pub fn cancel_step(&mut self) -> Result<MoveProgress>
    {
        self.request(mac_abi::files::CANCEL_ICON_GROUP)
    }

    fn request(&mut self, operation: u32) -> Result<MoveProgress>
    {
        if self.finished
        {
            return Ok(self.progress);
        }
        let mut call = mac_abi::files::IconGroupCall { operation,
                                                       handle: self.handle,
                                                       ..Default::default() };
        self.client.icon_group(&mut call, &[])?;
        let progress = progress(&call)?;
        self.progress = progress;
        self.finished =
            matches!(progress, MoveProgress::Committed { .. } | MoveProgress::Cancelled);
        Ok(progress)
    }
}
impl Drop for IconMoveGroup<'_>
{
    fn drop(&mut self)
    {
        if !self.finished
        {
            let mut call = mac_abi::files::IconGroupCall { operation:
                                                               mac_abi::files::ABANDON_ICON_GROUP,
                                                           handle: self.handle,
                                                           ..Default::default() };
            let _ = self.client.icon_group(&mut call, &[]);
        }
    }
}
fn progress(call: &mac_abi::files::IconGroupCall) -> Result<MoveProgress>
{
    if call.count > 1024
       || call.completed > call.count
       || call.changed > 1
       || call.reserved != 0
       || (call.phase != 1 && call.changed != 0)
    {
        return Err(Error::BadAbi);
    }
    match call.phase
    {
        0 =>
        {
            Ok(MoveProgress::Applying { completed: call.completed,
                                        total:     call.count })
        }
        1 if call.completed == call.count =>
        {
            Ok(MoveProgress::Committed { changed: call.changed != 0 })
        }
        2 =>
        {
            Ok(MoveProgress::Undoing { remaining: call.completed,
                                       total:     call.count })
        }
        3 if call.completed == 0 => Ok(MoveProgress::Cancelled),
        _ => Err(Error::BadAbi)
    }
}
impl<'a> Files<'a>
{
    pub fn begin_icon_moves(self, volume: VolumeId, entries: &[IconMove])
                            -> Result<IconMoveGroup<'a>>
    {
        self.begin_icon_moves_at(volume, entries, self.client.now()?)
    }

    pub fn begin_icon_moves_at(self, volume: VolumeId, entries: &[IconMove], when: Timestamp)
                               -> Result<IconMoveGroup<'a>>
    {
        if entries.len() > 1024
        {
            return Err(Error::NoMemory);
        }
        let mut rows = [mac_abi::files::IconMove::default(); 1024];
        for (row, entry) in rows.iter_mut().zip(entries)
        {
            *row = encode(*entry);
        }
        let mut call = mac_abi::files::IconGroupCall { operation:
                                                           mac_abi::files::BEGIN_ICON_GROUP,
                                                       volume: volume.0,
                                                       when: when.mac_microseconds()
                                                                 .ok_or(Error::BadArgument)?,
                                                       ..Default::default() };
        self.client.icon_group(&mut call, &rows[..entries.len()])?;
        // Acquire RAII ownership before decoding a provider's response, so a
        // malformed success reply still requests resident cleanup on error.
        let mut owner = IconMoveGroup { client:   self.client,
                                        handle:   call.handle,
                                        progress: MoveProgress::Applying { completed: 0,
                                                                           total:     entries.len()
                                                                                      as u32 },
                                        finished: false };
        if call.handle == 0 || call.count != entries.len() as u32
        {
            return Err(Error::BadAbi);
        }
        owner.progress = progress(&call)?;
        owner.finished =
            matches!(owner.progress, MoveProgress::Committed { .. } | MoveProgress::Cancelled);
        Ok(owner)
    }
}
fn encode(entry: IconMove) -> mac_abi::files::IconMove
{
    let (placement, point) = match entry.placement
    {
        Placement::Keep => (0, mac_abi::Point::default()),
        Placement::At(point) => (1, point),
        Placement::Clear => (2, mac_abi::Point::default())
    };
    mac_abi::files::IconMove { cnid: entry.id.0,
                               expected_parent: entry.expected_parent.0,
                               parent: entry.parent.0,
                               placement: placement | if entry.to_trash { 4 } else { 0 },
                               point }
}
impl Files<'_>
{
    pub fn move_icons(self, volume: VolumeId, entries: &[IconMove]) -> Result<bool>
    {
        self.move_icons_at(volume, entries, self.client.now()?)
    }

    pub fn move_icons_at(self, volume: VolumeId, entries: &[IconMove], when: Timestamp)
                         -> Result<bool>
    {
        // Synchronous convenience over the same owned protocol as async moves.
        // This works on native and provider volumes and drains the receipt
        // before returning, so callers retain no hidden write lease.
        let mut group = self.begin_icon_moves_at(volume, entries, when)?;
        loop
        {
            match group.step()?
            {
                MoveProgress::Committed { changed } => return Ok(changed),
                MoveProgress::Cancelled => return Err(Error::Cancelled),
                MoveProgress::Applying { .. } | MoveProgress::Undoing { .. } => ()
            }
        }
    }
}
