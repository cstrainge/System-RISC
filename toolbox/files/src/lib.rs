//! Safe File Manager clients. Storage and HFS mutation engines live in System.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::files::*;
use mac_traps::types::{FileHandle, FileWriteHandle};
mod scan;
mod watch;
pub use mac_abi::{dates::Timestamp, files::COMMENT_MAX_BYTES};
pub use mac_traps::{Cnid, TypeCode, VolumeId};
pub use watch::DirectoryWatch;
mod info;
pub use info::{CatalogFlags, CatalogInfo, EntryKind, FinderFlags, VolumeAttributes,
               VolumeCapacity, VolumeInfo, VolumeSource};
pub const ROOT_ID: Cnid = Cnid::ROOT;
pub use mac_traps::{Client, Error, Result};
pub use scan::{DirectoryCursor, SCAN_STALE, ScanStep};
/// Native filesystem identity reported by the mounted backend. Classic HFS has
/// no explicit filesystem version field; HFS-B uses whole-number versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filesystem
{
    Hfs,
    HfsB
    {
        version: u32
    },
    /// A provider-defined filesystem FourCC and whole-number format version.
    Driver
    {
        kind:    TypeCode,
        version: u32
    }
}
impl Filesystem
{
    pub const fn name(self) -> &'static str
    {
        match self
        {
            Self::Hfs => "HFS",
            Self::HfsB { .. } => "HFS-B",
            Self::Driver { kind,
                           .. } =>
            {
                match kind.bytes()
                {
                    [b'H', b'O', b'S', b'T'] => "Host directory",
                    [b'S', b'I', b'T', b' '] => "StuffIt",
                    _ => "Filesystem"
                }
            }
        }
    }

    pub const fn version(self) -> Option<u32>
    {
        match self
        {
            Self::Hfs => None,
            Self::HfsB { version } => Some(version),
            Self::Driver { version,
                           .. } => Some(version)
        }
    }
}
impl core::fmt::Display for Filesystem
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Driver { kind,
                           .. } if self.name() == "Filesystem" => write!(f, "{kind}"),
            _ => f.write_str(self.name())
        }
    }
}
#[derive(Clone, Copy)]
pub struct Files<'a>
{
    client: Client<'a>
}
impl<'a> Files<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    fn call(self, mut call: FileCall, name: &str) -> Result<FileCall>
    {
        self.client.file(&mut call, name, &mut [])?;
        Ok(call)
    }

    /// Use the System clock. Use `rename_at` when preserving an explicit date.
    pub fn rename(self, volume: VolumeId, cnid: Cnid, name: &str) -> Result<CatalogInfo>
    {
        self.rename_at(volume, cnid, name, self.client.now()?)
    }

    /// Move an existing file or folder within a volume, preserving its identity,
    /// name, dates, icon placement, other Finder metadata and both forks.
    /// Cross-volume transfers create a distinct destination identity instead.
    pub fn move_into(self, volume: VolumeId, cnid: Cnid, parent: Cnid) -> Result<CatalogInfo>
    {
        self.move_into_at(volume, cnid, parent, self.client.now()?)
    }

    pub fn move_into_at(self, volume: VolumeId, cnid: Cnid, parent: Cnid, when: Timestamp)
                        -> Result<CatalogInfo>
    {
        if cnid.0 <= ROOT_ID.0 || parent.0 < ROOT_ID.0
        {
            return Err(Error::BadArgument);
        }
        self.call(FileCall { operation: MOVE_INTO,
                             volume: volume.0,
                             cnid: cnid.0,
                             index: parent.0,
                             position: when.mac_microseconds().ok_or(Error::BadArgument)?,
                             ..Default::default() },
                  "")?
            .entry
            .try_into()
    }

    /// Use the System clock. Use `set_locked_at` when preserving an explicit date.
    pub fn set_locked(self, volume: VolumeId, cnid: Cnid, locked: bool) -> Result<CatalogInfo>
    {
        self.set_locked_at(volume, cnid, locked, self.client.now()?)
    }

    /// Use the System clock. Use `set_stationery_at` when preserving an explicit date.
    pub fn set_stationery(self, volume: VolumeId, cnid: Cnid, stationery: bool)
                          -> Result<CatalogInfo>
    {
        self.set_stationery_at(volume, cnid, stationery, self.client.now()?)
    }

    /// Use the System clock. Use `set_comment_at` when preserving an explicit date.
    pub fn set_comment(self, volume: VolumeId, cnid: Cnid, comment: &str) -> Result<()>
    {
        self.set_comment_at(volume, cnid, comment, self.client.now()?)
    }

    /// Use the System clock. Use `new_folder_at` when preserving an explicit date.
    pub fn new_folder(self, volume: VolumeId, parent: Cnid, name: &str) -> Result<CatalogInfo>
    {
        self.new_folder_at(volume, parent, name, self.client.now()?)
    }

    /// Save Finder placement with this file/folder's persistent catalog identity.
    /// None clears placement. Object dates, forks and unrelated Finder metadata
    /// are preserved. Coordinates are the catalog's cell origin, not screen pixels.
    pub fn set_icon_location(self, volume: VolumeId, cnid: Cnid, location: Option<mac_abi::Point>)
                             -> Result<CatalogInfo>
    {
        self.set_icon_location_at(volume, cnid, location, self.client.now()?)
    }

    pub fn set_icon_location_at(self, volume: VolumeId, cnid: Cnid,
                                location: Option<mac_abi::Point>, when: Timestamp)
                                -> Result<CatalogInfo>
    {
        if cnid.0 < ROOT_ID.0 || location.is_some_and(|p| p.h == -1 && p.v == -1)
        {
            return Err(Error::BadArgument);
        }
        let packed = location.map_or(0, |p| u32::from(p.h as u16) | (u32::from(p.v as u16) << 16));
        self.call(FileCall { operation: SET_ICON_LOCATION,
                             volume: volume.0,
                             cnid: cnid.0,
                             index: packed,
                             fork: u32::from(location.is_some()),
                             position: when.mac_microseconds().ok_or(Error::BadArgument)?,
                             ..Default::default() },
                  "")?
            .entry
            .try_into()
    }

    /// Use the System clock. Use `move_to_trash_at` when preserving an explicit date.
    pub fn move_to_trash(self, volume: VolumeId, cnid: Cnid) -> Result<CatalogInfo>
    {
        self.move_to_trash_at(volume, cnid, self.client.now()?)
    }

    /// Use the System clock. Use `put_away_at` when preserving an explicit date.
    pub fn put_away(self, volume: VolumeId, cnid: Cnid) -> Result<CatalogInfo>
    {
        self.put_away_at(volume, cnid, self.client.now()?)
    }

    /// Use the System clock. Use `empty_trash_at` when preserving an explicit date.
    pub fn empty_trash(self, volume: VolumeId) -> Result<u32>
    {
        self.empty_trash_at(volume, self.client.now()?)
    }

    pub fn volume_count(self) -> Result<u32>
    {
        Ok(self.call(FileCall { operation: VOLUME_COUNT,
                                ..Default::default() },
                     "")?
               .index)
    }

    /// Exports explicitly offered by the VM host. No host path is inferred.
    pub fn host_export_count(self) -> Result<u32>
    {
        Ok(self.call(FileCall { operation: HOST_EXPORT_COUNT,
                                ..Default::default() },
                     "")?
               .index)
    }

    /// Mount an explicit host export through the resident filesystem driver.
    /// An already-mounted export returns the existing mounted volume identity.
    pub fn mount_host_export(self, index: u32) -> Result<VolumeInfo>
    {
        self.call(FileCall { operation: MOUNT_HOST_EXPORT,
                             index,
                             ..Default::default() },
                  "")?
            .volume_info
            .try_into()
    }

    /// Unmount a provider volume after all forks/transactions have closed.
    pub fn unmount(self, volume: VolumeId) -> Result<()>
    {
        self.call(FileCall { operation: UNMOUNT_VOLUME,
                             volume: volume.0,
                             ..Default::default() },
                  "")
            .map(|_| ())
    }

    /// Mounted-volume enumeration is zero based. IDs remain stable for this boot.
    pub fn volume_at(self, index: u32) -> Result<VolumeInfo>
    {
        self.call(FileCall { operation: VOLUME_INDEX,
                             index,
                             ..Default::default() },
                  "")?
            .volume_info
            .try_into()
    }

    pub fn volume(self, id: VolumeId) -> Result<VolumeInfo>
    {
        self.call(FileCall { operation: VOLUME_INFO,
                             volume: id.0,
                             ..Default::default() },
                  "")?
            .volume_info
            .try_into()
    }

    /// The filesystem provider validates the container before publication.
    /// Source identity is volume/CNID; repeating a live mount reuses its volume.
    pub fn mount_file(self, volume: VolumeId, file: Cnid) -> Result<VolumeInfo>
    {
        self.call(FileCall { operation: MOUNT_FILE,
                             volume: volume.0,
                             cnid: file.0,
                             ..Default::default() },
                  "")?
            .volume_info
            .try_into()
    }

    /// Ask System to dispatch a registered file-code/extension action. The
    /// association belongs to the loaded driver, and disappears with it.
    pub fn open_item(self, volume: VolumeId, file: Cnid) -> Result<Option<VolumeInfo>>
    {
        let reply = self.call(FileCall { operation: OPEN_ITEM,
                                         volume: volume.0,
                                         cnid: file.0,
                                         ..Default::default() },
                              "")?;
        match reply.index
        {
            0 => Ok(None),
            1 => Ok(Some(reply.volume_info.try_into()?)),
            _ => Err(Error::BadAbi)
        }
    }

    pub fn catalog(self, volume: VolumeId, cnid: Cnid) -> Result<CatalogInfo>
    {
        self.call(FileCall { operation: CATALOG,
                             volume: volume.0,
                             cnid: cnid.0,
                             ..Default::default() },
                  "")?
            .entry
            .try_into()
    }

    /// Exact bytes allocated to each fork, including preallocation. Directories
    /// return Some(0); this does not recursively total their descendants. A
    /// provider that cannot measure physical allocation returns None.
    pub fn allocation_info(self, volume: VolumeId, cnid: Cnid) -> Result<AllocationInfo>
    {
        let call = self.call(FileCall { operation: ALLOCATION_INFO,
                                        volume: volume.0,
                                        cnid: cnid.0,
                                        ..Default::default() },
                             "")?;
        Ok(AllocationInfo { data_bytes:
                                (call.position != ALLOCATION_UNKNOWN).then_some(call.position),
                            resource_bytes:
                                (call.length != ALLOCATION_UNKNOWN).then_some(call.length) })
    }

    /// Rename one catalog item without changing its identity or fork contents.
    /// Classic HFS requires 1..31 representable MacRoman bytes, without colon/NUL.
    /// Locked/protected items and items with open forks are rejected explicitly.
    pub fn rename_at(self, volume: VolumeId, cnid: Cnid, name: &str, when: Timestamp)
                     -> Result<CatalogInfo>
    {
        self.mutation(RENAME, volume, cnid, name, when)?.entry.try_into()
    }

    /// Change a file's catalog lock while preserving all other catalog flags.
    /// Directories have no file-lock control in this native profile.
    pub fn set_locked_at(self, volume: VolumeId, cnid: Cnid, locked: bool, when: Timestamp)
                         -> Result<CatalogInfo>
    {
        self.set_flag(SET_LOCKED, volume, cnid, locked, when)
    }

    /// Change only the file-only Finder stationery flag 0x0800.
    pub fn set_stationery_at(self, volume: VolumeId, cnid: Cnid, stationery: bool, when: Timestamp)
                             -> Result<CatalogInfo>
    {
        self.set_flag(SET_STATIONERY, volume, cnid, stationery, when)
    }

    fn set_flag(self, operation: u32, volume: VolumeId, cnid: Cnid, value: bool, when: Timestamp)
                -> Result<CatalogInfo>
    {
        let time = when.mac_microseconds().ok_or(Error::BadArgument)?;
        self.call(FileCall { operation,
                             volume: volume.0,
                             cnid: cnid.0,
                             index: u32::from(value),
                             position: time,
                             ..Default::default() },
                  "")?
            .entry
            .try_into()
    }

    /// Write the file/folder's Desktop DB comment as UTF8 to an application
    /// string writer. Missing comment records return an empty string.
    pub fn comment(self, volume: VolumeId, cnid: Cnid, output: &mut impl core::fmt::Write)
                   -> Result<()>
    {
        let mut call = FileCall { operation: COMMENT,
                                  volume: volume.0,
                                  cnid: cnid.0,
                                  ..Default::default() };
        let mut bytes = [0; 768];
        self.client.file(&mut call, "", &mut bytes)?;
        let text = core::str::from_utf8(bytes.get(..call.transferred).ok_or(Error::BadArgument)?)
            .map_err(|_| Error::BadArgument)?;
        output.write_str(text).map_err(|_| Error::NoMemory)
    }

    /// Persist a Desktop DB comment. Empty text deletes only that comment key.
    /// Storage is limited to255 losslessly representable MacRoman bytes.
    pub fn set_comment_at(self, volume: VolumeId, cnid: Cnid, comment: &str, when: Timestamp)
                          -> Result<()>
    {
        self.mutation(SET_COMMENT, volume, cnid, comment, when).map(|_| ())
    }

    pub fn child(self, volume: VolumeId, directory: Cnid, index: u32) -> Result<CatalogInfo>
    {
        self.call(FileCall { operation: DIRECTORY_ENTRY,
                             volume: volume.0,
                             cnid: directory.0,
                             index,
                             ..Default::default() },
                  "")?
            .entry
            .try_into()
    }

    /// Alias for direct-child enumeration, matching catalog entry terminology.
    pub fn entry(self, volume: VolumeId, directory: Cnid, index: u32) -> Result<CatalogInfo>
    {
        self.child(volume, directory, index)
    }

    /// Resolve a UTF8 slash path relative to a directory, or from root with '/'.
    /// '.' and '..' are accepted. A CNID can address names containing literal '/'.
    pub fn lookup(self, volume: VolumeId, directory: Cnid, path: &str) -> Result<CatalogInfo>
    {
        self.call(FileCall { operation: LOOKUP,
                             volume: volume.0,
                             cnid: directory.0,
                             ..Default::default() },
                  path)?
            .entry
            .try_into()
    }

    pub fn parent(self, volume: VolumeId, cnid: Cnid) -> Result<CatalogInfo>
    {
        self.call(FileCall { operation: PARENT,
                             volume: volume.0,
                             cnid: cnid.0,
                             ..Default::default() },
                  "")?
            .entry
            .try_into()
    }

    /// Write a native UTF-8 path to a Rust string writer without requiring an
    /// application allocator. `String` and fixed-capacity fmt::Write buffers work.
    pub fn path(self, volume: VolumeId, cnid: Cnid, output: &mut impl core::fmt::Write)
                -> Result<()>
    {
        let mut call = FileCall { operation: PATH,
                                  volume: volume.0,
                                  cnid: cnid.0,
                                  ..Default::default() };
        let mut bytes = [0; 4096];
        self.client.file(&mut call, "", &mut bytes)?;
        let text=core::str::from_utf8(bytes.get(..call.transferred).ok_or(Error::BadArgument)?).map_err(|_|Error::BadArgument)?;
        output.write_str(text).map_err(|_| Error::NoMemory)
    }

    fn mutation(self, operation: u32, volume: VolumeId, cnid: Cnid, name: &str, when: Timestamp)
                -> Result<FileCall>
    {
        let time = when.mac_microseconds().ok_or(Error::BadArgument)?;
        self.call(FileCall { operation,
                             volume: volume.0,
                             cnid: cnid.0,
                             position: time,
                             ..Default::default() },
                  name)
    }

    pub fn new_folder_at(self, volume: VolumeId, parent: Cnid, name: &str, when: Timestamp)
                         -> Result<CatalogInfo>
    {
        self.mutation(NEW_FOLDER, volume, parent, name, when)?.entry.try_into()
    }

    pub fn trash_directory(self, volume: VolumeId, create: bool) -> Result<CatalogInfo>
    {
        let when = if create { self.client.now()? } else { Timestamp::from_mac_seconds(0) };
        self.trash_directory_at(volume, create, when)
    }

    pub fn trash_directory_at(self, volume: VolumeId, create: bool, when: Timestamp)
                              -> Result<CatalogInfo>
    {
        let time = when.mac_microseconds().ok_or(Error::BadArgument)?;
        self.call(FileCall { operation: TRASH_DIRECTORY,
                             volume: volume.0,
                             index: u32::from(create),
                             position: time,
                             ..Default::default() },
                  "")?
            .entry
            .try_into()
    }

    pub fn move_to_trash_at(self, volume: VolumeId, cnid: Cnid, when: Timestamp)
                            -> Result<CatalogInfo>
    {
        self.mutation(MOVE_TO_TRASH, volume, cnid, "", when)?.entry.try_into()
    }

    pub fn put_away_at(self, volume: VolumeId, cnid: Cnid, when: Timestamp) -> Result<CatalogInfo>
    {
        self.mutation(PUT_AWAY, volume, cnid, "", when)?.entry.try_into()
    }

    pub fn trash_usage(self, volume: VolumeId) -> Result<TrashUsage>
    {
        let call = self.call(FileCall { operation: TRASH_USAGE,
                                        volume: volume.0,
                                        ..Default::default() },
                             "")?;
        Ok(TrashUsage { items:           call.index,
                        allocated_bytes: call.length })
    }

    pub fn empty_trash_at(self, volume: VolumeId, when: Timestamp) -> Result<u32>
    {
        Ok(self.mutation(EMPTY_TRASH, volume, Cnid::UNSAVED, "", when)?.index)
    }

    pub fn open(self, volume: VolumeId, cnid: Cnid, fork: Fork) -> Result<OpenFile<'a>>
    {
        let call = self.call(FileCall { operation: OPEN,
                                        volume: volume.0,
                                        cnid: cnid.0,
                                        fork: fork as u32,
                                        ..Default::default() },
                             "")?;
        Ok(OpenFile { client: self.client,
                      handle: FileHandle(call.handle) })
    }
}
#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub enum Fork
{
    Data     = DATA_FORK,
    Resource = RESOURCE_FORK
}
#[derive(Clone, Copy, Debug)]
pub enum SeekFrom
{
    Start(u64),
    Current(i64),
    End(i64)
}
/// An exclusive logical fork mark. Close explicitly to observe close errors;
/// dropping also releases the resident slot. No payload is retained in the app.
pub struct OpenFile<'a>
{
    client: Client<'a>,
    handle: FileHandle
}
impl OpenFile<'_>
{
    /// Read up to the output length. A successful call may return fewer bytes;
    /// provider-backed reads are bounded to 64 KiB per call. Zero indicates EOF
    /// unless the output itself is empty.
    pub fn read(&mut self, output: &mut [u8]) -> Result<usize>
    {
        let mut call = FileCall { operation: READ,
                                  handle: self.handle.0,
                                  ..Default::default() };
        self.client.file(&mut call, "", output)?;
        Ok(call.transferred)
    }

    pub fn seek(&mut self, from: SeekFrom) -> Result<u64>
    {
        let (whence, offset) = match from
        {
            SeekFrom::Start(n) => (SEEK_START, i64::try_from(n).map_err(|_| Error::BadArgument)?),
            SeekFrom::Current(n) => (SEEK_CURRENT, n),
            SeekFrom::End(n) => (SEEK_END, n)
        };
        let mut call = FileCall { operation: SEEK,
                                  handle: self.handle.0,
                                  whence,
                                  offset,
                                  ..Default::default() };
        self.client.file(&mut call, "", &mut [])?;
        Ok(call.position)
    }

    pub fn position(&self) -> Result<u64>
    {
        let mut call = FileCall { operation: TELL,
                                  handle: self.handle.0,
                                  ..Default::default() };
        self.client.file(&mut call, "", &mut [])?;
        Ok(call.position)
    }

    pub fn len(&self) -> Result<u64>
    {
        let mut call = FileCall { operation: TELL,
                                  handle: self.handle.0,
                                  ..Default::default() };
        self.client.file(&mut call, "", &mut [])?;
        Ok(call.length)
    }

    pub fn is_empty(&self) -> Result<bool>
    {
        Ok(self.len()? == 0)
    }

    pub fn close(&mut self) -> Result<()>
    {
        if self.handle.0 == 0
        {
            return Ok(());
        }
        let mut call = FileCall { operation: CLOSE,
                                  handle: self.handle.0,
                                  ..Default::default() };
        self.client.file(&mut call, "", &mut [])?;
        self.handle = FileHandle(0);
        Ok(())
    }
}
impl Drop for OpenFile<'_>
{
    fn drop(&mut self)
    {
        if self.handle.0 != 0
        {
            let mut call = FileCall { operation: CLOSE,
                                      handle: self.handle.0,
                                      ..Default::default() };
            let _ = self.client.file(&mut call, "", &mut []);
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrashUsage
{
    pub items:           u32,
    pub allocated_bytes: u64
}

/// Physical fork allocation, distinct from logical lengths. None means the
/// mounted provider cannot report it; it never means a zero-byte allocation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocationInfo
{
    pub data_bytes:     Option<u64>,
    pub resource_bytes: Option<u64>
}
impl AllocationInfo
{
    pub fn total_bytes(self) -> Result<Option<u64>>
    {
        match (self.data_bytes, self.resource_bytes)
        {
            (Some(data), Some(resource)) =>
            {
                data.checked_add(resource).map(Some).ok_or(Error::BadArgument)
            }
            _ => Ok(None)
        }
    }
}

/// A staged copy-on-write ordinary data file. Drop aborts an unfinished write.
/// Only finish publishes a catalog entry; existing resource fork bytes survive.
pub struct DataWriter<'a>
{
    client: Client<'a>,
    handle: FileWriteHandle
}
/// Destination and metadata for one atomic ordinary data-fork save.
#[derive(Clone, Copy)]
pub struct DataFileSpec<'a>
{
    pub volume:    VolumeId,
    pub parent:    Cnid,
    pub name:      &'a str,
    /// None creates; Some(id) replaces only that exact confirmed catalog item.
    pub replacing: Option<Cnid>,
    pub file_type: TypeCode,
    pub creator:   TypeCode
}
impl<'a> Files<'a>
{
    pub fn create_data(self, spec: DataFileSpec<'_>, length: usize) -> Result<DataWriter<'a>>
    {
        self.create_data_at(spec, length, self.client.now()?)
    }

    /// Preserve an explicit creation/modification date, such as when copying a file.
    pub fn create_data_at(self, spec: DataFileSpec<'_>, length: usize, when: Timestamp)
                          -> Result<DataWriter<'a>>
    {
        let mut call = DataWriteCall { operation: BEGIN_DATA_WRITE,
                                       volume: spec.volume.0,
                                       parent: spec.parent.0,
                                       expected_cnid: spec.replacing.unwrap_or(Cnid::UNSAVED).0,
                                       file_type: spec.file_type.bytes(),
                                       creator: spec.creator.bytes(),
                                       length,
                                       when: when.mac_microseconds()
                                                 .ok_or(Error::BadArgument)?,
                                       ..Default::default() };
        self.client.data_write(&mut call, spec.name, &[])?;
        Ok(DataWriter { client: self.client,
                        handle: FileWriteHandle(call.handle) })
    }
}
impl DataWriter<'_>
{
    pub fn write_all(&mut self, data: &[u8]) -> Result<()>
    {
        for chunk in data.chunks(64 * 1024)
        {
            let mut call = DataWriteCall { operation: APPEND_DATA_WRITE,
                                           handle: self.handle.0,
                                           ..Default::default() };
            self.client.data_write(&mut call, "", chunk)?;
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Result<CatalogInfo>
    {
        let mut call = DataWriteCall { operation: COMMIT_DATA_WRITE,
                                       handle: self.handle.0,
                                       ..Default::default() };
        self.client.data_write(&mut call, "", &[])?;
        self.handle = FileWriteHandle(0);
        call.entry.try_into()
    }

    pub fn abort(&mut self) -> Result<()>
    {
        let mut call = DataWriteCall { operation: ABORT_DATA_WRITE,
                                       handle: self.handle.0,
                                       ..Default::default() };
        self.client.data_write(&mut call, "", &[])?;
        self.handle = FileWriteHandle(0);
        Ok(())
    }
}
impl Drop for DataWriter<'_>
{
    fn drop(&mut self)
    {
        if self.handle.0 != 0
        {
            let mut call = DataWriteCall { operation: ABORT_DATA_WRITE,
                                           handle: self.handle.0,
                                           ..Default::default() };
            let _ = self.client.data_write(&mut call, "", &[]);
        }
    }
}
