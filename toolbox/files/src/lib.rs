//! Safe File Manager clients. Storage and HFS mutation engines live in System.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::files::*;
mod scan;
pub use mac_abi::{dates::Timestamp,
                  files::{COMMENT_MAX_BYTES, CatalogInfo, ROOT_ID, VolumeInfo}};
pub use mac_traps::{Client, Error, Result};
pub use scan::{DirectoryCursor, SCAN_STALE, ScanStep};
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

    pub fn volume_count(self) -> Result<u32>
    {
        Ok(self.call(FileCall { operation: VOLUME_COUNT,
                                ..Default::default() },
                     "")?
               .index)
    }

    /// Mounted-volume enumeration is zero based. IDs remain stable for this boot.
    pub fn volume_at(self, index: u32) -> Result<VolumeInfo>
    {
        Ok(self.call(FileCall { operation: VOLUME_INDEX,
                                index,
                                ..Default::default() },
                     "")?
               .volume_info)
    }

    pub fn volume(self, id: u32) -> Result<VolumeInfo>
    {
        Ok(self.call(FileCall { operation: VOLUME_INFO,
                                volume: id,
                                ..Default::default() },
                     "")?
               .volume_info)
    }

    pub fn catalog(self, volume: u32, cnid: u32) -> Result<CatalogInfo>
    {
        Ok(self.call(FileCall { operation: CATALOG,
                                volume,
                                cnid,
                                ..Default::default() },
                     "")?
               .entry)
    }

    /// Exact bytes allocated to each fork, including preallocation. Directories
    /// return zero; this does not recursively total their descendants.
    pub fn allocation_info(self, volume: u32, cnid: u32) -> Result<AllocationInfo>
    {
        let call = self.call(FileCall { operation: ALLOCATION_INFO,
                                        volume,
                                        cnid,
                                        ..Default::default() },
                             "")?;
        Ok(AllocationInfo { data_bytes:     call.position,
                            resource_bytes: call.length })
    }

    /// Rename one catalog item without changing its identity or fork contents.
    /// Classic HFS requires1..31 representable MacRoman bytes, without colon/NUL.
    /// Locked/protected items and items with open forks are rejected explicitly.
    pub fn rename(self, volume: u32, cnid: u32, name: &str, when: Timestamp)
                  -> Result<CatalogInfo>
    {
        Ok(self.mutation(RENAME, volume, cnid, name, when)?.entry)
    }

    /// Change a file's catalog lock while preserving all other catalog flags.
    /// Directories have no file-lock control in this native profile.
    pub fn set_locked(self, volume: u32, cnid: u32, locked: bool, when: Timestamp)
                      -> Result<CatalogInfo>
    {
        self.set_flag(SET_LOCKED, volume, cnid, locked, when)
    }

    /// Change only the file-only Finder stationery flag0x0800.
    pub fn set_stationery(self, volume: u32, cnid: u32, stationery: bool, when: Timestamp)
                          -> Result<CatalogInfo>
    {
        self.set_flag(SET_STATIONERY, volume, cnid, stationery, when)
    }

    fn set_flag(self, operation: u32, volume: u32, cnid: u32, value: bool, when: Timestamp)
                -> Result<CatalogInfo>
    {
        let time = when.mac_seconds().ok_or(Error(mac_abi::BAD_ARGUMENT))?;
        Ok(self.call(FileCall { operation,
                                volume,
                                cnid,
                                index: u32::from(value),
                                position: u64::from(time),
                                ..Default::default() },
                     "")?
               .entry)
    }

    /// Write the file/folder's Desktop DB comment as UTF8 to an application
    /// string writer. Missing comment records return an empty string.
    pub fn comment(self, volume: u32, cnid: u32, output: &mut impl core::fmt::Write) -> Result<()>
    {
        let mut call = FileCall { operation: COMMENT,
                                  volume,
                                  cnid,
                                  ..Default::default() };
        let mut bytes = [0; 768];
        self.client.file(&mut call, "", &mut bytes)?;
        let text = core::str::from_utf8(bytes.get(..call.transferred).ok_or(Error(mac_abi::BAD_ARGUMENT))?)
            .map_err(|_| Error(mac_abi::BAD_ARGUMENT))?;
        output.write_str(text).map_err(|_| Error(mac_abi::NO_MEMORY))
    }

    /// Persist a Desktop DB comment. Empty text deletes only that comment key.
    /// Storage is limited to255 losslessly representable MacRoman bytes.
    pub fn set_comment(self, volume: u32, cnid: u32, comment: &str, when: Timestamp) -> Result<()>
    {
        self.mutation(SET_COMMENT, volume, cnid, comment, when).map(|_| ())
    }

    pub fn child(self, volume: u32, directory: u32, index: u32) -> Result<CatalogInfo>
    {
        Ok(self.call(FileCall { operation: DIRECTORY_ENTRY,
                                volume,
                                cnid: directory,
                                index,
                                ..Default::default() },
                     "")?
               .entry)
    }

    /// Alias for direct-child enumeration, matching catalog entry terminology.
    pub fn entry(self, volume: u32, directory: u32, index: u32) -> Result<CatalogInfo>
    {
        self.child(volume, directory, index)
    }

    /// Resolve a UTF8 slash path relative to a directory, or from root with '/'.
    /// '.' and '..' are accepted. A CNID can address names containing literal '/'.
    pub fn lookup(self, volume: u32, directory: u32, path: &str) -> Result<CatalogInfo>
    {
        Ok(self.call(FileCall { operation: LOOKUP,
                                volume,
                                cnid: directory,
                                ..Default::default() },
                     path)?
               .entry)
    }

    pub fn parent(self, volume: u32, cnid: u32) -> Result<CatalogInfo>
    {
        Ok(self.call(FileCall { operation: PARENT,
                                volume,
                                cnid,
                                ..Default::default() },
                     "")?
               .entry)
    }

    /// Write a native UTF-8 path to a Rust string writer without requiring an
    /// application allocator. `String` and fixed-capacity fmt::Write buffers work.
    pub fn path(self, volume: u32, cnid: u32, output: &mut impl core::fmt::Write) -> Result<()>
    {
        let mut call = FileCall { operation: PATH,
                                  volume,
                                  cnid,
                                  ..Default::default() };
        let mut bytes = [0; 4096];
        self.client.file(&mut call, "", &mut bytes)?;
        let text=core::str::from_utf8(bytes.get(..call.transferred).ok_or(Error(mac_abi::BAD_ARGUMENT))?).map_err(|_|Error(mac_abi::BAD_ARGUMENT))?;
        output.write_str(text).map_err(|_| Error(mac_abi::NO_MEMORY))
    }

    fn mutation(self, operation: u32, volume: u32, cnid: u32, name: &str, when: Timestamp)
                -> Result<FileCall>
    {
        let time = when.mac_seconds().ok_or(Error(mac_abi::BAD_ARGUMENT))?;
        self.call(FileCall { operation,
                             volume,
                             cnid,
                             position: time as u64,
                             ..Default::default() },
                  name)
    }

    pub fn new_folder(self, volume: u32, parent: u32, name: &str, when: Timestamp)
                      -> Result<CatalogInfo>
    {
        Ok(self.mutation(NEW_FOLDER, volume, parent, name, when)?.entry)
    }

    pub fn trash_directory(self, volume: u32, create: bool) -> Result<CatalogInfo>
    {
        self.trash_directory_at(volume, create, Timestamp::from_mac_seconds(0))
    }

    pub fn trash_directory_at(self, volume: u32, create: bool, when: Timestamp)
                              -> Result<CatalogInfo>
    {
        let time = when.mac_seconds().ok_or(Error(mac_abi::BAD_ARGUMENT))?;
        Ok(self.call(FileCall { operation: TRASH_DIRECTORY,
                                volume,
                                index: u32::from(create),
                                position: time as u64,
                                ..Default::default() },
                     "")?
               .entry)
    }

    pub fn move_to_trash(self, volume: u32, cnid: u32, when: Timestamp) -> Result<CatalogInfo>
    {
        Ok(self.mutation(MOVE_TO_TRASH, volume, cnid, "", when)?.entry)
    }

    pub fn put_away(self, volume: u32, cnid: u32, when: Timestamp) -> Result<CatalogInfo>
    {
        Ok(self.mutation(PUT_AWAY, volume, cnid, "", when)?.entry)
    }

    pub fn trash_usage(self, volume: u32) -> Result<TrashUsage>
    {
        let call = self.call(FileCall { operation: TRASH_USAGE,
                                        volume,
                                        ..Default::default() },
                             "")?;
        Ok(TrashUsage { items:           call.index,
                        allocated_bytes: call.length })
    }

    pub fn empty_trash(self, volume: u32, when: Timestamp) -> Result<u32>
    {
        Ok(self.mutation(EMPTY_TRASH, volume, 0, "", when)?.index)
    }

    pub fn open(self, volume: u32, cnid: u32, fork: Fork) -> Result<OpenFile<'a>>
    {
        let call = self.call(FileCall { operation: OPEN,
                                        volume,
                                        cnid,
                                        fork: fork as u32,
                                        ..Default::default() },
                             "")?;
        Ok(OpenFile { client: self.client,
                      handle: call.handle })
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
    handle: u32
}
impl OpenFile<'_>
{
    pub fn read(&mut self, output: &mut [u8]) -> Result<usize>
    {
        let mut call = FileCall { operation: READ,
                                  handle: self.handle,
                                  ..Default::default() };
        self.client.file(&mut call, "", output)?;
        Ok(call.transferred)
    }

    pub fn seek(&mut self, from: SeekFrom) -> Result<u64>
    {
        let (whence, offset) = match from
        {
            SeekFrom::Start(n) =>
            {
                (SEEK_START, i64::try_from(n).map_err(|_| Error(mac_abi::BAD_ARGUMENT))?)
            }
            SeekFrom::Current(n) => (SEEK_CURRENT, n),
            SeekFrom::End(n) => (SEEK_END, n)
        };
        let mut call = FileCall { operation: SEEK,
                                  handle: self.handle,
                                  whence,
                                  offset,
                                  ..Default::default() };
        self.client.file(&mut call, "", &mut [])?;
        Ok(call.position)
    }

    pub fn position(&self) -> Result<u64>
    {
        let mut call = FileCall { operation: TELL,
                                  handle: self.handle,
                                  ..Default::default() };
        self.client.file(&mut call, "", &mut [])?;
        Ok(call.position)
    }

    pub fn len(&self) -> Result<u64>
    {
        let mut call = FileCall { operation: TELL,
                                  handle: self.handle,
                                  ..Default::default() };
        self.client.file(&mut call, "", &mut [])?;
        Ok(call.length)
    }

    pub fn is_empty(&self) -> Result<bool>
    {
        Ok(self.len()? == 0)
    }

    pub fn close(mut self) -> Result<()>
    {
        let mut call = FileCall { operation: CLOSE,
                                  handle: self.handle,
                                  ..Default::default() };
        self.client.file(&mut call, "", &mut [])?;
        self.handle = 0;
        Ok(())
    }
}
impl Drop for OpenFile<'_>
{
    fn drop(&mut self)
    {
        if self.handle != 0
        {
            let mut call = FileCall { operation: CLOSE,
                                      handle: self.handle,
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

/// Exact HFS physical fork lengths, distinct from CatalogInfo logical lengths.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocationInfo
{
    pub data_bytes:     u64,
    pub resource_bytes: u64
}
