//! Native File Manager. Volume IDs and open handles are opaque.
//! Directory enumeration is zero-based; catalog identities are persistent HFS CNIDs.
pub const VOLUME_COUNT: u32 = 0;
pub const VOLUME_INDEX: u32 = 1;
pub const VOLUME_INFO: u32 = 2;
pub const CATALOG: u32 = 3;
pub const DIRECTORY_ENTRY: u32 = 4;
pub const LOOKUP: u32 = 5;
pub const PARENT: u32 = 6;
pub const PATH: u32 = 7;
pub const OPEN: u32 = 8;
pub const READ: u32 = 9;
pub const SEEK: u32 = 10;
pub const CLOSE: u32 = 11;
pub const TELL: u32 = 12;
pub const NEW_FOLDER: u32 = 13;
pub const TRASH_DIRECTORY: u32 = 14;
pub const MOVE_TO_TRASH: u32 = 15;
pub const PUT_AWAY: u32 = 16;
pub const EMPTY_TRASH: u32 = 17;
pub const TRASH_USAGE: u32 = 18;
/// Exact catalog physical fork lengths; position=data, length=resource.
pub const ALLOCATION_INFO: u32 = 19;
pub const RENAME: u32 = 20;
pub const SET_LOCKED: u32 = 21;
pub const SET_STATIONERY: u32 = 22;
pub const COMMENT: u32 = 23;
pub const SET_COMMENT: u32 = 24;
/// Size-distinct incremental request; the existing FileCall remains unchanged.
pub const SCAN_DIRECTORY: u32 = 25;
pub const HOST_EXPORT_COUNT: u32 = 26;
pub const MOUNT_HOST_EXPORT: u32 = 27;
pub const UNMOUNT_VOLUME: u32 = 28;
/// Mount a leased data fork using a file-backed filesystem provider.
pub const MOUNT_FILE: u32 = 29;
/// Dispatch a loaded driver's association. index=0 means unhandled; index=1
/// returns its mounted volume. Applications don't select a driver by name.
pub const OPEN_ITEM: u32 = 30;
/// Nonblocking directory watch service. Registration uses volume/cnid; polling
/// and disposal use handle. Poll returns a coalesced change in index and a
/// monotonic catalog revision in position. No caller buffer survives a call.
pub const WATCH_DIRECTORY: u32 = 31;
pub const POLL_WATCH: u32 = 32;
pub const DISPOSE_WATCH: u32 = 33;
/// Persist icon placement: fork=0 clears, fork=1 sets; index packs signed h/v
/// in its low/high 16 bits. position is the catalog-change timestamp, not an
/// object date. (-1,-1) is reserved for an unplaced item.
pub const SET_ICON_LOCATION: u32 = 34;
/// Move within a mounted volume. cnid is the object, index its new parent.
/// Preserve identity, object dates, Finder information and both forks.
pub const MOVE_INTO: u32 = 35;
pub const MOVE_ICONS: u32 = 36;
/// Save DInfo.frRect/frView and DXInfo.frScroll. Input uses only entry.finder_info
/// bytes 0..8 and 14..20; all other bytes must be zero. Preserve every other
/// Finder field and both forks. position is the catalog-change timestamp.
pub const SET_FOLDER_STATE: u32 = 37;
/// Remove a file or empty folder, retaining an owned receipt through cleanup.
pub const REMOVE: u32 = 38;
/// Replace object flags, Finder info, dates, backup and UTF-8 comment together.
/// Properties are in entry; name/name_length carry only the comment text.
/// Every non-property input field of entry must be zero.
pub const SET_PROPERTIES: u32 = 39;
/// Ordinary open descriptions, separate from atomic Save/replace transactions.
pub const OPEN_OPTIONS: u32 = 40;
pub const WRITE: u32 = 41;
/// Development stepping request: length is target on input and actual on
/// output; growth is currently limited to MAX_FILE_TRANSFER per call.
pub const SET_LENGTH: u32 = 42;
pub const SYNC: u32 = 43;
pub const DUPLICATE: u32 = 44;
/// Process path operations use UTF-8 `/Volumes/<unique mounted name>/...` paths.
/// CURRENT_DIRECTORY returns bytes, without a terminator, in output/transferred.
pub const CURRENT_DIRECTORY: u32 = 46;
pub const CHANGE_DIRECTORY: u32 = 47;
pub const RESOLVE_PATH: u32 = 48;
pub const OPEN_PATH: u32 = 49;
/// Query the actual executable alias; output/transferred contain UTF-8 bytes.
pub const CURRENT_EXECUTABLE_PATH: u32 = 50;
/// Open a named pipe as an item: `volume`/`cnid` name a live entry in the pipes
/// folder, and the reply carries a writer handle for the calling process. The
/// File Manager performs `connect` on the caller's behalf, because a pipe item
/// has no fork for the generic `OPEN` to act on. A destroyed pipe is
/// `NOT_FOUND`; an identity that is not a live named pipe is `BAD_ARGUMENT`.
/// The generic `OPEN` keeps refusing these items.
pub const OPEN_PIPE: u32 = 51;
/// Item name of the named-pipe folder under the volume's blessed folder. One
/// spelling shared by the image builder, which creates it, and the resident
/// File Manager, which enumerates it and records its identity.
pub const PIPES_FOLDER_NAME: &str = "Pipes";
pub const OPEN_READ: u32 = 1;
pub const OPEN_WRITE: u32 = 2;
pub const OPEN_APPEND: u32 = 4;
pub const OPEN_CREATE: u32 = 8;
pub const OPEN_CREATE_NEW: u32 = 16;
pub const OPEN_TRUNCATE: u32 = 32;
pub const OPEN_FLAGS: u32 =
    OPEN_READ | OPEN_WRITE | OPEN_APPEND | OPEN_CREATE | OPEN_CREATE_NEW | OPEN_TRUNCATE;
pub const SYNC_DATA: u32 = 1;
pub const MAX_FILE_TRANSFER: usize = 64 * 1024;
/// READ index flag: output may be uninitialized. Native HFS-B copies only the
/// returned prefix; providers without this boundary must return Unsupported.
pub const READ_UNINITIALIZED: u32 = 1;
/// Locate a per-volume special directory. volume=0 selects the startup volume;
/// file_type='desk' or 'trsh', index=create (0/1), position=creation timestamp
/// (zero for lookup only). Returns entry and its actual volume.
pub const FIND_FOLDER: u32 = 45;
/// Native combined desktop iteration, using FileScanCall.desktop. Read-only.
pub const SCAN_DESKTOP: u32 = 46;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IconMove
{
    pub cnid:            u32,
    pub expected_parent: u32,
    pub parent:          u32,
    /// 0 keeps placement, 1 sets point, 2 clears placement.
    pub placement:       u32,
    pub point:           crate::geometry::Point
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IconMoveCall
{
    pub operation: u32,
    pub volume:    u32,
    pub count:     u32,
    pub changed:   u32,
    pub when:      u64,
    pub entries:   *const IconMove
}
/// Size-distinct owner for stepped same-volume catalog moves.
pub const BEGIN_ICON_GROUP: u32 = 0;
pub const STEP_ICON_GROUP: u32 = 1;
pub const CANCEL_ICON_GROUP: u32 = 2;
pub const ABANDON_ICON_GROUP: u32 = 3;
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IconGroupCall
{
    pub operation: u32,
    pub handle:    u64,
    pub volume:    u32,
    pub phase:     u32,
    pub when:      u64,
    pub count:     u32,
    pub completed: u32,
    pub changed:   u32,
    pub reserved:  u32,
    pub entries:   *const IconMove
}
pub const SCAN_ENTRY: u32 = 0;
pub const SCAN_PENDING: u32 = 1;
pub const SCAN_DONE: u32 = 2;
/// Unknown physical fork allocation, projected as Option in the Rust SDK.
/// Mounted HFS/HFS-B block-address bounds cannot produce this reserved value.
pub const ALLOCATION_UNKNOWN: u64 = u64::MAX;
pub const CHILD_COUNT_UNKNOWN: u32 = u32::MAX;
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirectoryScanCursor
{
    pub revision:  u64,
    pub volume:    u32,
    pub directory: u32,
    pub node:      u32,
    pub previous:  u32,
    pub index:     u32,
    pub traversed: u32,
    pub state:     u32,
    /// Revision of the resident pipe registry for a scan of the Pipes folder,
    /// zero for every ordinary directory. A registration or destruction while
    /// the enumeration is open makes the continuation stale.
    pub reserved:  u32,
    /// Row offset into the pipe registry once the catalog portion of the Pipes
    /// folder is exhausted. Zero until then, and unused elsewhere.
    pub registry:  u32
}
/// Size-distinct open of a named pipe as an item. `FileCall.handle` is 32 bits
/// and a pipe handle is 64, so the writer travels in its own width instead of
/// being truncated into the generic call. `folder` reports the folder that
/// contained the item, so a caller can verify the item it opened.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PipeOpenCall
{
    pub operation: u32,
    pub volume:    u32,
    pub cnid:      u32,
    pub reserved:  u32,
    /// Output: the writer handle `connect` issued to the calling process.
    pub writer:    u64,
    /// Output: the folder that contained the item at open time.
    pub folder:    u32,
    pub reserved2: u32,
    /// Padding that keeps this structure a size no other `FILE` request uses.
    /// The `FILE` trap selects a request by its size alone, so two requests of
    /// equal size cannot be told apart; must be zero.
    pub reserved3: u32
}

// The `FILE` trap identifies a request by its length, and so does the nested
// pointer validation that runs before dispatch. A length shared with another
// request would deliver this one to the wrong validator and refuse a valid
// call, so a collision is a build failure rather than a runtime mystery.
const _: () = {
    let pipe = size_of::<PipeOpenCall>();
    assert!(pipe != size_of::<FileCall>());
    assert!(pipe != size_of::<FileScanCall>());
    assert!(pipe != size_of::<DataWriteCall>());
    assert!(pipe != size_of::<ForkWriteCall>());
    assert!(pipe != size_of::<FileTransferCall>());
    assert!(pipe != size_of::<IconMoveCall>());
    assert!(pipe != size_of::<IconGroupCall>());
};
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FileScanCall
{
    pub operation:          u32,
    pub volume:             u32,
    pub directory:          u32,
    pub step:               u32,
    pub cursor:             DirectoryScanCursor,
    pub desktop:            DesktopScanCursor,
    pub entry:              CatalogInfo,
    pub data_allocated:     u64,
    pub resource_allocated: u64
}
/// Bounded continuation over live mounted volumes and their ordinary catalogs.
/// Default starts enumeration; no synthetic volume or catalog identity exists.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DesktopScanCursor
{
    pub volume:    u32,
    pub state:     u32,
    pub directory: DirectoryScanCursor
}
pub const COMMENT_MAX_BYTES: usize = 255;
pub const FILE_LOCKED: u16 = 0x0001;
pub const FINDER_STATIONERY: u16 = 0x0800;
pub const FINDER_NAME_LOCKED: u16 = 0x1000;
pub const DATA_FORK: u32 = 0;
pub const RESOURCE_FORK: u32 = 1;
pub const SEEK_START: u32 = 0;
pub const SEEK_CURRENT: u32 = 1;
pub const SEEK_END: u32 = 2;
pub const FILE: u32 = 0;
pub const DIRECTORY: u32 = 1;
pub const ROOT_ID: u32 = 2;
/// Filesystem identity is its big-endian FourCC, independent of its backend.
pub const FILESYSTEM_HFS: u32 = u32::from_be_bytes(*b"HFS ");
pub const FILESYSTEM_HFS_B: u32 = u32::from_be_bytes(*b"HFSB");

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeInfo
{
    pub id:                 u32,
    pub disk:               u32,
    pub partition_start:    u64,
    pub total_bytes:        u64,
    pub free_bytes:         u64,
    pub created:            u64,
    pub modified:           u64,
    pub filesystem:         u32,
    pub filesystem_version: u32,
    pub blessed_folder:     u32,
    pub attributes:         u16,
    pub read_only:          u16,
    pub startup:            u32,
    pub icon_kind:          [u8; 4],
    pub icon_id:            i16,
    /// 0 = capacity unknown (both byte fields zero); 1 = measured capacity.
    pub capacity_known:     u16,
    pub finder_info:        [u8; 32],
    pub name_length:        u32,
    pub name:               [u8; 256]
}
impl Default for VolumeInfo
{
    fn default() -> Self
    {
        Self { id:                 0,
               disk:               0,
               partition_start:    0,
               total_bytes:        0,
               free_bytes:         0,
               created:            0,
               modified:           0,
               filesystem:         0,
               filesystem_version: 0,
               blessed_folder:     0,
               attributes:         0,
               read_only:          1,
               startup:            0,
               icon_kind:          [0; 4],
               icon_id:            0,
               capacity_known:     0,
               finder_info:        [0; 32],
               name_length:        0,
               name:               [0; 256] }
    }
}
impl VolumeInfo
{
    pub const fn created_at(&self) -> crate::dates::Timestamp
    {
        crate::dates::Timestamp::from_mac_microseconds(self.created)
    }

    pub const fn modified_at(&self) -> crate::dates::Timestamp
    {
        crate::dates::Timestamp::from_mac_microseconds(self.modified)
    }

    pub fn name(&self) -> &str
    {
        core::str::from_utf8(&self.name[..(self.name_length as usize).min(self.name.len())]).unwrap_or("")
    }
}
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogInfo
{
    pub volume:          u32,
    pub id:              u32,
    pub parent:          u32,
    pub kind:            u32,
    pub data_length:     u64,
    pub resource_length: u64,
    pub file_type:       [u8; 4],
    pub creator:         [u8; 4],
    pub flags:           u16,
    pub child_count:     u32,
    pub created:         u64,
    pub modified:        u64,
    pub backup:          u64,
    pub backup_present:  u32,
    pub finder_info:     [u8; 32],
    pub name_length:     u32,
    pub name:            [u8; 256]
}
impl Default for CatalogInfo
{
    fn default() -> Self
    {
        Self { volume:          0,
               id:              0,
               parent:          0,
               kind:            0,
               data_length:     0,
               resource_length: 0,
               file_type:       [0; 4],
               creator:         [0; 4],
               flags:           0,
               child_count:     0,
               created:         0,
               modified:        0,
               backup:          0,
               backup_present:  0,
               finder_info:     [0; 32],
               name_length:     0,
               name:            [0; 256] }
    }
}
impl CatalogInfo
{
    pub const fn created_at(&self) -> crate::dates::Timestamp
    {
        crate::dates::Timestamp::from_mac_microseconds(self.created)
    }

    pub const fn modified_at(&self) -> crate::dates::Timestamp
    {
        crate::dates::Timestamp::from_mac_microseconds(self.modified)
    }

    pub const fn backed_up_at(&self) -> crate::dates::Timestamp
    {
        crate::dates::Timestamp::from_mac_microseconds(self.backup)
    }

    pub fn name(&self) -> &str
    {
        core::str::from_utf8(&self.name[..(self.name_length as usize).min(self.name.len())]).unwrap_or("")
    }

    pub fn is_directory(&self) -> bool
    {
        self.kind == DIRECTORY
    }

    /// The catalog's file lock is independent of Finder flag bits.
    pub fn is_locked(&self) -> bool
    {
        !self.is_directory() && self.flags & FILE_LOCKED != 0
    }

    pub fn is_stationery(&self) -> bool
    {
        !self.is_directory() && self.finder_flags() & FINDER_STATIONERY != 0
    }

    pub fn is_name_locked(&self) -> bool
    {
        self.finder_flags() & FINDER_NAME_LOCKED != 0
    }

    /// Finder visibility, custom-icon and saved-position flags. `flags` above
    /// contains the separate HFS catalog record flags (for example file lock).
    pub fn finder_flags(&self) -> u16
    {
        u16::from_be_bytes([self.finder_info[8], self.finder_info[9]])
    }

    pub fn icon_location(&self) -> crate::geometry::Point
    {
        crate::geometry::Point { v: i16::from_be_bytes([self.finder_info[10],
                                                        self.finder_info[11]]),
                                 h: i16::from_be_bytes([self.finder_info[12],
                                                        self.finder_info[13]]) }
    }

    /// Raw persisted DInfo.frRect for a directory. This does not imply the
    /// rectangle is initialized, visible, or valid for the current screen.
    pub fn folder_window_bounds(&self) -> Option<crate::geometry::Rect>
    {
        self.is_directory()
            .then(|| crate::geometry::Rect::from_be_bytes(self.finder_info[..8].try_into().unwrap()))
    }

    /// Raw persisted DInfo.frView word. Finder-specific view/flag interpretation
    /// is intentionally left to Finder, including unknown high-byte bits.
    pub fn folder_view(&self) -> Option<u16>
    {
        self.is_directory()
            .then(|| u16::from_be_bytes([self.finder_info[14], self.finder_info[15]]))
    }

    /// Raw persisted DXInfo.frScroll, without changing sign or coordinate origin.
    pub fn folder_scroll(&self) -> Option<crate::geometry::Point>
    {
        self.is_directory().then(|| {
                               crate::geometry::Point { v: i16::from_be_bytes([self.finder_info
                                                                                   [16],
                                                                               self.finder_info
                                                                                   [17]]),
                                                        h: i16::from_be_bytes([self.finder_info
                                                                                   [18],
                                                                               self.finder_info
                                                                                   [19]]) }
                           })
    }

    /// Raw DXInfo.frOpenChain catalog identity. Startup-list membership and
    /// termination rules belong to Finder, rather than the read-only parser.
    pub fn put_away_parent(&self) -> u32
    {
        u32::from_be_bytes(self.finder_info[28..32].try_into().unwrap())
    }

    pub fn folder_open_chain(&self) -> Option<u32>
    {
        self.is_directory()
            .then(|| u32::from_be_bytes(self.finder_info[20..24].try_into().unwrap()))
    }
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct FileCall
{
    pub operation:    u32,
    /// FIND_FOLDER special-directory type; zero for other operations.
    pub file_type:    [u8; 4],
    pub volume:       u32,
    pub cnid:         u32,
    pub index:        u32,
    pub handle:       u32,
    pub fork:         u32,
    pub whence:       u32,
    pub reserved:     u32,
    /// Input only for OPEN_OPTIONS/OPEN_PATH; unknown bits refuse before mutation.
    pub open_flags:   u32,
    pub offset:       i64,
    pub position:     u64,
    pub length:       u64,
    pub name:         *const u8,
    pub name_length:  usize,
    /// Immutable borrowed bytes for WRITE. System owns its staging before waits.
    pub input:        *const u8,
    pub input_length: usize,
    pub output:       *mut u8,
    pub capacity:     usize,
    pub transferred:  usize,
    pub volume_info:  VolumeInfo,
    pub entry:        CatalogInfo
}

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn folder_state_accessors_preserve_original_signed_coordinates_and_raw_bits()
    {
        let mut folder = CatalogInfo { kind: DIRECTORY,
                                       ..Default::default() };
        folder.finder_info[..8].copy_from_slice(&crate::Rect::new(34, -12, 350, 210).to_be_bytes());
        folder.finder_info[8..10].copy_from_slice(&0x5100u16.to_be_bytes());
        folder.finder_info[10..12].copy_from_slice(&(-7i16).to_be_bytes());
        folder.finder_info[12..14].copy_from_slice(&(-9i16).to_be_bytes());
        folder.finder_info[14..16].copy_from_slice(&0x8143u16.to_be_bytes());
        folder.finder_info[16..18].copy_from_slice(&(-320i16).to_be_bytes());
        folder.finder_info[18..20].copy_from_slice(&48i16.to_be_bytes());
        folder.finder_info[20..24].copy_from_slice(&0x81002020u32.to_be_bytes());
        assert_eq!(folder.finder_flags(), 0x5100);
        assert_eq!(folder.icon_location(),
                   crate::Point { h: -9,
                                  v: -7 });
        assert_eq!(folder.folder_window_bounds(), Some(crate::Rect::new(34, -12, 350, 210)));
        assert_eq!(folder.folder_view(), Some(0x8143));
        assert_eq!(folder.folder_scroll(),
                   Some(crate::Point { h: 48,
                                       v: -320 }));
        assert_eq!(folder.folder_open_chain(), Some(0x81002020));
        let file = CatalogInfo { kind: FILE,
                                 ..folder };
        assert_eq!(file.folder_window_bounds(), None);
        assert_eq!(file.folder_view(), None);
        assert_eq!(file.folder_scroll(), None);
        assert_eq!(file.folder_open_chain(), None);
        // Uninitialized bytes are retained; validity is not silently invented.
        assert_eq!(CatalogInfo { kind: DIRECTORY,
                                 ..Default::default() }.folder_window_bounds(),
                   Some(crate::Rect::ZERO));
    }
}

pub const BEGIN_DATA_WRITE: u32 = 0;
pub const APPEND_DATA_WRITE: u32 = 1;
pub const COMMIT_DATA_WRITE: u32 = 2;
pub const ABORT_DATA_WRITE: u32 = 3;
pub const MAX_DATA_WRITE: usize = 4 * 1024 * 1024;
pub const BEGIN_FORK_WRITE: u32 = 0;
pub const APPEND_FORK_WRITE: u32 = 1;
pub const FINISH_FORK_WRITE: u32 = 2;
pub const ABORT_FORK_WRITE: u32 = 3;
pub const ABANDON_FORK_WRITE: u32 = 4;
pub const READ_FORK_WRITE: u32 = 5;
/// Native streaming two-fork transaction. Payload requests are at most 64 KiB;
/// declared fork lengths are u64 and never require whole-fork allocations.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ForkWriteCall
{
    pub operation:       u32,
    pub handle:          u64,
    pub volume:          u32,
    pub parent:          u32,
    pub expected_cnid:   u32,
    pub fork:            u32,
    pub when:            u64,
    pub data_length:     u64,
    pub resource_length: u64,
    pub created:         u64,
    pub modified:        u64,
    pub backup:          u64,
    pub backup_present:  u32,
    pub attributes:      u16,
    pub padding:         u16,
    pub finder_info:     [u8; 32],
    pub name:            *const u8,
    pub name_length:     usize,
    pub comment:         *const u8,
    pub comment_length:  usize,
    pub data:            *const u8,
    pub count:           usize,
    pub output:          *mut u8,
    pub capacity:        usize,
    pub offset:          u64,
    pub transferred:     usize,
    /// Zero requires another bounded commit/abort step; one is terminal.
    pub completed:       u32,
    pub reserved:        u32,
    pub entry:           CatalogInfo
}
/// Size-distinct native copy-on-write data-file transaction. expected_cnid=0
/// means create-new; otherwise commit requires that same confirmed file ID.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DataWriteCall
{
    pub operation:     u32,
    pub handle:        u64,
    pub volume:        u32,
    pub parent:        u32,
    pub expected_cnid: u32,
    pub when:          u64,
    pub file_type:     [u8; 4],
    pub creator:       [u8; 4],
    pub length:        usize,
    pub name:          *const u8,
    pub name_length:   usize,
    pub data:          *const u8,
    pub data_length:   usize,
    pub entry:         CatalogInfo,
    pub reserved:      [u64; 2]
}

/// Owned recursive transfer requests use a size-distinct File trap record.
pub const BEGIN_TRANSFER: u32 = 0;
pub const STEP_TRANSFER: u32 = 1;
pub const CANCEL_TRANSFER: u32 = 2;
pub const ABANDON_TRANSFER: u32 = 3;
pub const TRANSFER_COPY: u32 = 0;
pub const TRANSFER_MOVE: u32 = 1;
pub const TRANSFER_TRASH: u32 = 2;
/// Move each source to its own volume's Desktop Folder.
pub const TRANSFER_DESKTOP: u32 = 3;
pub const TRANSFER_PREPARING: u32 = 0;
pub const TRANSFER_COPYING: u32 = 1;
pub const TRANSFER_VERIFYING: u32 = 2;
pub const TRANSFER_COMMITTING: u32 = 3;
pub const TRANSFER_ROLLING_BACK: u32 = 4;
pub const TRANSFER_CLEANING_UP: u32 = 5;
pub const TRANSFER_COMMITTED: u32 = 6;
pub const TRANSFER_CANCELLED: u32 = 7;
pub const TRANSFER_FAILED: u32 = 8;
pub const TRANSFER_READING: u32 = 9;
pub const TRANSFER_WRITING: u32 = 10;
pub const MAX_TRANSFER_SOURCES: usize = 1024;
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileTransferSource
{
    pub volume:          u32,
    pub cnid:            u32,
    pub expected_parent: u32,
    /// 0 preserve copied placement; 1 place at point; 2 clear placement.
    pub placement:       u32,
    pub point:           crate::geometry::Point,
    pub reserved:        u32
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FileTransferCall
{
    pub operation:          u32,
    pub handle:             u64,
    pub mode:               u32,
    pub destination_volume: u32,
    pub destination_parent: u32,
    pub phase:              u32,
    pub completed_items:    u32,
    pub total_items:        u32,
    pub completed_bytes:    u64,
    pub total_bytes:        u64,
    pub when:               u64,
    /// Original cause and any failed cleanup remain separately typed.
    pub error:              crate::Status,
    pub cleanup_error:      crate::Status,
    /// The irreversible barrier has won; cancellation can no longer undo.
    pub commit_decided:     u32,
    pub sources:            *const FileTransferSource,
    pub source_count:       usize,
    pub current:            CatalogInfo,
    pub reserved:           [u32; 4]
}

// The native File gate selects the record family by its exact byte size. Keep
// every family disjoint at compile time: a collision would route valid calls to
// the wrong service before operation-specific validation could run.
#[cfg(target_pointer_width = "64")]
const _: () = {
    let sizes = [core::mem::size_of::<FileCall>(),
                 core::mem::size_of::<DataWriteCall>(),
                 core::mem::size_of::<FileScanCall>(),
                 core::mem::size_of::<ForkWriteCall>(),
                 core::mem::size_of::<IconMoveCall>(),
                 core::mem::size_of::<IconGroupCall>(),
                 core::mem::size_of::<FileTransferCall>()];
    let mut left = 0;
    while left < sizes.len()
    {
        let mut right = left + 1;
        while right < sizes.len()
        {
            assert!(sizes[left] != sizes[right], "native File request sizes must be distinct");
            right += 1;
        }
        left += 1;
    }
};

// Native FILE routing distinguishes these wire request families by size;
// SCAN_DESKTOP and CURRENT_DIRECTORY have separate typed opcode namespaces.
const _: () = assert!(core::mem::size_of::<FileCall>() != core::mem::size_of::<FileScanCall>());
