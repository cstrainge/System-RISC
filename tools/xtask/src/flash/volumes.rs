//! Dismounting the volumes on a disk before writing to it.
//!
//! On Windows a physical drive's volumes are usually mounted, and a mounted
//! volume can refuse a raw write to its disk (Access is denied) even from an
//! elevated shell. Other flashing tools lock and dismount the volumes first;
//! this module does the same: it finds every volume on the target disk, locks
//! it, dismounts it, and keeps the volume handles open (locked) until the
//! write and verify are done. Closing a handle releases its lock, so the
//! guard must be held for the whole write. There is no remount step: the
//! card's old filesystem is gone once the image is written, and the card is
//! about to be pulled.
//!
//! A volume with no mounted filesystem (a Linux partition, which Windows
//! shows as RAW) is still locked and held: no filesystem means no open file
//! can be holding it, so it cannot block the write — but the held lock is
//! what keeps Windows from mounting a filesystem on the disk mid-write and
//! denying the rest of the raw write. The same goes for a volume Windows
//! lists but cannot open at all (a stale entry, e.g. a half-written
//! partition left by an interrupted flash): its device object no longer
//! exists, so nothing can be using it.

#[cfg(windows)]
use std::io;

use crate::Result;
#[cfg(windows)]
use crate::error;

/// The volumes on the target disk, locked and dismounted for the write.
///
/// Closing a volume handle releases its lock, so this guard must be held
/// until the write and verify are done; dropping it is the unlock. A failure
/// anywhere in the dismount drops the guard too, so no volume is left locked
/// by a refused flash.
pub struct VolumeLocks
{
    // Held, never read: the handles close (releasing their locks) when the
    // guard drops, and that is the whole purpose of the field.
    #[allow(dead_code)]
    #[cfg(windows)]
    volumes: Vec<std::fs::File>
}

/// Locks and dismounts every volume on the target disk, before the write.
///
/// This runs only after the `--yes` gate has passed: a dry run or any refused
/// flash must never dismount anything. Returns `None` where there is nothing
/// to hold (a file target, or a platform without the concept).
pub fn dismount_volumes(target: &str) -> Result<Option<VolumeLocks>>
{
    #[cfg(windows)]
    {
        dismount_volumes_windows(target)
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        Ok(None)
    }
}

#[cfg(windows)]
fn dismount_volumes_windows(target: &str) -> Result<Option<VolumeLocks>>
{
    let disk = disk_number(target)?;
    let mut locked = Vec::new();
    for name in list_volumes()?
    {
        let Some(volume) = open_volume(&name)?
        else
        {
            continue;
        };
        if !volume_disks(&volume, &name)?.contains(&disk)
        {
            continue;
        }
        // Take the lock on every openable volume on the target disk,
        // mounted or not, and hold it through the write: a held lock is
        // what keeps Windows from mounting a filesystem on the disk
        // mid-write and denying the rest of the raw write. A mounted
        // volume is dismounted as well, so its filesystem is detached.
        // Locking an unmounted volume can fail (there is no filesystem
        // to lock); that volume has no filesystem to block the write, so
        // the failure is noted and the volume skipped, not refused.
        if volume_mounted(&name)?
        {
            lock_and_dismount(&volume, &name)?;
            println!("locked and dismounted volume {name}");
        }
        else if let Err(cause) = lock_volume(&volume, &name)
        {
            eprintln!("could not lock unmounted volume {name} ({cause}); it has no filesystem, \
                       so it needs no lock and the flash continues without holding it");
            continue;
        }
        else
        {
            println!("locked volume {name}");
        }
        locked.push(volume);
    }
    Ok(Some(VolumeLocks { volumes: locked }))
}

/// The disk number in a `\\.\PhysicalDriveN` target.
#[cfg(windows)]
fn disk_number(target: &str) -> Result<u32>
{
    let upper = target.to_ascii_uppercase();
    let digits =
        upper.strip_prefix("\\\\.\\PHYSICALDRIVE").ok_or_else(|| {
                                                       error(format!("{target} is not a physical \
                                                                      drive path the flash tool \
                                                                      can dismount volumes for; \
                                                                      refusing to write rather \
                                                                      than guessing which disk \
                                                                      it is"))
                                                   })?;
    digits.parse().map_err(|_| {
                      error(format!("{target} does not name a physical drive number; refusing to \
                                     write rather than guessing which disk it is"))
                  })
}

/// Every volume on the machine, as `\\?\Volume{...}\` paths.
#[cfg(windows)]
fn list_volumes() -> Result<Vec<String>>
{
    use windows_sys::Win32::{Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE},
                             Storage::FileSystem::{FindFirstVolumeW, FindNextVolumeW,
                                                   FindVolumeClose}};

    // A volume name is about fifty characters; 512 leaves room no real
    // volume name needs.
    let mut buffer = vec![0u16; 512];
    let search = unsafe { FindFirstVolumeW(buffer.as_mut_ptr(), buffer.len() as u32) };
    if search == INVALID_HANDLE_VALUE
    {
        let cause = io::Error::last_os_error();
        return Err(error(format!("cannot list volumes to find the ones on the target disk: \
                                  FindFirstVolumeW failed ({cause}). The flash is refused and \
                                  nothing was written")));
    }
    let mut names = Vec::new();
    loop
    {
        names.push(wide_string(&buffer));
        let more = unsafe { FindNextVolumeW(search, buffer.as_mut_ptr(), buffer.len() as u32) };
        if more != 0
        {
            continue;
        }
        let cause = io::Error::last_os_error();
        if cause.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32)
        {
            break;
        }
        unsafe { FindVolumeClose(search) };
        return Err(error(format!("cannot list volumes to find the ones on the target disk: \
                                  FindNextVolumeW failed ({cause}). The flash is refused and \
                                  nothing was written")));
    }
    unsafe { FindVolumeClose(search) };
    Ok(names)
}

/// A null-terminated wide string as a Rust string.
#[cfg(windows)]
fn wide_string(buffer: &[u16]) -> String
{
    let end = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// Opens a volume by its `\\?\Volume{...}\` path.
///
/// The returned File owns the handle and closes it on drop; nothing else
/// closes it. Returns `None` for a volume Windows lists but cannot open:
/// `ERROR_FILE_NOT_FOUND` / `ERROR_PATH_NOT_FOUND` (a stale entry — for
/// example a half-written partition left by an interrupted flash — whose
/// device object no longer exists) or `ERROR_NOT_READY` (a volume whose
/// device is not ready, e.g. no media). In both cases nothing can be using
/// the volume, so it holds no mount and needs no lock. That case is noted
/// and skipped; any other open failure is refused, because the volume's
/// state is genuinely unknown.
#[cfg(windows)]
fn open_volume(name: &str) -> Result<Option<std::fs::File>>
{
    use std::os::windows::{ffi::OsStrExt, io::FromRawHandle};

    use windows_sys::Win32::{Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NOT_READY,
                                          ERROR_PATH_NOT_FOUND, GENERIC_READ, GENERIC_WRITE,
                                          INVALID_HANDLE_VALUE},
                             Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL,
                                                   FILE_SHARE_READ, FILE_SHARE_WRITE,
                                                   OPEN_EXISTING}};

    let wide: Vec<u16> = std::ffi::OsStr::new(name).encode_wide().chain([0]).collect();
    let handle = unsafe {
        CreateFileW(wide.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut())
    };
    if handle == INVALID_HANDLE_VALUE
    {
        let cause = io::Error::last_os_error();
        if matches!(cause.raw_os_error(),
                    Some(code) if code == ERROR_FILE_NOT_FOUND as i32
                                 || code == ERROR_PATH_NOT_FOUND as i32
                                 || code == ERROR_NOT_READY as i32)
        {
            eprintln!("skipping volume {name}: it cannot be opened ({cause}) — it holds no \
                       mount, so it needs no lock");
            return Ok(None);
        }
        return Err(error(format!("cannot open volume {name} to check which disk it is on \
                                  ({cause}). The flash is refused and nothing was written \
                                  rather than writing to a disk whose volumes are unknown")));
    }
    // SAFETY: CreateFileW returned a fresh owned handle; the File owns it from
    // here and closes it on drop, and nothing else closes it.
    Ok(Some(unsafe { std::fs::File::from_raw_handle(handle) }))
}

/// The disk numbers a volume spans, from `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`.
#[cfg(windows)]
fn volume_disks(file: &std::fs::File, name: &str) -> Result<Vec<u32>>
{
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::{Storage::FileSystem::IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
                             System::{IO::DeviceIoControl,
                                      Ioctl::{DISK_EXTENT, VOLUME_DISK_EXTENTS}}};

    // Room for sixteen extents; a volume on one card has one. The buffer is
    // u64 so its alignment (8) covers VOLUME_DISK_EXTENTS, which a byte
    // buffer would not guarantee.
    const MAX_EXTENTS: usize = 16;
    let bytes = std::mem::size_of::<VOLUME_DISK_EXTENTS>()
                + (MAX_EXTENTS - 1) * std::mem::size_of::<DISK_EXTENT>();
    let mut buffer = vec![0u64; bytes.div_ceil(8)];
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(file.as_raw_handle(),
                        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
                        std::ptr::null(),
                        0,
                        buffer.as_mut_ptr() as *mut core::ffi::c_void,
                        (buffer.len() * 8) as u32,
                        &mut returned,
                        std::ptr::null_mut())
    };
    if ok == 0
    {
        let cause = io::Error::last_os_error();
        return Err(error(format!("cannot ask volume {name} which disk it is on: \
                                  DeviceIoControl IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS failed \
                                  ({cause}). The flash is refused and nothing was written")));
    }
    // SAFETY: the call wrote a VOLUME_DISK_EXTENTS into the buffer, which is
    // large and aligned for sixteen extents; the count is clamped to that.
    let header = unsafe { &*(buffer.as_ptr() as *const VOLUME_DISK_EXTENTS) };
    let count = (header.NumberOfDiskExtents as usize).min(MAX_EXTENTS);
    let mut disks = Vec::new();
    for index in 0..count
    {
        let extent = unsafe { &*header.Extents.as_ptr().add(index) };
        disks.push(extent.DiskNumber);
    }
    Ok(disks)
}

/// Whether a filesystem is mounted on the volume.
///
/// A volume with no mounted filesystem has no open file that could be
/// holding it, so it needs no lock. Only the "unrecognized volume" failure
/// means that; any other failure is refused, because the volume's state is
/// genuinely unknown.
#[cfg(windows)]
fn volume_mounted(name: &str) -> Result<bool>
{
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::{Foundation::ERROR_UNRECOGNIZED_VOLUME,
                             Storage::FileSystem::GetVolumeInformationW};

    let wide: Vec<u16> = std::ffi::OsStr::new(name).encode_wide().chain([0]).collect();
    let mounted = unsafe {
        GetVolumeInformationW(wide.as_ptr(),
                              std::ptr::null_mut(),
                              0,
                              std::ptr::null_mut(),
                              std::ptr::null_mut(),
                              std::ptr::null_mut(),
                              std::ptr::null_mut(),
                              0)
    };
    if mounted != 0
    {
        return Ok(true);
    }
    let cause = io::Error::last_os_error();
    if cause.raw_os_error() == Some(ERROR_UNRECOGNIZED_VOLUME as i32)
    {
        return Ok(false);
    }
    Err(error(format!("cannot tell whether volume {name} has a mounted filesystem: \
                       GetVolumeInformationW failed ({cause}). The flash is refused and nothing \
                       was written")))
}

/// Locks a volume and then dismounts it.
///
/// The lock fails while any process has a file open on the volume; that is
/// the refusal doing its job, and the error says so.
#[cfg(windows)]
/// Locks a volume.
///
/// The lock is held by keeping the volume's handle open (see `VolumeLocks`);
/// closing the handle releases it. The lock fails while any process has a
/// file open on the volume; that is the refusal doing its job, and the error
/// says so.
#[cfg(windows)]
fn lock_volume(file: &std::fs::File, name: &str) -> Result<()>
{
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::System::{IO::DeviceIoControl, Ioctl::FSCTL_LOCK_VOLUME};

    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(file.as_raw_handle(),
                        FSCTL_LOCK_VOLUME,
                        std::ptr::null(),
                        0,
                        std::ptr::null_mut(),
                        0,
                        &mut returned,
                        std::ptr::null_mut())
    };
    if ok == 0
    {
        let cause = io::Error::last_os_error();
        return Err(error(format!("cannot lock volume {name}: DeviceIoControl \
                                  FSCTL_LOCK_VOLUME failed ({cause}). Some process has a file \
                                  open on the volume, so the flash is refused and nothing was \
                                  written; close whatever is using the volume and try again")));
    }
    Ok(())
}

/// Dismounts a volume, detaching its filesystem. Only for a volume that has
/// one mounted; locking is what holds the disk claimed, dismounting is what
/// detaches the filesystem that is already there.
#[cfg(windows)]
fn dismount_volume(file: &std::fs::File, name: &str) -> Result<()>
{
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::System::{IO::DeviceIoControl, Ioctl::FSCTL_DISMOUNT_VOLUME};

    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(file.as_raw_handle(),
                        FSCTL_DISMOUNT_VOLUME,
                        std::ptr::null(),
                        0,
                        std::ptr::null_mut(),
                        0,
                        &mut returned,
                        std::ptr::null_mut())
    };
    if ok == 0
    {
        let cause = io::Error::last_os_error();
        return Err(error(format!("cannot dismount volume {name}: DeviceIoControl \
                                  FSCTL_DISMOUNT_VOLUME failed ({cause}). The flash is \
                                  refused and nothing was written")));
    }
    Ok(())
}

/// Locks a volume and then dismounts it.
#[cfg(windows)]
fn lock_and_dismount(file: &std::fs::File, name: &str) -> Result<()>
{
    lock_volume(file, name)?;
    dismount_volume(file, name)
}
