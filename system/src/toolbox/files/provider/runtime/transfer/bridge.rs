//! Blocking single-item compatibility calls park their thread on the resident
//! transfer owner. They never pump events or retain an application pointer.
use super::*;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Bridge
{
    pub thread:    u64,
    pub operation: u32,
    pub volume:    u32,
    pub cnid:      u32,
    pub when:      u64
}
pub(crate) fn pending(thread: u64, handle: u64) -> Result<bool, Status>
{
    files(|m| {
        let entry = m.transfers.completing(handle).ok_or(NOT_FOUND)?;
        if entry.bridge.is_none_or(|bridge| bridge.thread != thread)
        {
            return Err(ACCESS_DENIED);
        }
        Ok(!entry.record.lock().closed)
    })
}
pub(crate) fn abandon_waiter(thread: u64, handle: u64)
{
    let _ = files(|m| {
        let matching =
            m.transfers
             .completing(handle)
             .is_some_and(|entry| entry.bridge.is_some_and(|bridge| bridge.thread == thread));
        if matching
        {
            if m.transfers.get(handle).is_some()
            {
                m.transfers.abandon(handle)?;
            }
            else if let Some(entry) = m.transfers.completing(handle)
            {
                crate::toolbox::files::objects::Payload::begin_cleanup(entry);
            }
        }
        Ok(())
    });
}
fn defer(handle: u64) -> Status
{
    crate::context::current_thread_context().traps
                                            .defer(crate::context::DeferredIo::FileTransfer(handle))
                                            .err()
                                            .unwrap_or(SERVICE_BUSY)
}
use crate::toolbox::files::application::catalog;
pub(crate) fn request(owner: u64, call: &mut FileCall) -> Status
{
    let key = Bridge { thread:    crate::context::current_thread().0,
                       operation: call.operation,
                       volume:    call.volume,
                       cnid:      call.cnid,
                       when:      call.position };
    if key.thread <= 1
       || !matches!(key.operation, MOVE_TO_TRASH | PUT_AWAY | EMPTY_TRASH | TRASH_USAGE)
    {
        return WRONG_THREAD;
    }
    let existing = files(|m| {
        if let Some((handle, entry)) =
            m.transfers
             .iter()
             .find(|(_, entry)| entry.bridge.is_some_and(|bridge| bridge.thread == key.thread))
        {
            let record = entry.record.lock();
            if entry.owner != owner || entry.bridge != Some(key) || record.abandoned
            {
                return Err(BAD_ABI);
            }
            Ok(Some((handle, record.snapshot)))
        }
        else
        {
            Ok(None)
        }
    });
    let existing = match existing
    {
        Ok(value) => value,
        Err(error) => return error
    };
    if let Some((handle, snapshot)) = existing
    {
        if !snapshot.terminal()
        {
            return defer(handle);
        }
        if snapshot.phase != TRANSFER_COMMITTED
        {
            let _ = files(|m| {
                retire(m, handle)?;
                Ok(())
            });
            return if snapshot.phase == TRANSFER_CANCELLED
            {
                OPERATION_CANCELLED
            }
            else if snapshot.cleanup_error != OK
            {
                ROLLBACK_FAILED
            }
            else
            {
                snapshot.error
            };
        }
        if matches!(key.operation, EMPTY_TRASH | TRASH_USAGE)
        {
            return match files(|m| {
                      retire(m, handle)?;
                      Ok(())
                  })
            {
                Ok(()) =>
                {
                    call.index = snapshot.total_items;
                    call.length = snapshot.allocated_bytes;
                    OK
                }
                Err(error) => error
            };
        }
        return match catalog(owner, call.volume, call.cnid)
        {
            Ok(entry) =>
            {
                let retired = files(|m| {
                    retire(m, handle)?;
                    Ok(())
                });
                match retired
                {
                    Ok(()) =>
                    {
                        call.entry = entry;
                        OK
                    }
                    Err(error) => error
                }
            }
            Err(error) => error
        };
    }
    if matches!(key.operation, EMPTY_TRASH | TRASH_USAGE)
    {
        let mut transfer = FileTransferCall { operation: BEGIN_TRANSFER,
                                              mode: TRANSFER_COPY,
                                              destination_volume: key.volume,
                                              when: key.when,
                                              ..Default::default() };
        let source = FileTransferSource { volume: key.volume,
                                          cnid: ROOT_ID,
                                          expected_parent: ROOT_ID,
                                          ..Default::default() };
        return match reserve(owner, &mut transfer, &[source], Some(key))
        {
            Ok(()) => defer(transfer.handle),
            Err(error) => error
        };
    }
    let source = match catalog(owner, call.volume, call.cnid)
    {
        Ok(entry) => entry,
        Err(error) => return error
    };
    if source.id <= ROOT_ID
    {
        return ACCESS_DENIED;
    }
    let parent = if key.operation == PUT_AWAY
    {
        let id = u32::from_be_bytes(source.finder_info[28..32].try_into().unwrap());
        if id < ROOT_ID || id == source.parent
        {
            return NOT_FOUND;
        }
        id
    }
    else
    {
        0
    };
    let mut transfer = FileTransferCall { operation: BEGIN_TRANSFER,
                                          mode: if key.operation == MOVE_TO_TRASH
                                          {
                                              TRANSFER_TRASH
                                          }
                                          else
                                          {
                                              TRANSFER_MOVE
                                          },
                                          destination_volume: if key.operation == PUT_AWAY
                                          {
                                              source.volume
                                          }
                                          else
                                          {
                                              0
                                          },
                                          destination_parent: parent,
                                          when: key.when,
                                          ..Default::default() };
    let source = FileTransferSource { volume: source.volume,
                                      cnid: source.id,
                                      expected_parent: source.parent,
                                      placement: 2,
                                      ..Default::default() };
    match reserve(owner, &mut transfer, &[source], Some(key))
    {
        Ok(()) => defer(transfer.handle),
        Err(error) => error
    }
}
