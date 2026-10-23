//! Bounded atomic native catalog moves. No partial icon group is published.
#![forbid(unsafe_code)]
use super::*;
impl Manager
{
    /// Cheap admission for the atomic operation used by folder-target drags.
    /// A writable mount alone is not proof that its move path is implemented.
    /// Writable provider contracts include the stepped group apply/undo path.
    pub(crate) fn supports_icon_moves(&self, volume: u32) -> Result<bool, Status>
    {
        let mounted = self.volumes.get(volume).ok_or(NOT_FOUND)?;
        mounted.info().ok_or(NOT_FOUND)?;
        Ok(match mounted
        {
            Mounted::Block(block) => block.info.read_only == 0 && !block.poisoned,
            Mounted::Provider(provider) =>
            {
                provider.phase == provider::Phase::Live
                && provider.info.is_some_and(|info| info.read_only == 0)
            }
        })
    }

    pub(super) fn move_icons(&mut self, owner: u64, call: &mut IconMoveCall, rows: &[IconMove])
                             -> Result<(), Status>
    {
        if call.operation != MOVE_ICONS || rows.len() > 1024
        {
            return Err(BAD_ARGUMENT);
        }
        call.changed = 0;
        self.validate_transfer_mutation(owner, call.volume)?;
        self.validate_provider_mutation(call.volume)?;
        let mut protected = Vec::new();
        protected.try_reserve(MAX_OPEN_FILES + self.writes.len() + self.streams.len())
                 .map_err(|_| NO_MEMORY)?;
        for (_, file) in self.handles.iter().filter(|(_, f)| f.volume == call.volume)
        {
            protected.push(file.id);
        }
        for (_, file) in
            self.writes.iter().filter(|(_, f)| f.volume == call.volume && f.expected_cnid != 0)
        {
            protected.push(file.expected_cnid);
        }
        protected.extend(self.streams
                             .iter()
                             .filter(|(_, mark)| mark.volume == call.volume)
                             .map(|(_, mark)| mark.expected)
                             .filter(|id| *id != 0));
        let mounted = self.mounted(call.volume)?;
        if mounted.poisoned
        {
            return Err(ROLLBACK_FAILED);
        }
        if mounted.info.read_only != 0
        {
            return Err(READ_ONLY);
        }
        let mut volume = io(Volume::mount(&mut mounted.device, mounted.partition))?;
        let mut entries = Vec::new();
        let mut affected = Vec::new();
        entries.try_reserve_exact(rows.len()).map_err(|_| NO_MEMORY)?;
        affected.try_reserve_exact(rows.len() * 2).map_err(|_| NO_MEMORY)?;
        let mut ids = Vec::new();
        ids.try_reserve_exact(rows.len()).map_err(|_| NO_MEMORY)?;
        ids.extend(rows.iter().map(|r| r.cnid));
        ids.sort_unstable();
        if ids.windows(2).any(|p| p[0] == p[1])
        {
            return Err(BAD_ARGUMENT);
        }
        for row in rows
        {
            if row.cnid <= ROOT_ID
               || row.parent < ROOT_ID
               || row.expected_parent < ROOT_ID
               || row.placement & !7 != 0
               || row.placement & 3 > 2
               || row.placement & 3 == 1
                  && row.point
                     == (Point { h: -1,
                                 v: -1 })
            {
                return Err(BAD_ARGUMENT);
            }
            let before = volume.entry(row.cnid).map_err(|_| IO_ERROR)?.ok_or(NOT_FOUND)?;
            if before.parent != row.expected_parent
            {
                return Err(SCAN_STALE);
            }
            let mut info = before.finder_info;
            if row.placement & 3 != 0
            {
                let flags = u16::from_be_bytes(info[8..10].try_into().unwrap());
                let (flags, point) = if row.placement & 3 == 1
                {
                    (flags | 0x100, row.point)
                }
                else
                {
                    (flags & !0x100,
                     Point { h: -1,
                             v: -1 })
                };
                info[8..10].copy_from_slice(&flags.to_be_bytes());
                info[10..12].copy_from_slice(&point.v.to_be_bytes());
                info[12..14].copy_from_slice(&point.h.to_be_bytes());
            }
            let trash = row.placement & 4 != 0 && before.parent != row.parent;
            if trash
            {
                let target = volume.entry(row.parent).map_err(|_| IO_ERROR)?.ok_or(NOT_FOUND)?;
                if !target.directory
                   || target.parent != ROOT_ID
                   || !target.name.eq_ignore_ascii_case(b"Trash")
                {
                    return Err(BAD_ARGUMENT);
                }
                info[28..32].copy_from_slice(&before.parent.to_be_bytes());
            }
            entries.push((row.cnid, row.parent, info, trash));
            affected.extend_from_slice(&[before.parent, row.parent]);
        }
        // Reserve a revision before publication; failed/no-op groups don't
        // advertise a nonexistent change to scans or directory watches.
        let revision = scan::next_revision()?;
        let result = volume.move_entries(&entries, call.when, &protected);
        let changed = match result
        {
            Ok(changed) => changed,
            Err(error) =>
            {
                if error == MutationError::RollbackFailed
                {
                    mounted.poisoned = true;
                    mounted.info.read_only = 1;
                }
                return Err(super::write::status(error));
            }
        };
        if !changed
        {
            return Ok(());
        }
        mounted.catalog_revision = revision;
        mounted.scanner = volume.catalog_scanner().ok();
        mounted.info.modified = volume.modified;
        mounted.info.free_bytes = volume.free_bytes;
        affected.sort_unstable();
        affected.dedup();
        for parent in affected
        {
            self.notify_directory(call.volume, parent)?;
        }
        call.changed = 1;
        Ok(())
    }
}
