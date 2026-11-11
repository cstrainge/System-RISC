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
    let capacity = drive_length(&file, target)?;
    Ok(Medium { inner: Box::new(file),
                capacity: Some(capacity) })
}

/// Asks Windows how many bytes a physical drive holds.
///
/// `IOCTL_DISK_GET_LENGTH_INFO` returns the drive's length in bytes. A raw
/// device has no file metadata to read a size from, and flashing refuses to
/// write to a target whose capacity it does not know, so without this query
/// no physical drive on Windows could ever be flashed.
#[cfg(windows)]
fn drive_length(file: &std::fs::File, target: &str) -> Result<u64>
{
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::System::{IO::DeviceIoControl,
                                     Ioctl::{GET_LENGTH_INFORMATION, IOCTL_DISK_GET_LENGTH_INFO}};

    let mut info = GET_LENGTH_INFORMATION::default();
    let mut returned: u32 = 0;
    // SAFETY: `info` is a plain repr(C) struct owned by this frame, `returned`
    // receives the byte count, and the call is synchronous (no OVERLAPPED), so
    // nothing outlives the call.
    let ok = unsafe {
        DeviceIoControl(file.as_raw_handle(),
                        IOCTL_DISK_GET_LENGTH_INFO,
                        std::ptr::null(),
                        0,
                        &mut info as *mut GET_LENGTH_INFORMATION as *mut core::ffi::c_void,
                        std::mem::size_of::<GET_LENGTH_INFORMATION>() as u32,
                        &mut returned,
                        std::ptr::null_mut())
    };
    if ok == 0
    {
        let cause = io::Error::last_os_error();
        return Err(error(format!("cannot ask {target} how many bytes it holds: \
                                  DeviceIoControl IOCTL_DISK_GET_LENGTH_INFO failed \
                                  ({cause}). The drive did not report its size, so the flash \
                                  is refused rather than writing blind; check the drive is \
                                  readable in the platform's own disk tool.")));
    }
    if info.Length < 0
    {
        return Err(error(format!("{target} reported a drive length of {} bytes, which is not \
                                  a usable size; refusing to write rather than guessing",
                                 info.Length)));
    }
    Ok(info.Length as u64)
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

/// A disk held offline for a write.
///
/// Taking a disk offline deletes its partition device objects, so Windows
/// cannot mount a filesystem on it — not the ones that were there, and not
/// new ones created by the image being written. That is what a volume lock
/// alone cannot do: a lock covers the volume object that exists when it is
/// taken, and a volume created mid-write (when the new partition table
/// lands) has no lock on it. ZFS on Windows uses this same call
/// (`IOCTL_DISK_SET_DISK_ATTRIBUTES` with `DISK_ATTRIBUTE_OFFLINE`) to claim
/// a disk for exclusive raw access, and does its I/O through a handle opened
/// before the disk went offline.
///
/// Dropping the guard brings the disk back online, so the new partitions
/// appear afterwards. The restore is best-effort: if it fails the disk is
/// left offline and the error is printed loudly, because that is a state the
/// operator must fix by hand (Disk Management → Online, or re-plug the
/// reader). The offline state does not persist across a replug or reboot
/// (`Persist` is false).
pub struct DiskOffline
{
    #[cfg(windows)]
    target: String
}

impl DiskOffline
{
    /// Takes the target disk offline. Only for a raw device, only after the
    /// `--yes` gate, and only after the volume dismount — a dry run or a
    /// refused flash must never offline anything.
    pub fn take(target: &str) -> Result<DiskOffline>
    {
        #[cfg(windows)]
        {
            set_disk_attributes(target, true)?;
            Ok(DiskOffline { target: target.to_string() })
        }
        #[cfg(not(windows))]
        {
            let _ = target;
            Ok(DiskOffline {})
        }
    }
}

#[cfg(windows)]
impl Drop for DiskOffline
{
    fn drop(&mut self)
    {
        if let Err(cause) = set_disk_attributes(&self.target, false)
        {
            eprintln!("WARNING: could not bring {} back online ({cause}); the disk is left \
                       offline — bring it back in Disk Management (right-click → Online) or \
                       re-plug the reader",
                      self.target);
        }
    }
}

/// Sets (or clears) the offline attribute on a disk, via
/// `IOCTL_DISK_SET_DISK_ATTRIBUTES`. A second handle is opened for the call:
/// the attribute belongs to the disk, not to any one handle, and the medium's
/// handle stays open for the write.
#[cfg(windows)]
fn set_disk_attributes(target: &str, offline: bool) -> Result<()>
{
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::System::{IO::DeviceIoControl,
                                     Ioctl::{DISK_ATTRIBUTE_OFFLINE,
                                             IOCTL_DISK_SET_DISK_ATTRIBUTES, SET_DISK_ATTRIBUTES}};

    let file = std::fs::OpenOptions::new().read(true)
                                          .write(true)
                                          .open(target)
                                          .map_err(|cause| {
                                              error(format!("cannot open {target} to {} it: \
                                                             {cause}",
                                                            if offline { "take offline" }
                                                            else { "bring back online" }))
                                          })?;
    // The Version member is the size of this structure, used for versioning
    // (not a version number: windows-sys defines no SET_DISK_ATTRIBUTES_VERSION
    // constant, and the working Rust callers set it to the struct size).
    // Persist is false: the offline state must not survive a replug or reboot.
    let attributes =
        SET_DISK_ATTRIBUTES { Version: std::mem::size_of::<SET_DISK_ATTRIBUTES>() as u32,
                              Persist: false,
                              Attributes: if offline { DISK_ATTRIBUTE_OFFLINE } else { 0 },
                              AttributesMask: DISK_ATTRIBUTE_OFFLINE,
                              ..Default::default() };
    let mut returned: u32 = 0;
    // SAFETY: `attributes` is a plain repr(C) struct owned by this frame, the
    // call is synchronous (no OVERLAPPED), so nothing outlives the call.
    let ok = unsafe {
        DeviceIoControl(file.as_raw_handle(),
                        IOCTL_DISK_SET_DISK_ATTRIBUTES,
                        &attributes as *const SET_DISK_ATTRIBUTES as *mut core::ffi::c_void,
                        std::mem::size_of::<SET_DISK_ATTRIBUTES>() as u32,
                        std::ptr::null_mut(),
                        0,
                        &mut returned,
                        std::ptr::null_mut())
    };
    if ok == 0
    {
        let cause = io::Error::last_os_error();
        return Err(error(format!("cannot {} {target}: DeviceIoControl \
                                  IOCTL_DISK_SET_DISK_ATTRIBUTES failed ({cause}). The flash \
                                  is refused and nothing was written",
                                 if offline { "take offline" } else { "bring back online" })));
    }
    Ok(())
}

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
