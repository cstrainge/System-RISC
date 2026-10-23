//! Generation-checked ownership for same-volume group transactions. The native
//! block path commits atomically; a provider group advances one bounded row and
//! retains its owner through reverse-order rollback acknowledgement.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicBool, Ordering};

use mac_filesystem_interface::{IconMovePhase as Phase, IconMoveProgress as Progress};

use super::*;
pub(super) struct Group
{
    pub owner:       u64,
    pub volume:      u32,
    pub when:        u64,
    pub revision:    u64,
    pub rows:        Vec<IconMove>,
    pub parents:     Vec<u32>,
    pub provider:    Option<u32>,
    pub running:     bool,
    pub abandoned:   bool,
    pub quarantined: bool,
    pub published:   bool,
    pub progress:    Progress
}
static CLEANUP: AtomicBool = AtomicBool::new(false);
pub(super) fn request_cleanup()
{
    CLEANUP.store(true, Ordering::Release);
}
pub(super) fn needs_cleanup() -> bool
{
    CLEANUP.load(Ordering::Acquire)
}
pub(super) fn idle_cleanup()
{
    CLEANUP.store(false, Ordering::Release);
}
impl Manager
{
    pub(super) fn group_provider(&self, owner: u64, handle: u32) -> Result<Option<u32>, Status>
    {
        let group = self.groups.get(handle).ok_or(NOT_FOUND)?;
        if group.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        Ok(group.provider)
    }

    pub(super) fn reserve_group(&mut self, owner: u64, call: &mut IconGroupCall, rows: &[IconMove])
                                -> Result<(), Status>
    {
        if call.operation != BEGIN_ICON_GROUP
           || call.handle != 0
           || call.reserved != 0
           || rows.len() > 1024
        {
            return Err(BAD_ARGUMENT);
        }
        self.validate_transfer_mutation(owner, call.volume)?;
        self.validate_provider_mutation(call.volume)?;
        let mounted = self.volumes.get(call.volume).ok_or(NOT_FOUND)?;
        if mounted.info().ok_or(NOT_FOUND)?.read_only != 0
        {
            return Err(READ_ONLY);
        }
        if self.groups.iter().any(|(_, group)| group.volume == call.volume)
           || self.streams.iter().any(|(_, stream)| stream.volume == call.volume)
           || self.writes.iter().any(|(_, write)| write.volume == call.volume)
        {
            return Err(SERVICE_BUSY);
        }
        let provider = self.read_provider(call.volume)?;
        let mut owned = Vec::new();
        owned.try_reserve_exact(rows.len()).map_err(|_| NO_MEMORY)?;
        let mut parents = Vec::new();
        parents.try_reserve_exact(rows.len() * 2).map_err(|_| NO_MEMORY)?;
        let mut identities = Vec::new();
        identities.try_reserve_exact(rows.len()).map_err(|_| NO_MEMORY)?;
        for row in rows
        {
            mac_filesystem_interface::IconMove { id:              row.cnid,
                                                 expected_parent: row.expected_parent,
                                                 parent:          row.parent,
                                                 placement:       row.placement,
                                                 horizontal:      row.point.h,
                                                 vertical:        row.point.v }.validate()
                                                                               .map_err(|_| {
                                                                                   BAD_ARGUMENT
                                                                               })?;
            identities.push(row.cnid);
            parents.extend_from_slice(&[row.expected_parent, row.parent]);
            owned.push(*row);
        }
        identities.sort_unstable();
        if identities.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(BAD_ARGUMENT);
        }
        parents.sort_unstable();
        parents.dedup();
        let revision = scan::next_revision()?;
        let progress = Progress { phase:     Phase::Applying,
                                  completed: 0,
                                  total:     rows.len() as u32,
                                  changed:   false };
        call.handle = self.groups
                          .insert(Group { owner,
                                          volume: call.volume,
                                          when: call.when,
                                          revision,
                                          rows: owned,
                                          parents,
                                          provider,
                                          running: false,
                                          abandoned: false,
                                          quarantined: false,
                                          published: false,
                                          progress })
                          .map_err(|_| NO_MEMORY)?;
        fill(call, progress);
        Ok(())
    }

    pub(super) fn group_request(&mut self, owner: u64, call: &mut IconGroupCall, rows: &[IconMove])
                                -> Result<(), Status>
    {
        if call.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        if call.operation == BEGIN_ICON_GROUP
        {
            // The asynchronous provider route takes ownership outside this
            // guard; do not admit a mark through the synchronous block adapter.
            if self.read_provider(call.volume)?.is_some()
            {
                return Err(NOT_SUPPORTED);
            }
            return self.reserve_group(owner, call, rows);
        }
        let group = self.groups.get(call.handle).ok_or(NOT_FOUND)?;
        if group.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        if group.quarantined
        {
            return Err(ROLLBACK_FAILED);
        }
        if call.operation == ABANDON_ICON_GROUP
        {
            if group.provider.is_none()
            {
                self.groups.remove(call.handle);
            }
            else
            {
                self.groups.get_mut(call.handle).ok_or(NOT_FOUND)?.abandoned = true;
                request_cleanup();
            }
            return Ok(());
        }
        if group.running
        {
            return Err(SERVICE_BUSY);
        }
        if group.provider.is_some()
        {
            return Err(NOT_SUPPORTED);
        }
        let total = group.progress.total;
        let mut progress = Progress { phase: Phase::Aborted,
                                      completed: 0,
                                      total,
                                      changed: false };
        match call.operation
        {
            CANCEL_ICON_GROUP => (),
            STEP_ICON_GROUP =>
            {
                // No side effect occurs before this step; native move_entries
                // already owns its complete atomic publication/rollback.
                let mut move_call = IconMoveCall { operation: MOVE_ICONS,
                                                   volume: group.volume,
                                                   when: group.when,
                                                   ..Default::default() };
                let rows =
                    core::mem::take(&mut self.groups.get_mut(call.handle).ok_or(NOT_FOUND)?.rows);
                let result = self.move_icons(owner, &mut move_call, &rows);
                if let Err(error) = result
                {
                    if error == ROLLBACK_FAILED
                    {
                        let group = self.groups.get_mut(call.handle).ok_or(NOT_FOUND)?;
                        group.quarantined = true;
                        group.rows = rows;
                    }
                    else
                    {
                        self.groups.remove(call.handle);
                    }
                    return Err(error);
                }
                progress.phase = Phase::Committed;
                progress.completed = total;
                progress.changed = move_call.changed != 0;
            }
            _ => return Err(BAD_ARGUMENT)
        }
        self.groups.remove(call.handle).ok_or(NOT_FOUND)?;
        fill(call, progress);
        Ok(())
    }

    pub(super) fn ready_group(&mut self, handle: u32, progress: Progress) -> Result<(), Status>
    {
        let group = self.groups.get_mut(handle).ok_or(NOT_FOUND)?;
        if group.provider.is_none()
           || progress.total != group.progress.total
           || (group.progress.phase == Phase::Undoing && progress.phase == Phase::Applying)
           || (progress.phase == Phase::Applying && progress.completed < group.progress.completed)
        {
            return Err(BAD_ABI);
        }
        group.running = false;
        group.progress = progress;
        let volume = group.volume;
        let revision = group.revision;
        let publish = progress.phase == Phase::Committed && progress.changed && !group.published;
        if publish
        {
            group.published = true;
            let mounted = self.provider_mount(volume)?;
            mounted.revision = revision;
            mounted.changes = true;
            self.notify_group_at(volume, handle, revision);
        }
        Ok(())
    }

    pub(super) fn quarantine_group(&mut self, handle: u32)
    {
        let Some(group) = self.groups.get_mut(handle)
        else
        {
            return;
        };
        group.running = false;
        group.quarantined = true;
        let volume = group.volume;
        if let Some(Mounted::Provider(mounted)) = self.volumes.get_mut(volume)
        {
            mounted.phase = provider::Phase::Quarantined;
            if let Some(info) = &mut mounted.info
            {
                info.read_only = 1;
            }
        }
    }
}
pub(super) fn fill(call: &mut IconGroupCall, progress: Progress)
{
    call.phase = progress.phase as u32;
    call.completed = progress.completed;
    call.count = progress.total;
    call.changed = u32::from(progress.changed);
}
