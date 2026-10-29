//! Data-only custom-ROM monitor wire. No SBI, CPU release or callable firmware
//! object crosses this boundary. Control has raw assembly ownership: bindings
//! borrow only its individual atomic fields after permanent publication.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicI64, AtomicU64};

use super::{CheckedMemory, topology::Cpu};
use crate::BootMemoryRange;
pub const JOIN_PARKED: u64 = 1;
pub const JOIN_PRIMARY: u64 = 2;
pub const FENCE_I: u64 = 1;
/// Legacy CLINT MSIP register bank ends before MTIMECMP.
pub const MSIP_BYTES: u64 = 0x4000;
#[repr(C, align(16))]
pub struct Frame
{
    pub registers: [u64; 32],
    pub pc:        u64,
    pub status:    u64
}
#[repr(C, align(16))]
pub struct Control
{
    pub hart:         u64,
    pub msip:         u64,
    pub requested:    AtomicU64,
    pub acknowledged: AtomicU64,
    pub stop:         AtomicU64,
    pub stopped:      AtomicU64,
    pub joined:       AtomicU64,
    pub command:      AtomicU64,
    pub result:       AtomicI64,
    pub entered:      AtomicU64,
    pub frame:        Frame
}
impl Control
{
    pub const fn new(hart: u64, msip: u64) -> Self
    {
        Self { hart,
               msip,
               requested: AtomicU64::new(0),
               acknowledged: AtomicU64::new(0),
               stop: AtomicU64::new(0),
               stopped: AtomicU64::new(0),
               joined: AtomicU64::new(0),
               command: AtomicU64::new(0),
               result: AtomicI64::new(0),
               entered: AtomicU64::new(0),
               frame: Frame { registers: [0; 32],
                              pc:        0,
                              status:    0 } }
    }
}
pub const CONTROL_BYTES: usize = core::mem::size_of::<Control>();
pub const CONTROL_ALIGNMENT: usize = core::mem::align_of::<Control>();
const _: () = assert!(CONTROL_BYTES == 352
                      && CONTROL_ALIGNMENT == 16
                      && core::mem::offset_of!(Control, frame) == 80);
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Entry
{
    pub hart:    u64,
    pub msip:    u64,
    pub storage: BootMemoryRange
}
#[repr(C)]
pub struct Directory
{
    pub entries:     *const Entry,
    pub count:       usize,
    pub stack_bytes: u64,
    /// Actual /cpus timebase-frequency; never a second hardcoded clock.
    pub timebase_hz: u64
}
#[derive(Debug)]
pub enum Error
{
    Layout,
    Topology,
    Storage
    {
        hart: u64
    },
    Route
    {
        hart: u64
    },
    Duplicate
    {
        hart: u64
    }
}
impl core::fmt::Display for Error
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Layout =>
            {
                f.write_str("machine monitor layout/clock disagrees with platform; rebuild ROM \
                             and System")
            }
            Self::Topology =>
            {
                f.write_str("machine monitor directory disagrees with enabled CPU topology")
            }
            Self::Storage { hart } =>
            {
                write!(f, "machine monitor hart {hart} has invalid/unreserved storage")
            }
            Self::Route { hart } =>
            {
                write!(f, "machine monitor hart {hart} has invalid CLINT MSIP route")
            }
            Self::Duplicate { hart } =>
            {
                write!(f, "machine monitor hart {hart} aliases another hart's storage/route")
            }
        }
    }
}
impl Directory
{
    pub fn checked(&self, entries: &[Entry], cpus: &[Cpu], memory: CheckedMemory<'_>,
                   stack_bytes: u64, firmware: BootMemoryRange, clint: BootMemoryRange)
                   -> Result<(), Error>
    {
        if self.stack_bytes != stack_bytes
           || stack_bytes < memory.page_bytes()
           || stack_bytes < CONTROL_BYTES as u64
           || !stack_bytes.is_multiple_of(memory.page_bytes())
           || self.timebase_hz == 0
           || self.timebase_hz > u64::MAX / 2
        {
            return Err(Error::Layout);
        }
        if entries.is_empty() || self.count != entries.len() || entries.len() != cpus.len()
        {
            return Err(Error::Topology);
        }
        let firmware_end = firmware.base.checked_add(firmware.length).ok_or(Error::Layout)?;
        if clint.length < MSIP_BYTES
        {
            return Err(Error::Layout);
        }
        let clint_end = clint.base.checked_add(MSIP_BYTES).ok_or(Error::Layout)?;
        for (index, (entry, cpu)) in entries.iter().zip(cpus).enumerate()
        {
            if entry.hart != cpu.hart
            {
                return Err(Error::Topology);
            }
            let end = entry.storage
                           .base
                           .checked_add(entry.storage.length)
                           .ok_or(Error::Storage { hart: entry.hart })?;
            if entry.storage.length != stack_bytes
               || !entry.storage.base.is_multiple_of(memory.page_bytes())
               || entry.storage.base < firmware.base
               || end > firmware_end
               || !memory.reserved(entry.storage)
            {
                return Err(Error::Storage { hart: entry.hart });
            }
            if entry.msip < clint.base
               || entry.msip.checked_add(4).is_none_or(|end| end > clint_end)
               || !entry.msip.is_multiple_of(4)
            {
                return Err(Error::Route { hart: entry.hart });
            }
            for other in &entries[..index]
            {
                if entry.msip == other.msip
                   || (entry.storage.base < other.storage.base + other.storage.length
                       && other.storage.base < end)
                {
                    return Err(Error::Duplicate { hart: entry.hart });
                }
            }
        }
        Ok(())
    }
}
const _: () =
    assert!(core::mem::size_of::<Entry>() == 32 && core::mem::size_of::<Directory>() == 32);
