//! `System Folder/Pipes` as a File Manager folder. The resident pipe registry
//! is enumerated directly, so a named pipe is a real item with a stable
//! synthetic CNID without a sub-volume, provider mount or driver boundary
//! around state that never leaves the System.
//!
//! The folder itself is an ordinary HFS directory; only its *contents* are
//! virtual. Anonymous pipes have no registry row, so they are absent
//! structurally rather than by a filter here.
#![forbid(unsafe_code)]
use mac_abi::files::*;
use mac_filesystem::CatalogEntry;
pub(in crate::toolbox::files) use resident::virtual_entry;

mod resident
{
    use super::*;
    use crate::toolbox::{files::{BAD_ARGUMENT, MAX_VOLUMES, Manager, Mounted, NOT_FOUND, Status,
                                 utf8_name},
                         pipes};

    /// Item name of the folder under the volume's blessed folder. The image
    /// builder creates it, so this is the same spelling that created it.
    const NAME: &str = mac_abi::files::PIPES_FOLDER_NAME;
    /// Reserved item type and creator for a named pipe (section 9.3). Nothing
    /// else in the system claims this pair.
    const PIPE_TYPE: [u8; 4] = *b"PIPE";

    /// A registry row as item metadata. A pipe has no fork, child count or
    /// Finder record. Its type is the reserved `PIPE`/`PIPE` pair from section
    /// 9.3, never a type derived from its name, so no extension association can
    /// claim it.
    pub(in crate::toolbox::files) fn info(volume: u32, parent: u32, row: &pipes::Row)
                                          -> CatalogInfo
    {
        let mut result = CatalogInfo { volume,
                                       id: row.cnid,
                                       parent,
                                       kind: FILE,
                                       file_type: PIPE_TYPE,
                                       creator: PIPE_TYPE,
                                       created: row.created,
                                       modified: row.created,
                                       ..Default::default() };
        result.name_length = utf8_name(row.name(), &mut result.name);
        result
    }

    /// A registry row as a catalog entry, for the callers whose contract is a
    /// `CatalogEntry`. Lookup is per path component, not per painted item. The
    /// type pair lives in Finder information so the shared `metadata` projection
    /// reports the same kind as the enumeration.
    pub(in crate::toolbox::files) fn entry(parent: u32, row: &pipes::Row) -> CatalogEntry
    {
        let mut finder_info = [0u8; 32];
        finder_info[..4].copy_from_slice(&PIPE_TYPE);
        finder_info[4..8].copy_from_slice(&PIPE_TYPE);
        CatalogEntry { id: row.cnid,
                       parent,
                       name: row.name().to_vec(),
                       directory: false,
                       flags: 0,
                       finder_info,
                       created: row.created,
                       modified: row.created,
                       backup: None,
                       child_count: 0,
                       data: None,
                       resources: None,
                       data_allocated: 0,
                       resource_allocated: 0 }
    }

    /// Resolve a name inside the folder without borrowing the File Manager.
    /// Path walking already holds the volume, so it can only reach the registry
    /// through a free function. `folder` is the caller's cached identity.
    pub(in crate::toolbox::files) fn virtual_entry(folder: u32, name: &[u8])
                                                   -> Option<CatalogEntry>
    {
        pipes::find(name).map(|row| entry(folder, &row))
    }

    impl Manager
    {
        /// The cached folder identity, when a volume has one.
        pub(in crate::toolbox::files) fn pipes_folder(&self) -> Option<(u32, u32)>
        {
            self.pipes_folder
        }

        /// Whether this volume and directory is the Pipes folder.
        fn pipes_owns(&self, volume: u32, directory: u32) -> bool
        {
            self.pipes_folder == Some((volume, directory))
        }

        /// Registry revision for an enumeration of the Pipes folder, or `None`
        /// for every ordinary directory. The continuation carries this value so
        /// a registration or destruction during the scan is reported as stale.
        pub(in crate::toolbox::files) fn pipes_scan_revision(&self, volume: u32, directory: u32)
                                                             -> Option<u32>
        {
            self.pipes_owns(volume, directory).then(pipes::revision)
        }

        /// One enumeration row of the Pipes folder, by offset in registration
        /// order. The caller has already exhausted the real catalog children.
        pub(in crate::toolbox::files) fn pipes_row(&self, volume: u32, directory: u32, index: u32)
                                                   -> Option<CatalogInfo>
        {
            if !self.pipes_owns(volume, directory)
            {
                return None;
            }
            pipes::row(index as usize).map(|row| info(volume, directory, &row))
        }

        /// Registry item reached by its synthetic CNID. The row must still
        /// exist: a destroyed pipe is not resolvable, and a reserved-range
        /// identity alone never fabricates an item.
        pub(in crate::toolbox::files) fn pipes_entry(&self, volume: u32, cnid: u32)
                                                     -> Option<CatalogEntry>
        {
            if !pipes::virtual_cnid(cnid)
            {
                return None;
            }
            let (owner, folder) = self.pipes_folder?;
            if owner != volume
            {
                return None;
            }
            pipes::row_of(cnid).map(|row| entry(folder, &row))
        }

        /// Mutations inside the folder have no catalog object to act on, and a
        /// Open a named pipe item, performing `connect` for the calling process.
        /// The File Manager owns the item namespace, so it validates the volume
        /// and the identity before the pipe facility takes a connection slot;
        /// the pipe facility still owns the writer limit and the handle. This is
        /// deliberately separate from `OPEN`: a pipe item has no fork, and the
        /// generic open must keep refusing it.
        pub(in crate::toolbox::files) fn pipes_open(&self, owner: u64,
                                                    call: &mut mac_abi::files::PipeOpenCall)
                                                    -> Result<(), Status>
        {
            let Some((volume, folder)) = self.pipes_folder
            else
            {
                return Err(NOT_FOUND);
            };
            if call.volume != volume
            {
                return Err(BAD_ARGUMENT);
            }
            // A destroyed pipe is not openable: the identity must still be live.
            let row = pipes::row_of(call.cnid).ok_or(NOT_FOUND)?;
            // The connection belongs to the requesting process, taken from the
            // trap's trusted caller identity rather than the request record.
            let owner = crate::toolbox::files::creation_authority(owner)?;
            call.writer = pipes::connect_item(&owner, row.cnid, true)?;
            call.folder = folder;
            Ok(())
        }

        /// virtual identity has no fork, name or Finder record to change. Both
        /// are refused before any volume is touched, so a File Manager delete
        /// can never reach the registry. A real catalog item whose CNID happens
        /// to fall in the reserved band is refused too: failing closed on that
        /// identity is the only safe direction.
        pub(in crate::toolbox::files) fn pipes_refuses(&self, call: &FileCall) -> bool
        {
            let Some((volume, folder)) = self.pipes_folder
            else
            {
                return false;
            };
            if call.volume != volume
            {
                return false;
            }
            if pipes::virtual_cnid(call.cnid)
            {
                return true;
            }
            match call.operation
            {
                NEW_FOLDER => (if call.cnid == 0 { ROOT_ID } else { call.cnid }) == folder,
                MOVE_INTO => call.index == folder,
                _ => false
            }
        }

        /// Record a folder this volume created under its blessed folder. The
        /// probe and a later boot-ensure step can both create it, so a
        /// boot-only cache would be stale.
        pub(in crate::toolbox::files) fn pipes_note(&mut self, call: &FileCall, name: &str)
        {
            if call.entry.kind != DIRECTORY || call.entry.id < ROOT_ID || name != NAME
            {
                return;
            }
            let parent = if call.cnid == 0 { ROOT_ID } else { call.cnid };
            let blessed = self.volumes.get(call.volume).and_then(Mounted::info);
            if blessed.is_some_and(|volume| volume.blessed_folder == parent)
            {
                self.pipes_folder = Some((call.volume, call.entry.id));
            }
        }

        /// Re-resolve the cached identity after a mutation on the folder        /// itself. The
        /// folder is a real directory, so renaming, moving or deleting it is a legitimate
        /// catalog change that must not leave a stale identity pointing at nothing.
        pub(in crate::toolbox::files) fn pipes_refresh(&mut self, volume: u32, cnid: u32)
        {
            if self.pipes_folder == Some((volume, cnid))
            {
                self.pipes_folder = self.pipes_discover();
            }
        }

        /// Resolve the folder from the blessed folder of every mounted volume.
        /// A missing folder is not an error: it appears when created.
        pub(in crate::toolbox::files) fn pipes_discover(&mut self) -> Option<(u32, u32)>
        {
            // Volume identities are snapshotted because the lookup below needs
            // mutable access, so no volume borrow can stay live across it.
            let mut ids = [0u32; MAX_VOLUMES];
            let mut count = 0;
            for (id, mounted) in self.volumes.iter()
            {
                if mounted.info().is_none() || count == ids.len()
                {
                    continue;
                }
                ids[count] = id;
                count += 1;
            }
            for volume in ids.into_iter().take(count)
            {
                let Some(info) = self.volumes.get(volume).and_then(Mounted::info)
                else
                {
                    continue;
                };
                if info.blessed_folder < ROOT_ID
                {
                    continue;
                }
                if let Ok(found) = self.lookup(volume, info.blessed_folder, NAME)
                   && found.directory
                {
                    return Some((volume, found.id));
                }
            }
            None
        }
    }
}

