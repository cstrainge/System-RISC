//! Native canonical File Manager descriptors. Plain std files use data forks.
//! Unimplemented filesystem operations refuse explicitly; no host/libc fallback.
use mac_abi::files::*;

pub use crate::sys::fs::common::{Dir, ExtraHomeDirs, ExtraMediaDirs};
use crate::{ffi::OsString,
            fmt,
            fs::TryLockError,
            io::{self, BorrowedCursor, IoSlice, IoSliceMut, SeekFrom},
            path::{Path, PathBuf},
            sys::{pal::runtime,
                  time::{SystemTime, UNIX_EPOCH},
                  unsupported},
            time::Duration};

pub struct File
{
    handle: u32
}
#[derive(Clone)]
pub struct FileAttr(CatalogInfo);
pub struct ReadDir(!);
pub struct DirEntry(!);
#[derive(Clone, Debug, Default)]
pub struct OpenOptions
{
    flags: u32
}
#[derive(Copy, Clone, Debug, Default)]
pub struct FileTimes
{
    accessed: Option<SystemTime>,
    modified: Option<SystemTime>
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePermissions
{
    readonly: bool
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileType(u32);
#[derive(Debug)]
pub struct DirBuilder;
fn invalid(message: &'static str) -> io::Error
{
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn error(cause: mac_traps::Error) -> io::Error
{
    io::Error::from_raw_os_error(cause.status())
}
fn text(path: &Path) -> io::Result<&str>
{
    path.to_str().ok_or_else(|| invalid("System RISC paths must be valid UTF-8"))
}
fn request(mut call: FileCall, name: &str, output: &mut [u8]) -> io::Result<FileCall>
{
    runtime::client().file(&mut call, name, output).map_err(error)?;
    Ok(call)
}
fn descriptor(handle: u32, operation: u32) -> io::Result<FileCall>
{
    request(FileCall { operation,
                       handle,
                       ..Default::default() },
            "",
            &mut [])
}
fn timestamp(microseconds: u64) -> io::Result<SystemTime>
{
    let native = mac_abi::dates::Timestamp::from_mac_microseconds(microseconds);
    let seconds = native.unix_seconds();
    let nanos = Duration::from_micros(native.subsec_microseconds() as u64);
    let time = if seconds >= 0
    {
        UNIX_EPOCH.checked_add_duration(&Duration::from_secs(seconds as u64))
    }
    else
    {
        UNIX_EPOCH.checked_sub_duration(&Duration::from_secs(seconds.unsigned_abs()))
    };
    time.and_then(|time| time.checked_add_duration(&nanos))
        .ok_or_else(|| invalid("native file timestamp exceeds SystemTime range"))
}
impl FileAttr
{
    pub fn size(&self) -> u64
    {
        self.0.data_length
    }

    pub fn perm(&self) -> FilePermissions
    {
        FilePermissions { readonly: self.0.is_locked() }
    }

    pub fn file_type(&self) -> FileType
    {
        FileType(self.0.kind)
    }

    pub fn modified(&self) -> io::Result<SystemTime>
    {
        timestamp(self.0.modified)
    }

    pub fn accessed(&self) -> io::Result<SystemTime>
    {
        unsupported()
    }

    pub fn created(&self) -> io::Result<SystemTime>
    {
        timestamp(self.0.created)
    }
}
impl FilePermissions
{
    pub fn readonly(&self) -> bool
    {
        self.readonly
    }

    pub fn set_readonly(&mut self, readonly: bool)
    {
        self.readonly = readonly;
    }
}
impl FileType
{
    pub fn is_dir(&self) -> bool
    {
        self.0 == DIRECTORY
    }

    pub fn is_file(&self) -> bool
    {
        self.0 == FILE
    }

    pub fn is_symlink(&self) -> bool
    {
        false
    }
}
impl FileTimes
{
    pub fn set_accessed(&mut self, time: SystemTime)
    {
        self.accessed = Some(time);
    }

    pub fn set_modified(&mut self, time: SystemTime)
    {
        self.modified = Some(time);
    }
}
impl fmt::Debug for ReadDir
{
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        self.0
    }
}
impl Iterator for ReadDir
{
    type Item = io::Result<DirEntry>;

    fn next(&mut self) -> Option<Self::Item>
    {
        self.0
    }
}
impl DirEntry
{
    pub fn path(&self) -> PathBuf
    {
        self.0
    }

    pub fn file_name(&self) -> OsString
    {
        self.0
    }

    pub fn metadata(&self) -> io::Result<FileAttr>
    {
        self.0
    }

    pub fn file_type(&self) -> io::Result<FileType>
    {
        self.0
    }
}
impl OpenOptions
{
    pub fn new() -> Self
    {
        Self::default()
    }

    fn flag(&mut self, flag: u32, enabled: bool)
    {
        if enabled
        {
            self.flags |= flag;
        }
        else
        {
            self.flags &= !flag;
        }
    }

    pub fn read(&mut self, value: bool)
    {
        self.flag(OPEN_READ, value);
    }

    pub fn write(&mut self, value: bool)
    {
        self.flag(OPEN_WRITE, value);
    }

    pub fn append(&mut self, value: bool)
    {
        self.flag(OPEN_APPEND, value);
    }

    pub fn truncate(&mut self, value: bool)
    {
        self.flag(OPEN_TRUNCATE, value);
    }

    pub fn create(&mut self, value: bool)
    {
        self.flag(OPEN_CREATE, value);
    }

    pub fn create_new(&mut self, value: bool)
    {
        self.flag(OPEN_CREATE_NEW, value);
    }
}
impl File
{
    pub fn open(path: &Path, options: &OpenOptions) -> io::Result<Self>
    {
        let call = request(FileCall { operation: OPEN_PATH,
                                      fork: DATA_FORK,
                                      open_flags: options.flags,
                                      ..Default::default() },
                           text(path)?,
                           &mut [])?;
        if call.handle == 0
        {
            return Err(io::const_error!(io::ErrorKind::InvalidData,
                                        "native open returned no canonical descriptor"));
        }
        Ok(Self { handle: call.handle })
    }

    pub fn file_attr(&self) -> io::Result<FileAttr>
    {
        let entry = descriptor(self.handle, TELL)?.entry;
        if entry.id == 0
        {
            return Err(io::const_error!(io::ErrorKind::InvalidData,
                                        "native descriptor returned no catalog identity"));
        }
        Ok(FileAttr(entry))
    }

    pub fn fsync(&self) -> io::Result<()>
    {
        descriptor(self.handle, SYNC).map(|_| ())
    }

    pub fn datasync(&self) -> io::Result<()>
    {
        request(FileCall { operation: SYNC,
                           handle: self.handle,
                           index: SYNC_DATA,
                           ..Default::default() },
                "",
                &mut []).map(|_| ())
    }

    pub fn lock(&self) -> io::Result<()>
    {
        unsupported()
    }

    pub fn lock_shared(&self) -> io::Result<()>
    {
        unsupported()
    }

    pub fn try_lock(&self) -> Result<(), TryLockError>
    {
        Err(TryLockError::Error(crate::sys::unsupported_err()))
    }

    pub fn try_lock_shared(&self) -> Result<(), TryLockError>
    {
        Err(TryLockError::Error(crate::sys::unsupported_err()))
    }

    pub fn unlock(&self) -> io::Result<()>
    {
        unsupported()
    }

    pub fn truncate(&self, size: u64) -> io::Result<()>
    {
        request(FileCall { operation: SET_LENGTH,
                           handle: self.handle,
                           length: size,
                           ..Default::default() },
                "",
                &mut []).map(|_| ())
    }

    pub fn read(&self, buffer: &mut [u8]) -> io::Result<usize>
    {
        if buffer.is_empty()
        {
            return Ok(0);
        }
        let count = buffer.len().min(MAX_FILE_TRANSFER);
        request(FileCall { operation: READ,
                           handle: self.handle,
                           ..Default::default() },
                "",
                &mut buffer[..count]).and_then(|call| {
                                         if call.transferred <= count
                                         {
                                             Ok(call.transferred)
                                         }
                                         else
                                         {
                                             Err(io::const_error!(io::ErrorKind::InvalidData,
                                                                  "native read exceeded its \
                                                                   output span"))
                                         }
                                     })
    }

    pub fn read_vectored(&self, buffers: &mut [IoSliceMut<'_>]) -> io::Result<usize>
    {
        io::default_read_vectored(|bytes| self.read(bytes), buffers)
    }

    pub fn is_read_vectored(&self) -> bool
    {
        false
    }

    pub fn read_buf(&self, mut cursor: BorrowedCursor<'_, u8>) -> io::Result<()>
    {
        if cursor.capacity() == 0
        {
            return Ok(());
        }
        let mut call = FileCall { handle: self.handle,
                                  ..Default::default() };
        // SAFETY: the transport only writes this borrowed uninitialized span;
        // it does not observe or deinitialize any existing initialized bytes.
        let count = runtime::client().file_read_uninit(&mut call, unsafe { cursor.as_mut() })
                                     .map_err(error)?;
        // SAFETY: a successful native READ initializes exactly this checked
        // prefix. No cursor advancement occurs on service failure.
        unsafe {
            cursor.advance(count);
        }
        Ok(())
    }

    pub fn write(&self, buffer: &[u8]) -> io::Result<usize>
    {
        if buffer.is_empty()
        {
            return Ok(0);
        }
        let mut call = FileCall { handle: self.handle,
                                  ..Default::default() };
        runtime::client().file_write(&mut call, buffer).map_err(error)?;
        if call.transferred > buffer.len().min(MAX_FILE_TRANSFER)
        {
            return Err(io::const_error!(io::ErrorKind::InvalidData,
                                        "native write exceeded its admitted input"));
        }
        Ok(call.transferred)
    }

    pub fn write_vectored(&self, buffers: &[IoSlice<'_>]) -> io::Result<usize>
    {
        io::default_write_vectored(|bytes| self.write(bytes), buffers)
    }

    pub fn is_write_vectored(&self) -> bool
    {
        false
    }

    /// File writes have no userspace buffer. Persistence uses sync_all/sync_data.
    pub fn flush(&self) -> io::Result<()>
    {
        Ok(())
    }

    pub fn seek(&self, position: SeekFrom) -> io::Result<u64>
    {
        let (whence, offset) = match position
        {
            SeekFrom::Start(value) =>
            {
                (SEEK_START,
                 i64::try_from(value).map_err(|_| invalid("native seek start exceeds i64::MAX"))?)
            }
            SeekFrom::Current(value) => (SEEK_CURRENT, value),
            SeekFrom::End(value) => (SEEK_END, value)
        };
        request(FileCall { operation: SEEK,
                           handle: self.handle,
                           whence,
                           offset,
                           ..Default::default() },
                "",
                &mut []).map(|call| call.position)
    }

    pub fn size(&self) -> Option<io::Result<u64>>
    {
        Some(descriptor(self.handle, TELL).map(|call| call.length))
    }

    pub fn tell(&self) -> io::Result<u64>
    {
        descriptor(self.handle, TELL).map(|call| call.position)
    }

    pub fn duplicate(&self) -> io::Result<Self>
    {
        Ok(Self { handle: descriptor(self.handle, DUPLICATE)?.handle })
    }

    pub fn set_permissions(&self, _: FilePermissions) -> io::Result<()>
    {
        unsupported()
    }

    pub fn set_times(&self, _: FileTimes) -> io::Result<()>
    {
        unsupported()
    }
}
impl Drop for File
{
    fn drop(&mut self)
    {
        let _ = descriptor(self.handle, CLOSE);
    }
}
impl fmt::Debug for File
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.debug_struct("File").field("handle", &self.handle).finish()
    }
}
impl DirBuilder
{
    pub fn new() -> Self
    {
        Self
    }

    pub fn mkdir(&self, _: &Path) -> io::Result<()>
    {
        unsupported()
    }
}
pub fn readdir(_: &Path) -> io::Result<ReadDir>
{
    unsupported()
}
pub fn unlink(_: &Path) -> io::Result<()>
{
    unsupported()
}
pub fn rename(_: &Path, _: &Path) -> io::Result<()>
{
    unsupported()
}
pub fn set_perm(_: &Path, _: FilePermissions) -> io::Result<()>
{
    unsupported()
}
pub fn set_perm_nofollow(_: &Path, _: FilePermissions) -> io::Result<()>
{
    unsupported()
}
pub fn set_times(_: &Path, _: FileTimes) -> io::Result<()>
{
    unsupported()
}
pub fn set_times_nofollow(_: &Path, _: FileTimes) -> io::Result<()>
{
    unsupported()
}
pub fn rmdir(_: &Path) -> io::Result<()>
{
    unsupported()
}
pub fn remove_dir_all(_: &Path) -> io::Result<()>
{
    unsupported()
}
pub fn exists(path: &Path) -> io::Result<bool>
{
    match stat(path)
    {
        Ok(_) => Ok(true),
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(cause) => Err(cause)
    }
}
pub fn readlink(_: &Path) -> io::Result<PathBuf>
{
    unsupported()
}
pub fn symlink(_: &Path, _: &Path) -> io::Result<()>
{
    unsupported()
}
pub fn link(_: &Path, _: &Path) -> io::Result<()>
{
    unsupported()
}
pub fn stat(path: &Path) -> io::Result<FileAttr>
{
    request(FileCall { operation: RESOLVE_PATH,
                       ..Default::default() },
            text(path)?,
            &mut []).map(|call| FileAttr(call.entry))
}
pub fn lstat(path: &Path) -> io::Result<FileAttr>
{
    stat(path)
}
pub fn canonicalize(_: &Path) -> io::Result<PathBuf>
{
    unsupported()
}
pub fn copy(_: &Path, _: &Path) -> io::Result<u64>
{
    unsupported()
}
