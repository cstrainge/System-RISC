//! Read-only native File Manager. Volume IDs and open handles are opaque.
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
pub const SCAN_ENTRY: u32 = 0;
pub const SCAN_PENDING: u32 = 1;
pub const SCAN_DONE: u32 = 2;
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
    pub reserved:  u32
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FileScanCall
{
    pub operation:          u32,
    pub volume:             u32,
    pub directory:          u32,
    pub step:               u32,
    pub cursor:             DirectoryScanCursor,
    pub entry:              CatalogInfo,
    pub data_allocated:     u64,
    pub resource_allocated: u64
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

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeInfo
{
    pub id:              u32,
    pub disk:            u32,
    pub partition_start: u64,
    pub total_bytes:     u64,
    pub free_bytes:      u64,
    pub created:         u32,
    pub modified:        u32,
    pub attributes:      u16,
    pub read_only:       u16,
    pub startup:         u32,
    pub icon_kind:       [u8; 4],
    pub icon_id:         i16,
    pub reserved:        u16,
    pub finder_info:     [u8; 32],
    pub name_length:     u32,
    pub name:            [u8; 96]
}
impl Default for VolumeInfo
{
    fn default() -> Self
    {
        Self { id:              0,
               disk:            0,
               partition_start: 0,
               total_bytes:     0,
               free_bytes:      0,
               created:         0,
               modified:        0,
               attributes:      0,
               read_only:       1,
               startup:         0,
               icon_kind:       [0; 4],
               icon_id:         0,
               reserved:        0,
               finder_info:     [0; 32],
               name_length:     0,
               name:            [0; 96] }
    }
}
impl VolumeInfo
{
    pub const fn created_at(&self) -> crate::dates::Timestamp
    {
        crate::dates::Timestamp::from_mac_seconds(self.created)
    }

    pub const fn modified_at(&self) -> crate::dates::Timestamp
    {
        crate::dates::Timestamp::from_mac_seconds(self.modified)
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
    pub child_count:     u16,
    pub created:         u32,
    pub modified:        u32,
    pub backup:          u32,
    pub finder_info:     [u8; 32],
    pub name_length:     u32,
    pub name:            [u8; 128]
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
               finder_info:     [0; 32],
               name_length:     0,
               name:            [0; 128] }
    }
}
impl CatalogInfo
{
    pub const fn created_at(&self) -> crate::dates::Timestamp
    {
        crate::dates::Timestamp::from_mac_seconds(self.created)
    }

    pub const fn modified_at(&self) -> crate::dates::Timestamp
    {
        crate::dates::Timestamp::from_mac_seconds(self.modified)
    }

    pub const fn backed_up_at(&self) -> crate::dates::Timestamp
    {
        crate::dates::Timestamp::from_mac_seconds(self.backup)
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

    pub fn icon_location(&self) -> crate::Point
    {
        crate::Point { v: i16::from_be_bytes([self.finder_info[10], self.finder_info[11]]),
                       h: i16::from_be_bytes([self.finder_info[12], self.finder_info[13]]) }
    }

    /// Raw persisted DInfo.frRect for a directory. This does not imply the
    /// rectangle is initialized, visible, or valid for the current screen.
    pub fn folder_window_bounds(&self) -> Option<crate::Rect>
    {
        self.is_directory()
            .then(|| crate::Rect::from_be_bytes(self.finder_info[..8].try_into().unwrap()))
    }

    /// Raw persisted DInfo.frView word. Finder-specific view/flag interpretation
    /// is intentionally left to Finder, including unknown high-byte bits.
    pub fn folder_view(&self) -> Option<u16>
    {
        self.is_directory()
            .then(|| u16::from_be_bytes([self.finder_info[14], self.finder_info[15]]))
    }

    /// Raw persisted DXInfo.frScroll, without changing sign or coordinate origin.
    pub fn folder_scroll(&self) -> Option<crate::Point>
    {
        self.is_directory().then(|| {
                               crate::Point { v: i16::from_be_bytes([self.finder_info[16],
                                                                     self.finder_info[17]]),
                                              h: i16::from_be_bytes([self.finder_info[18],
                                                                     self.finder_info[19]]) }
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
    pub operation:   u32,
    pub volume:      u32,
    pub cnid:        u32,
    pub index:       u32,
    pub handle:      u32,
    pub fork:        u32,
    pub whence:      u32,
    pub reserved:    u32,
    pub offset:      i64,
    pub position:    u64,
    pub length:      u64,
    pub name:        *const u8,
    pub name_length: usize,
    pub output:      *mut u8,
    pub capacity:    usize,
    pub transferred: usize,
    pub volume_info: VolumeInfo,
    pub entry:       CatalogInfo
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
