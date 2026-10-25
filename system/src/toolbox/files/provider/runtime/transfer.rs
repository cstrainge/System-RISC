//! Resident ownership of recursive transfers. Requests only borrow caller
//! memory during admission. Every subsequent I/O and undo receipt is owned.
#![forbid(unsafe_code)]
use alloc::sync::Arc;

use super::*;
use crate::toolbox::handles::OwnerRef;
pub(crate) mod lifetime;
mod model;
mod plan;
mod engine;
mod replies;
mod publication;
pub(in crate::toolbox::files) mod bridge;
use model::*;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static PUMPING: AtomicBool = AtomicBool::new(false);
// One preflighted continuation slot per registered transfer; slot indices
// are bookkeeping only and never escape as object identities.
struct Parked
{
    handle: u64,
    state:  Option<Box<State>>
}
static PARKED: Spinlock<Vec<Arc<Spinlock<Parked>>>> = Spinlock::new(Vec::new());
pub(in crate::toolbox::files) struct Entry
{
    continuation: Arc<Spinlock<Parked>>,
    bridge:       Option<bridge::Bridge>,
    mutating:     bool,
    owner:        u64,
    authority:    OwnerRef,
    scope:        super::super::super::subject::Lease,
    record:       Spinlock<Record>
}
impl Drop for Entry
{
    fn drop(&mut self)
    {
        let removed = {
            let mut parked = PARKED.lock();
            parked.iter()
                  .position(|row| Arc::ptr_eq(row, &self.continuation))
                  .map(|at| parked.swap_remove(at))
        };
        drop(removed);
    }
}
struct Record
{
    closed:         bool,
    read_protected: Vec<(u32, u32)>,
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
        let record = self.record.lock();
        !record.closed && record.volumes.contains(&volume)
    }

    pub(in crate::toolbox::files) fn protects_read(&self, _handle: u64,
                                                   owner: crate::toolbox::files::subject::Access<'_>,
                                                   volume: u32, id: u32)
                                                   -> bool
    {
        let record = self.record.lock();
        !record.closed
        && record.volumes.contains(&volume)
        && !matches!(owner,crate::toolbox::files::subject::Access::Operation(scope) if self.scope.same_scope(scope))
        && record.read_protected.binary_search(&(volume, id)).is_ok()
    }

    pub(in crate::toolbox::files) fn holds_notices(&self, volume: u32) -> bool
    {
        self.mutating && self.uses(volume)
    }

    pub(in crate::toolbox::files) fn permits(&self, _handle: u64,
                                             owner: crate::toolbox::files::subject::Access<'_>)
                                             -> bool
    {
        !self.mutating
        || matches!(owner,crate::toolbox::files::subject::Access::Operation(scope) if self.scope.same_scope(scope))
    }
}
impl crate::toolbox::files::objects::Payload for Entry
{
    type Storage = crate::toolbox::files::objects::storage::Native<Self>;

    const KIND: crate::toolbox::handles::KindCode = crate::toolbox::handles::KindCode(*b"FTXN");

    fn cleanup_pending(&self) -> bool
    {
        !self.record.lock().closed
    }

    fn begin_cleanup(&self)
    {
        let mut record = self.record.lock();
        record.cancel = true;
        record.abandoned = true;
        ACTIVE.store(true, Ordering::Release);
    }

    fn access_allowed(&self, access: crate::toolbox::files::subject::Access<'_>) -> bool
    {
        matches!(access,crate::toolbox::files::subject::Access::Client(owner) if self.owner==owner)
    }

    fn owner_stopping(&self) -> bool
    {
        !self.authority.accepting()
    }
}
impl lifetime::TransferHost for crate::toolbox::files::Manager
{
    fn release_file_transfer(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                             storage: bool)
                             -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        self.transfers.release(ticket, storage)
    }
}

fn reserve(owner: u64, call: &mut FileTransferCall, sources: &[FileTransferSource],
           bridge: Option<bridge::Bridge>)
           -> Result<(), Status>
{
    let authority = super::super::super::creation_authority(owner)?;
    let kinds = crate::toolbox::handles::file_transfer_kinds()?;
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
    let reservation=mac_object_table::scope::Reservation::try_new(&authority,crate::toolbox::handles::KindCode(*b"FTXN"))
        .map_err(|error|crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error)))?;
    let scope = reservation.lease();
    let mut state = Box::try_new(State::new(call, sources, scope.clone())?).map_err(|_| NO_MEMORY)?;
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
    let continuation = Arc::try_new(Spinlock::new(Parked { handle: 0,
                                                           state:  None })).map_err(|_| NO_MEMORY)?;
    let handle = files(|m| {
        m.transfers.prepare(kinds)?;
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
        PARKED.lock().try_reserve(1).map_err(|_| NO_MEMORY)?;
        let handle =
            m.transfers.insert_root(reservation, |handle| {
                            state.handle = handle;
                            continuation.lock().handle = handle;
                            PARKED.lock().push(continuation.clone());
                            Entry { continuation,
                                    mutating: bridge.is_none_or(|key| {
                                                        key.operation != TRASH_USAGE
                                                    }),
                                    bridge,
                                    owner,
                                    authority: authority.clone(),
                                    scope: scope.clone(),
                                    record: Spinlock::new(Record { closed: false,
                                                                   read_protected: Vec::new(),
                                                                   volumes,
                                                                   state: Some(state),
                                                                   snapshot:
                                                                       Snapshot::default(),
                                                                   cancel: false,
                                                                   abandoned: false,
                                                                   notice_due: false,
                                                                   notice_pending: false }) }
                        })?;

        Ok(handle)
    })?;
    call.handle = handle;
    Snapshot::default().fill(call);
    ACTIVE.store(true, Ordering::Release);
    Ok(())
}

fn retire(manager: &mut crate::toolbox::files::Manager, handle: u64) -> Result<(), Status>
{
    let entry = manager.transfers.completing(handle).ok_or(NOT_FOUND)?;
    if !entry.record.lock().closed
    {
        return Err(SERVICE_BUSY);
    }
    manager.transfers.remove(handle).ok_or(NOT_FOUND)
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
        let mut record = entry.record.lock();
        if matches!(call.operation, CANCEL_TRANSFER | ABANDON_TRANSFER)
        {
            record.cancel = true;
        }
        record.notice_pending = false;
        record.snapshot.fill(call);
        let closed = record.closed;
        drop(record);
        if call.operation == ABANDON_TRANSFER
        {
            m.transfers.abandon(call.handle)?;
        }
        else if closed
        {
            m.transfers.remove(call.handle).ok_or(NOT_FOUND)?;
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
    let mut cursor = 0;
    loop
    {
        let continuation = {
            let parked = PARKED.lock();
            let Some(row) = parked.get(cursor)
            else
            {
                break;
            };
            cursor += 1;
            row.clone()
        };
        let (handle, parked) = {
            let mut row = continuation.lock();
            (row.handle, row.state.take())
        };
        let taken = if let Some(state) = parked
        {
            Ok(Some((handle, state)))
        }
        else
        {
            files(|m| {
                let entry = m.transfers.completing(handle).ok_or(NOT_FOUND)?;
                let mut record = entry.record.lock();
                if record.closed && !record.notice_due
                {
                    let abandoned = record.abandoned;
                    drop(record);
                    if abandoned
                    {
                        m.transfers.remove(handle).ok_or(NOT_FOUND)?;
                    }
                    return Ok(None);
                }
                let Some(mut state) = record.state.take()
                else
                {
                    return Ok(None);
                };
                state.cancelling |= record.cancel && !state.committed;
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
            let entry = m.transfers.completing(handle).ok_or(NOT_FOUND)?;
            let mut record = entry.record.lock();
            let mut state = owned.take().ok_or(BAD_ABI)?;
            if state.stage == Stage::CommitBarrier
            {
                state.cancelling |= record.cancel;
                if !state.cancelling
                {
                    state.committed = true;
                    state.stats.committed = true;
                    state.cursor = state.nodes.len();
                    state.stage = Stage::FinalizeProperties;
                }
            }
            let finished = state.terminal() && !record.closed;
            record.closed = state.terminal();
            // An unreachable destination must not freeze an untouched startup
            // volume (preferences, documents and unrelated future drags). Keep
            // quarantine leases only where a mutation may have occurred.
            let before = record.volumes.len();
            if state.undo_complete
            {
                record.volumes.clear();
            }
            else if state.stats.cleanup_error != OK
            {
                record.volumes.retain(|volume| state.mutated.binary_search(volume).is_ok());
            }
            let released = record.volumes.len() != before;
            record.notice_due |= state.stats.changed(record.snapshot);
            record.snapshot = state.stats;
            let waiter = if finished { entry.bridge.map(|bridge| bridge.thread) } else { None };
            if entry.bridge.is_some() && state.terminal()
            {
                record.notice_due = false;
            }
            if state.terminal() && record.abandoned
            {
                drop(record);
                m.transfers.remove(handle).ok_or(NOT_FOUND)?;
                if finished
                {
                    m.release_transfer_notices();
                }
                return Ok((None, waiter));
            }
            record.state = Some(state);
            let notice = if record.notice_due
                            && !record.notice_pending
                            && !record.abandoned
                            && entry.bridge.is_none()
            {
                record.notice_due = false;
                record.notice_pending = true;
                Some(entry.owner)
            }
            else
            {
                None
            };
            drop(record);
            if finished || released
            {
                m.release_transfer_notices();
            }
            Ok((notice, waiter))
        });
        if let Ok((_, Some(thread))) = &restored
        {
            crate::machine::scheduler::wake_io(*thread,
                                               crate::context::DeferredIo::FileTransfer(handle));
        }
        if let Some(state) = owned
        {
            continuation.lock().state = Some(state);
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
                    if let Some(entry) = m.transfers.completing(handle)
                    {
                        let mut record = entry.record.lock();
                        record.notice_pending = false;
                        record.notice_due = true;
                        if error == NOT_FOUND
                        {
                            record.abandoned = true;
                            record.cancel = true;
                        }
                    }
                    Ok(())
                });
            }
        }
    }
    // Check/store under the same guard as Begin, avoiding a lost wake.
    let _ = files(|m| {
        if m.transfers.iter().all(|(_, entry)| {
                                 let record = entry.record.lock();
                                 record.closed && !record.notice_due && !record.abandoned
                             })
        {
            ACTIVE.store(false, Ordering::Release);
        }
        Ok(())
    });
}
