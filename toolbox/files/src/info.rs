//! Decoded native metadata. None of these snapshots is a trap request record.
use mac_abi::{Point, Rect, files as wire};
use mac_traps::{Error, Result, types::InlineText};

use super::{Cnid, Filesystem, Timestamp, TypeCode, VolumeId};

mac_traps::native_flags!(CatalogFlags, u16; LOCKED = wire::FILE_LOCKED);
mac_traps::native_flags!(FinderFlags, u16;
    STATIONERY = wire::FINDER_STATIONERY, NAME_LOCKED = wire::FINDER_NAME_LOCKED,
    INVISIBLE = 0x4000, CUSTOM_ICON = 0x0400, INITIALIZED = 0x0100);
mac_traps::native_flags!(VolumeAttributes, u16;);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EntryKind
{
    #[default]
    File,
    Directory
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogInfo
{
    pub volume:          VolumeId,
    pub id:              Cnid,
    pub parent:          Cnid,
    pub kind:            EntryKind,
    pub data_length:     u64,
    pub resource_length: u64,
    pub file_type:       TypeCode,
    pub creator:         TypeCode,
    pub flags:           CatalogFlags,
    pub child_count:     u32,
    pub created:         Timestamp,
    pub modified:        Timestamp,
    pub backup:          Option<Timestamp>,
    pub finder_info:     [u8; 32],
    pub name:            InlineText<256>
}
impl Default for CatalogInfo
{
    fn default() -> Self
    {
        Self { volume:          VolumeId::NONE,
               id:              Cnid::UNSAVED,
               parent:          Cnid::UNSAVED,
               kind:            EntryKind::File,
               data_length:     0,
               resource_length: 0,
               file_type:       TypeCode::ZERO,
               creator:         TypeCode::ZERO,
               flags:           CatalogFlags::NONE,
               child_count:     0,
               created:         Timestamp::from_mac_microseconds(0),
               modified:        Timestamp::from_mac_microseconds(0),
               backup:          None,
               finder_info:     [0; 32],
               name:            InlineText::default() }
    }
}
impl TryFrom<wire::CatalogInfo> for CatalogInfo
{
    type Error = Error;

    fn try_from(info: wire::CatalogInfo) -> Result<Self>
    {
        Ok(Self { volume:          VolumeId(info.volume),
                  id:              Cnid(info.id),
                  parent:          Cnid(info.parent),
                  kind:            match info.kind
                  {
                      wire::FILE => EntryKind::File,
                      wire::DIRECTORY => EntryKind::Directory,
                      _ => return Err(Error::BadAbi)
                  },
                  data_length:     info.data_length,
                  resource_length: info.resource_length,
                  file_type:       TypeCode::new(info.file_type),
                  creator:         TypeCode::new(info.creator),
                  flags:           CatalogFlags::from_bits_retain(info.flags),
                  child_count:     info.child_count,
                  created:         info.created_at(),
                  modified:        info.modified_at(),
                  backup:          (info.backup_present != 0).then(|| info.backed_up_at()),
                  finder_info:     info.finder_info,
                  name:            InlineText::from_reply(info.name, info.name_length)? })
    }
}
impl CatalogInfo
{
    pub fn name(&self) -> &str
    {
        self.name.as_str()
    }

    pub const fn created_at(&self) -> Timestamp
    {
        self.created
    }

    pub const fn modified_at(&self) -> Timestamp
    {
        self.modified
    }

    pub const fn backed_up_at(&self) -> Option<Timestamp>
    {
        self.backup
    }

    pub fn is_directory(&self) -> bool
    {
        self.kind == EntryKind::Directory
    }

    pub fn is_locked(&self) -> bool
    {
        !self.is_directory() && self.flags.contains(CatalogFlags::LOCKED)
    }

    pub fn is_stationery(&self) -> bool
    {
        !self.is_directory() && self.finder_flags().contains(FinderFlags::STATIONERY)
    }

    pub fn is_name_locked(&self) -> bool
    {
        self.finder_flags().contains(FinderFlags::NAME_LOCKED)
    }

    pub fn finder_flags(&self) -> FinderFlags
    {
        FinderFlags::from_bits_retain(u16::from_be_bytes([self.finder_info[8],
                                                          self.finder_info[9]]))
    }

    pub fn icon_location(&self) -> Point
    {
        Point { v: i16::from_be_bytes([self.finder_info[10], self.finder_info[11]]),
                h: i16::from_be_bytes([self.finder_info[12], self.finder_info[13]]) }
    }

    pub fn folder_window_bounds(&self) -> Option<Rect>
    {
        self.is_directory().then(|| Rect::from_be_bytes(self.finder_info[..8].try_into().unwrap()))
    }

    pub fn folder_view(&self) -> Option<u16>
    {
        self.is_directory()
            .then(|| u16::from_be_bytes([self.finder_info[14], self.finder_info[15]]))
    }

    pub fn folder_scroll(&self) -> Option<Point>
    {
        self.is_directory().then(|| {
                               Point { v: i16::from_be_bytes([self.finder_info[16],
                                                              self.finder_info[17]]),
                                       h: i16::from_be_bytes([self.finder_info[18],
                                                              self.finder_info[19]]) }
                           })
    }

    pub fn put_away_parent(&self) -> Cnid
    {
        Cnid(u32::from_be_bytes(self.finder_info[28..32].try_into().unwrap()))
    }

    pub fn folder_open_chain(&self) -> Option<Cnid>
    {
        self.is_directory()
            .then(|| Cnid(u32::from_be_bytes(self.finder_info[20..24].try_into().unwrap())))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeCapacity
{
    pub total_bytes: u64,
    pub free_bytes:  u64
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolumeSource
{
    BlockDevice
    {
        disk:            u32,
        partition_start: u64
    },
    Filesystem
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeInfo
{
    pub id:             VolumeId,
    pub source:         VolumeSource,
    pub capacity:       Option<VolumeCapacity>,
    pub created:        Timestamp,
    pub modified:       Timestamp,
    pub filesystem:     Filesystem,
    pub blessed_folder: Cnid,
    pub attributes:     VolumeAttributes,
    pub read_only:      bool,
    pub startup:        bool,
    pub icon_kind:      TypeCode,
    pub icon_id:        i16,
    pub finder_info:    [u8; 32],
    pub name:           InlineText<256>
}
impl TryFrom<wire::VolumeInfo> for VolumeInfo
{
    type Error = Error;

    fn try_from(info: wire::VolumeInfo) -> Result<Self>
    {
        Ok(Self { id:             VolumeId(info.id),
                  source:         if info.disk == u32::MAX
                  {
                      if info.partition_start != 0
                      {
                          return Err(Error::BadAbi);
                      }
                      VolumeSource::Filesystem
                  }
                  else
                  {
                      VolumeSource::BlockDevice { disk:            info.disk,
                                                  partition_start: info.partition_start }
                  },
                  capacity:       match info.capacity_known
                  {
                      0 if info.total_bytes == 0 && info.free_bytes == 0 => None,
                      1 if info.free_bytes <= info.total_bytes =>
                      {
                          Some(VolumeCapacity { total_bytes: info.total_bytes,
                                                free_bytes:  info.free_bytes })
                      }
                      _ => return Err(Error::BadAbi)
                  },
                  created:        info.created_at(),
                  modified:       info.modified_at(),
                  filesystem:     match (info.filesystem, info.filesystem_version)
                  {
                      (wire::FILESYSTEM_HFS, 0) => Filesystem::Hfs,
                      (wire::FILESYSTEM_HFS_B, version) if version != 0 =>
                      {
                          Filesystem::HfsB { version }
                      }
                      (kind, version)
                          if version != 0
                             && kind.to_be_bytes()
                                    .iter()
                                    .all(|b| b.is_ascii_graphic() || *b == b' ')
                             && kind.to_be_bytes() != *b"    " =>
                      {
                          Filesystem::Driver { kind: TypeCode::new(kind.to_be_bytes()),
                                               version }
                      }
                      _ => return Err(Error::BadAbi)
                  },
                  blessed_folder: Cnid(info.blessed_folder),
                  attributes:     VolumeAttributes::from_bits_retain(info.attributes),
                  read_only:      info.read_only != 0,
                  startup:        info.startup != 0,
                  icon_kind:      TypeCode::new(info.icon_kind),
                  icon_id:        info.icon_id,
                  finder_info:    info.finder_info,
                  name:           InlineText::from_reply(info.name, info.name_length)? })
    }
}
impl VolumeInfo
{
    pub fn name(&self) -> &str
    {
        self.name.as_str()
    }

    pub const fn created_at(&self) -> Timestamp
    {
        self.created
    }

    pub const fn modified_at(&self) -> Timestamp
    {
        self.modified
    }
}
