//! Opening a physical medium for writing.
//!
//! This is the one platform-specific step in flashing, kept separate so no
//! raw-device call appears in the command handler and so the write and verify
//! logic above it stays testable against an ordinary file.
//!
//! On Windows a physical drive is a raw device path, not a file: it must be
//! opened unbuffered, written in whole sectors, and it needs elevation. Each of
//! those failures has an OS error that says very little, so this module names
//! them instead.

use std::io::{self, Read, Seek, SeekFrom, Write};

use crate::{Result, error};

/// An opened medium: seekable, readable and writable.
pub struct Medium
{
    inner:    Box<dyn ReadWriteSeek>,
    capacity: Option<u64>
}

trait ReadWriteSeek: Read + Write + Seek {}
impl<T: Read + Write + Seek> ReadWriteSeek for T {}

impl Medium
{
    pub fn capacity(&self) -> Option<u64>
    {
        self.capacity
    }
}

impl Read for Medium
{
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize>
    {
        self.inner.read(buffer)
    }
}

impl Write for Medium
{
    fn write(&mut self, data: &[u8]) -> io::Result<usize>
    {
        self.inner.write(data)
    }

    fn flush(&mut self) -> io::Result<()>
    {
        self.inner.flush()
    }
}

impl Seek for Medium
{
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64>
    {
        self.inner.seek(position)
    }
}

/// Opens a medium the operator named.
///
/// Nothing here chooses a device. `target` is exactly what the operator passed,
/// and an ordinary file path is accepted so the same command can be exercised
/// without touching hardware.
pub fn open(target: &str) -> Result<Medium>
{
    if is_raw_device(target)
    {
        return open_raw(target);
    }
    let file = std::fs::OpenOptions::new().read(true)
                                          .write(true)
                                          .open(target)
                                          .map_err(|cause| {
                                              error(format!("cannot open {target}: {cause}"))
                                          })?;
    let capacity = file.metadata().ok().map(|metadata| metadata.len());
    Ok(Medium { inner: Box::new(file),
                capacity })
}

/// A path naming a physical device rather than a file.
pub fn is_raw_device(target: &str) -> bool
{
    let upper = target.to_ascii_uppercase();
    upper.starts_with("\\\\.\\PHYSICALDRIVE") || upper.starts_with("/DEV/")
}

#[cfg(windows)]
fn open_raw(target: &str) -> Result<Medium>
{
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
    const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;

    let file = std::fs::OpenOptions::new().read(true)
                                          .write(true)
                                          .custom_flags(FILE_FLAG_NO_BUFFERING
                                                        | FILE_FLAG_WRITE_THROUGH)
                                          .open(target)
                                          .map_err(|cause| {
                                              if cause.kind() == io::ErrorKind::PermissionDenied
                                              {
                                                  error(format!("cannot open {target}: permission \
                                                                 denied. Writing a physical drive \
                                                                 needs an elevated shell."))
                                              }
                                              else
                                              {
                                                  error(format!("cannot open {target}: {cause}"))
                                              }
                                          })?;
    Ok(Medium { inner: Box::new(file),
                capacity: None })
}

#[cfg(not(windows))]
fn open_raw(target: &str) -> Result<Medium>
{
    // Block devices need whole-sector writes and a size a plain file handle does
    // not report. Until that adapter exists, say so rather than writing through
    // a buffered handle and producing a corrupt card.
    let _ = target;
    Err(error("writing a raw block device is not implemented on this platform yet; write the \
               image to the device with the platform's own tool and verify it separately"))
}

/// Sector size for unbuffered writes on a raw device.
pub const SECTOR_BYTES: usize = 512;

/// Pads `image` up to a whole number of sectors, which is what an unbuffered raw
/// device requires. The padding is zeros and only extends the write, never the
/// image's declared length.
pub fn pad_to_sector(image: &[u8]) -> Vec<u8>
{
    let remainder = image.len() % SECTOR_BYTES;
    if remainder == 0
    {
        return image.to_vec();
    }
    let mut padded = image.to_vec();
    padded.resize(image.len() + (SECTOR_BYTES - remainder), 0);
    padded
}
