//! Native catalog snapshots and bounded directory enumeration through Toolbox.
//! No Unix permissions, pathname guessing, or unsupported std directory calls.
use std::{ffi::OsString,
          io,
          path::{Path, PathBuf},
          time::{Duration, SystemTime, UNIX_EPOCH}};

use mac_toolbox::{System,
                  files::{CatalogFlags, CatalogInfo, DirectoryCursor, EntryKind, ScanStep}};

#[derive(Clone, Copy)]
pub struct Metadata(CatalogInfo);
impl Metadata
{
    pub fn catalog(&self) -> &CatalogInfo
    {
        &self.0
    }

    pub fn is_dir(&self) -> bool
    {
        self.0.kind == EntryKind::Directory
    }

    pub fn is_locked(&self) -> bool
    {
        self.0.flags.contains(CatalogFlags::LOCKED)
    }

    pub fn is_application(&self) -> bool
    {
        !self.is_dir() && matches!(&self.0.file_type.bytes(), b"APPL" | b"CLAP")
    }

    pub fn len(&self) -> u64
    {
        self.0.data_length
    }

    pub fn modified(&self) -> io::Result<SystemTime>
    {
        let seconds = self.0.modified.unix_seconds();
        let whole = Duration::from_secs(seconds.unsigned_abs());
        let time =
            if seconds < 0 { UNIX_EPOCH.checked_sub(whole) } else { UNIX_EPOCH.checked_add(whole) };
        time.and_then(|value| {
                value.checked_add(Duration::from_micros(self.0.modified.subsec_microseconds()
                                                        as u64))
            })
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "file timestamp out of range")
            })
    }
}

fn error(cause: mac_toolbox::Error) -> io::Error
{
    io::Error::other(cause)
}

pub fn metadata(path: &Path) -> io::Result<Metadata>
{
    let path =
        path.to_str()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "System RISC paths must be UTF-8")
            })?;
    System::new().map_err(error)?.files().resolve_path(path).map(Metadata).map_err(error)
}

pub struct DirEntry
{
    path:     PathBuf,
    metadata: Metadata
}
impl DirEntry
{
    pub fn path(&self) -> PathBuf
    {
        self.path.clone()
    }

    pub fn file_name(&self) -> OsString
    {
        OsString::from(self.metadata.0.name.as_str())
    }

    pub fn metadata(&self) -> io::Result<Metadata>
    {
        Ok(self.metadata)
    }
}

pub struct ReadDir
{
    system:    System<'static>,
    directory: CatalogInfo,
    path:      PathBuf,
    cursor:    DirectoryCursor,
    done:      bool
}
pub fn read_dir(path: &Path) -> io::Result<ReadDir>
{
    let directory = metadata(path)?.0;
    if directory.kind != EntryKind::Directory
    {
        return Err(io::Error::new(io::ErrorKind::NotADirectory, "not a directory"));
    }
    Ok(ReadDir { system: System::new().map_err(error)?,
                 directory,
                 path: path.to_path_buf(),
                 cursor: DirectoryCursor::default(),
                 done: false })
}
impl Iterator for ReadDir
{
    type Item = io::Result<DirEntry>;

    fn next(&mut self) -> Option<Self::Item>
    {
        while !self.done
        {
            match self.system
                      .files()
                      .scan_directory(self.directory.volume, self.directory.id, &mut self.cursor)
            {
                Ok(ScanStep::Entry { catalog,
                                     .. }) =>
                {
                    return Some(Ok(DirEntry { path:     self.path.join(catalog.name.as_str()),
                                              metadata: Metadata(catalog) }));
                }
                Ok(ScanStep::Pending) => std::thread::yield_now(),
                Ok(ScanStep::Done) => self.done = true,
                Err(cause) =>
                {
                    self.done = true;
                    return Some(Err(error(cause)));
                }
            }
        }
        None
    }
}
