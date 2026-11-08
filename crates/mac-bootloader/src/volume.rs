//! The storage contract a bootstrap hands to System.
//!
//! `BootInfo` carries four callbacks and an opaque context rather than a device
//! type, because System is architecture- and bus-independent: it asks for sector
//! 40 of disk 1 and knows nothing about virtio, SDHCI or the instruction set
//! underneath. This module is the one implementation of that contract, shared by
//! every bootstrap, so a second board's loader cannot drift from the first in how
//! it bounds a request or which status it reports.
//!
//! Every entry point is `extern "C"` and reached from System through a raw
//! pointer, so each one validates its arguments before touching them: a null
//! context, a null buffer, an out-of-range disk and an out-of-range sector are
//! refusals with distinct statuses rather than undefined behaviour.

use alloc::boxed::Box;

use mac_abi::{BAD_ARGUMENT, DiskInfo, IO_ERROR, NOT_FOUND, NOT_SUPPORTED, OK, READ_ONLY, Status};
use mac_block::WritableBlockDevice;

/// The disks this bootstrap offers, borrowed for the whole System lifetime.
///
/// A pointer rather than a slice because `BootInfo` is a C layout and carries
/// its own count; the array itself stays owned by the bootstrap and is never
/// moved or reallocated after this is formed.
pub struct BootVolume
{
    pub disks:      *mut Box<dyn WritableBlockDevice>,
    pub disk_count: usize
}

impl BootVolume
{
    /// The disk at `index`, or `None` when the index names no disk.
    ///
    /// # Safety
    /// `disks` must point at `disk_count` initialised entries that stay alive
    /// for as long as any callback formed from this volume can be called.
    unsafe fn disk(&self, index: u32) -> Option<&dyn WritableBlockDevice>
    {
        if index as usize >= self.disk_count
        {
            return None;
        }
        Some(unsafe { &**self.disks.add(index as usize) })
    }

    /// The same disk, mutably, for the operations that write.
    ///
    /// # Safety
    /// As [`BootVolume::disk`], and the caller must not hold a second view of
    /// the same disk at the same time.
    unsafe fn disk_mut(&mut self, index: u32) -> Option<&mut (dyn WritableBlockDevice + 'static)>
    {
        if index as usize >= self.disk_count
        {
            return None;
        }
        Some(unsafe { &mut **self.disks.add(index as usize) })
    }
}

/// # Safety
/// `context` must be a `BootVolume` pointer supplied in `BootInfo`, and `output`
/// must be writable for one `DiskInfo`.
pub unsafe extern "C" fn disk_info(context: *mut (), index: u32, output: *mut DiskInfo) -> Status
{
    if context.is_null()
       || output.is_null()
       || !(output as usize).is_multiple_of(core::mem::align_of::<DiskInfo>())
    {
        return BAD_ARGUMENT;
    }
    let context = unsafe { &*context.cast::<BootVolume>() };
    let Some(disk) = (unsafe { context.disk(index) })
    else
    {
        return NOT_FOUND;
    };
    unsafe {
        *output = DiskInfo { sector_count: disk.sector_count(),
                             read_only:    u32::from(disk.is_read_only()),
                             icon_kind:    *b"ICN#",
                             icon_id:      24000,
                             reserved:     0 };
    }
    OK
}

/// # Safety
/// As [`disk_info`], with `output` writable for `count` contiguous 512-byte sectors.
/// `count` must be between one and `FIRMWARE_READ_SECTORS`, inclusive.
pub unsafe extern "C" fn read_sectors(context: *mut (), index: u32, sector: u64, count: u32,
                                      output: *mut u8)
                                      -> Status
{
    if context.is_null()
       || output.is_null()
       || !(1..=mac_abi::FIRMWARE_READ_SECTORS as u32).contains(&count)
    {
        return BAD_ARGUMENT;
    }
    let context = unsafe { &mut *context.cast::<BootVolume>() };
    let Some(disk) = (unsafe { context.disk_mut(index) })
    else
    {
        return NOT_FOUND;
    };
    if sector.checked_add(u64::from(count)).is_none_or(|end| end > disk.sector_count())
    {
        return BAD_ARGUMENT;
    }
    let output =
        unsafe { core::slice::from_raw_parts_mut(output.cast::<[u8; 512]>(), count as usize) };
    match disk.read_sectors(sector, output)
    {
        Ok(()) => OK,
        Err(error) =>
        {
            super::log(format_args!("DISK_READ_FAILED: disk={} sector={} {}",
                                    index, sector, error));
            // Preserved from the bootstrap this moved out of: a failed read has
            // always reported NOT_SUPPORTED here, and changing it would alter
            // what System sees for an existing failure.
            NOT_SUPPORTED
        }
    }
}

/// # Safety
/// As [`disk_info`], with `input` readable for one 512-byte sector.
pub unsafe extern "C" fn write_sector(context: *mut (), index: u32, sector: u64, input: *const u8)
                                      -> Status
{
    if context.is_null() || input.is_null()
    {
        return BAD_ARGUMENT;
    }
    let context = unsafe { &mut *context.cast::<BootVolume>() };
    let Some(disk) = (unsafe { context.disk_mut(index) })
    else
    {
        return NOT_FOUND;
    };
    if disk.is_read_only()
    {
        return READ_ONLY;
    }
    if sector >= disk.sector_count()
    {
        return BAD_ARGUMENT;
    }
    let input = unsafe { &*input.cast::<[u8; 512]>() };
    disk.write_sector(sector, input).map_or_else(|error| {
                                                     super::log(format_args!("DISK_WRITE_FAILED: \
                                                                              disk={} sector={} \
                                                                              {}",
                                                                             index, sector, error));
                                                     IO_ERROR
                                                 },
                                                 |_| OK)
}

/// # Safety
/// `context` must be a `BootVolume` pointer supplied in `BootInfo`.
pub unsafe extern "C" fn flush_disk(context: *mut (), index: u32) -> Status
{
    if context.is_null()
    {
        return BAD_ARGUMENT;
    }
    let context = unsafe { &mut *context.cast::<BootVolume>() };
    let Some(disk) = (unsafe { context.disk_mut(index) })
    else
    {
        return NOT_FOUND;
    };
    if disk.is_read_only()
    {
        return READ_ONLY;
    }
    disk.flush().map_or_else(|error| {
                                 super::log(format_args!("DISK_FLUSH_FAILED: disk={} {}",
                                                         index, error));
                                 IO_ERROR
                             },
                             |_| OK)
}
