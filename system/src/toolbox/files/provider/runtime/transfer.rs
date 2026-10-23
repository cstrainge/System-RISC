//! Resident ownership of recursive transfers. Requests only borrow caller
//! memory during admission. Every subsequent I/O and undo receipt is owned.
#![forbid(unsafe_code)]
use super::*;
use crate::toolbox::handles::Kind;
mod model;
mod plan;
mod engine;
mod replies;
mod publication;
pub(in crate::toolbox::files) mod bridge;
use model::*;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static PUMPING: AtomicBool = AtomicBool::new(false);
static PARKED: Spinlock<[Option<Box<State>>; Kind::FileTransfers.capacity()]> =
    Spinlock::new([const { None }; Kind::FileTransfers.capacity()]);
// Application identities are u32 generation-tagged IDs. A coordinator's I/O
// owner is disjoint and cannot be supplied by any public request record.
fn internal_owner(handle: u32) -> u64
{
    (1u64 << 63) | u64::from(handle)
}

pub(in crate::toolbox::files) struct Entry
{
    bridge:         Option<bridge::Bridge>,
    mutating:       bool,
    read_protected: Vec<(u32, u32)>,
    owner:          u64,
    volumes:        Vec<u32>,
    state:          Option<Box<State>>,
    snapshot:       Snapshot,
    cancel:         bool,
    abandoned:      bool,
    notice_due:     bool,
    notice_pending: bool
}
impl Entry
{
    pub(in crate::toolbox::files) fn uses(&self, volume: u32) -> bool
    {
        !self.snapshot.terminal() && self.volumes.contains(&volume)
    }

    pub(in crate::toolbox::files) fn protects_read(&self, handle: u32, owner: u64, volume: u32,
                                                   id: u32)
                                                   -> bool
    {
        self.uses(volume)
        && internal_owner(handle) != owner
        && self.read_protected.binary_search(&(volume, id)).is_ok()
    }

    pub(in crate::toolbox::files) fn holds_notices(&self, volume: u32) -> bool
    {
        self.mutating && self.uses(volume)
    }

    pub(in crate::toolbox::files) fn permits(&self, handle: u32, owner: u64) -> bool
    {
        !self.mutating || internal_owner(handle) == owner
    }

    pub(in crate::toolbox::files) fn abandon(&mut self, owner: u64)
    {
        if self.owner == owner
        {
            self.cancel = true;
            self.abandoned = true;
        }
    }
}

fn reserve(owner: u64, call: &mut FileTransferCall, sources: &[FileTransferSource],
           bridge: Option<bridge::Bridge>)
           -> Result<(), Status>
{
    let query = bridge.is_some_and(|key| matches!(key.operation, EMPTY_TRASH | TRASH_USAGE));
    if call.handle != 0
       || sources.is_empty()
       || sources.len() > MAX_TRANSFER_SOURCES
       || call.mode > TRANSFER_TRASH
       || (call.mode == TRANSFER_TRASH
           && (call.destination_volume != 0 || call.destination_parent != 0))
       || (!query
           && call.mode != TRANSFER_TRASH
           && (call.destination_volume == 0 || call.destination_parent < ROOT_ID))
       || sources.iter().any(|s| {
                            s.volume == 0
                            || s.cnid < ROOT_ID
                            || s.cnid == ROOT_ID && !query
                            || s.expected_parent < ROOT_ID
                            || s.placement > 2
                            || s.reserved != 0
                            || s.placement == 1
                               && s.point
                                  == (Point { h: -1,
                                              v: -1 })
                        })
    {
        return Err(BAD_ARGUMENT);
    }
    let mut state = Box::try_new(State::new(call, sources)?).map_err(|_| NO_MEMORY)?;
    state.put_away = bridge.is_some_and(|key| key.operation == PUT_AWAY);
    if let Some(key) = bridge.filter(|_| query)
    {
        state.mode =
            if key.operation == TRASH_USAGE { Mode::CountTrash } else { Mode::DeleteTrash };
        state.trash_volume = key.volume;
        state.stage = Stage::SourceVolume;
    }
    let mut volumes = Vec::new();
    volumes.try_reserve_exact(sources.len() + 1).map_err(|_| NO_MEMORY)?;
    volumes.extend(sources.iter().map(|s| s.volume));
    if call.mode != TRANSFER_TRASH
    {
        volumes.push(call.destination_volume);
    }
    volumes.sort_unstable();
    volumes.dedup();
    let handle = files(|m| {
        for &volume in &volumes
        {
            m.volumes.get(volume).and_then(super::super::super::Mounted::info).ok_or(NOT_FOUND)?;
            if m.transfers.iter().any(|(_, entry)| entry.uses(volume))
               || m.writes.iter().any(|(_, mark)| mark.volume == volume)
               || m.streams.iter().any(|(_, mark)| mark.volume == volume)
               || m.groups.iter().any(|(_, mark)| mark.volume == volume)
            {
                return Err(SERVICE_BUSY);
            }
        }
        m.transfers.reserve().map_err(|_| NO_MEMORY)?;
        let entry = Entry { read_protected: Vec::new(),
                            mutating: bridge.is_none_or(|key| key.operation != TRASH_USAGE),
                            bridge,
                            owner,
                            volumes,
                            state: None,
                            snapshot: Snapshot::default(),
                            cancel: false,
                            abandoned: false,
                            notice_due: false,
                            notice_pending: false };
        let handle = m.transfers.insert(entry).map_err(|_| NO_MEMORY)?;
        state.handle = handle;
        // The same reservation owns the state before the public ID escapes.
        m.transfers.get_mut(handle).unwrap().state = Some(state);
        Ok(handle)
    })?;
    call.handle = handle;
    Snapshot::default().fill(call);
    ACTIVE.store(true, Ordering::Release);
    Ok(())
}

pub(crate) fn request(owner: u64, call: &mut FileTransferCall, sources: &[FileTransferSource])
                      -> Status
{
    if call.reserved != [0; 4]
    {
        return BAD_ARGUMENT;
    }
    if call.operation == BEGIN_TRANSFER
    {
        return reserve(owner, call, sources, None).err().unwrap_or(OK);
    }
    if !sources.is_empty() || call.operation > ABANDON_TRANSFER
    {
        return BAD_ARGUMENT;
    }
    files(|m| {
        let entry = m.transfers.get_mut(call.handle).ok_or(NOT_FOUND)?;
        if entry.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        if matches!(call.operation, CANCEL_TRANSFER | ABANDON_TRANSFER)
        {
            entry.cancel = true;
        }
        if call.operation == ABANDON_TRANSFER
        {
            entry.abandoned = true;
        }
        entry.notice_pending = false;
        entry.snapshot.fill(call);
        if entry.snapshot.terminal()
        {
            m.transfers.remove(call.handle);
        }
        // STEP is an observation, not a nested driver pump. The idle service
        // advances bounded work independently of app polling or mouse motion.
        Ok(())
    }).err()
      .unwrap_or(OK)
}

pub(super) fn pump()
{
    if !ACTIVE.load(Ordering::Acquire) || PUMPING.swap(true, Ordering::AcqRel)
    {
        return;
    }
    struct Pump;
    impl Drop for Pump
    {
        fn drop(&mut self)
        {
            PUMPING.store(false, Ordering::Release);
        }
    }
    let _pump = Pump;
    for slot in 0..Kind::FileTransfers.capacity()
    {
        let parked = PARKED.lock()[slot].take();
        let taken = if let Some(state) = parked
        {
            Ok(Some((state.handle, state)))
        }
        else
        {
            files(|m| {
                let Some(handle) = m.transfers.iter().nth(slot).map(|(id, _)| id)
                else
                {
                    return Ok(None);
                };
                let entry = m.transfers.get_mut(handle).ok_or(NOT_FOUND)?;
                if entry.snapshot.terminal() && !entry.notice_due
                {
                    if entry.abandoned
                    {
                        m.transfers.remove(handle);
                    }
                    return Ok(None);
                }
                let Some(mut state) = entry.state.take()
                else
                {
                    return Ok(None);
                };
                state.cancelling |= entry.cancel && !state.committed;
                Ok(Some((handle, state)))
            })
        };
        let Ok(Some((handle, mut state))) = taken
        else
        {
            continue;
        };
        state.advance();
        // A temporary guard failure must not drop an active transaction. The
        // closure borrows the owner; it moves it only after locating its mark.
        let mut owned = Some(state);
        let restored = files(|m| {
            let entry = m.transfers.get_mut(handle).ok_or(NOT_FOUND)?;
            let mut state = owned.take().unwrap();
            if state.stage == Stage::CommitBarrier
            {
                state.cancelling |= entry.cancel;
                if !state.cancelling
                {
                    state.committed = true;
                    state.stats.committed = true;
                    state.cursor = state.nodes.len();
                    state.stage = Stage::FinalizeProperties;
                }
            }
            let finished = state.terminal() && !entry.snapshot.terminal();
            // An unreachable destination must not freeze an untouched startup
            // volume (preferences, documents and unrelated future drags). Keep
            // quarantine leases only where a mutation may have occurred.
            let before = entry.volumes.len();
            if state.undo_complete
            {
                entry.volumes.clear();
            }
            else if state.stats.cleanup_error != OK
            {
                entry.volumes.retain(|volume| state.mutated.binary_search(volume).is_ok());
            }
            let released = entry.volumes.len() != before;
            entry.notice_due |= state.stats.changed(entry.snapshot);
            entry.snapshot = state.stats;
            let waiter = if finished { entry.bridge.map(|bridge| bridge.thread) } else { None };
            if entry.bridge.is_some() && state.terminal()
            {
                entry.notice_due = false;
            }
            if state.terminal() && entry.abandoned
            {
                m.transfers.remove(handle);
                if finished
                {
                    m.release_transfer_notices();
                }
                return Ok((None, waiter));
            }
            entry.state = Some(state);
            let notice = if entry.notice_due
                            && !entry.notice_pending
                            && !entry.abandoned
                            && entry.bridge.is_none()
            {
                entry.notice_due = false;
                entry.notice_pending = true;
                Some(entry.owner)
            }
            else
            {
                None
            };
            if finished || released
            {
                m.release_transfer_notices();
            }
            Ok((notice, waiter))
        });
        if let Ok((_, Some(thread))) = &restored
        {
            crate::machine::scheduler::wake_io(*thread, handle);
        }
        if let Some(state) = owned
        {
            PARKED.lock()[slot] = Some(state);
        }
        if let Ok((Some(owner), _)) = restored
        {
            let delivered = crate::kernel::with_desktop(|desktop| {
                                desktop.notify_file_transfer(owner, handle)
                            }).map_err(crate::state::AccessError::status)
                              .and_then(|r| r);
            if let Err(error) = delivered
            {
                let _ = files(|m| {
                    if let Some(entry) = m.transfers.get_mut(handle)
                    {
                        entry.notice_pending = false;
                        entry.notice_due = true;
                        if error == NOT_FOUND
                        {
                            entry.abandoned = true;
                            entry.cancel = true;
                        }
                    }
                    Ok(())
                });
            }
        }
    }
    // Check/store under the same guard as Begin, avoiding a lost wake.
    let _ = files(|m| {
        if m.transfers
            .iter()
            .all(|(_, entry)| entry.snapshot.terminal() && !entry.notice_due && !entry.abandoned)
        {
            ACTIVE.store(false, Ordering::Release);
        }
        Ok(())
    });
}

pub(in crate::toolbox::files) fn abandon(manager: &mut super::super::Manager, owner: u64)
{
    for entry in manager.transfers.values_mut()
    {
        entry.abandon(owner);
    }
    ACTIVE.store(true, Ordering::Release);
}
