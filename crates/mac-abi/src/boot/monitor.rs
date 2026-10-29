//! Data-only custom-ROM monitor wire. No SBI or callable firmware
//! object crosses this boundary. Control has raw assembly ownership: bindings
//! borrow only its individual atomic fields after permanent publication.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicI64, AtomicU64};

use super::{CheckedMemory, topology::Cpu};
use crate::BootMemoryRange;
pub const JOIN_PARKED: u64 = 1;
pub const JOIN_PRIMARY: u64 = 2;
/// Wake protocol semantics are part of the ROM/System artifact contract even
/// when the raw structure sizes stay unchanged. The receiver accepts primary
/// JOIN or admitted secondary START, and epochs are monotonic per target with
/// multiple possible senders. This is not an object ID or a restart counter.
const WAKE_PRIMARY_JOIN_AND_TARGET_EPOCHS: usize = 1;
pub const FENCE_I: u64 = 1;
/// One-shot secondary transition; acknowledgment is M acceptance, not S readiness.
pub const START: u64 = 2;
/// Current checked RVA23S64 major families; Z extensions remain a selected
/// platform contract, never inferred from these MISA bits.
pub const REQUIRED_ISA_BITS: u64 = (1 << 8)
                                   | (1 << 12)
                                   | (1 << 0)
                                   | (1 << 5)
                                   | (1 << 3)
                                   | (1 << 2)
                                   | (1 << 21)
                                   | (1 << 7)
                                   | (1 << 18)
                                   | (1 << 20);
/// Private System entry register contract: a0 payload, a1 exact bytes, a2 role.
pub const ENTRY_PRIMARY: u64 = 0x424f4f54;
pub const ENTRY_SECONDARY: u64 = 0x48415254;
pub const START_PREPARED: u64 = 0;
pub const START_STARTING: u64 = 1;
pub const START_ADMITTED_IDLE: u64 = 2;
pub const START_REFUSED: u64 = 3;
/// Admission steps identify the real failed CSR/ownership check without formatting
/// or allocating in the integer-only machine handler.
pub mod step
{
    pub const ISA: u64 = 2;
    pub const WIDTH: u64 = 3;
    pub const ENVIRONMENT: u64 = 4;
    pub const COUNTERS: u64 = 5;
    pub const DELEGATION: u64 = 6;
    pub const PMP: u64 = 7;
    pub const ROOT: u64 = 8;
    pub const ASID: u64 = 9;
    pub const VECTOR: u64 = 10;
    pub const ENTRY: u64 = 11;
}
/// Allocated before Join, sealed once at the final nonreturning primary ROM
/// handoff. All cells are atomic because the directory is already published.
/// Payload cells become immutable before sealed is release-published. The
/// actual NABI load plan supplies entry/image; S never selects a callback.
#[repr(C)]
pub struct BootImage
{
    pub sealed:            AtomicU64,
    pub entry:             AtomicU64,
    pub image_base:        AtomicU64,
    pub image_bytes:       AtomicU64,
    /// Exclusive maximum of all admitted RAM and required physical devices.
    pub physical_limit:    AtomicU64,
    pub required_isa:      AtomicU64,
    pub vector_bytes:      AtomicU64,
    pub hardware_tag_mask: AtomicU64,
    pub fault_entry:       AtomicU64,
    pub returned_entry:    AtomicU64
}
impl BootImage
{
    pub const fn new() -> Self
    {
        Self { sealed:            AtomicU64::new(0),
               entry:             AtomicU64::new(0),
               image_base:        AtomicU64::new(0),
               image_bytes:       AtomicU64::new(0),
               physical_limit:    AtomicU64::new(0),
               required_isa:      AtomicU64::new(0),
               vector_bytes:      AtomicU64::new(0),
               hardware_tag_mask: AtomicU64::new(0),
               fault_entry:       AtomicU64::new(0),
               returned_entry:    AtomicU64::new(0) }
    }
}
/// Embedded, permanent one-shot payload. Primary publishes the four input cells
/// before the canonical command request. M publishes report then START_STARTING
/// before mret, or START_REFUSED only after returning to a nonexecuting park
/// disposition. S publishes START_ADMITTED_IDLE after actual local admission.
/// These states do not grant scheduler dispatch eligibility.
#[repr(C)]
pub struct SecondaryStart
{
    pub sequence:          AtomicU64,
    /// Exact owned resident firmware-root SATP value, including its admitted tag.
    /// Warm S admission moves to the separately owned canonical kernel root.
    pub root_register:     AtomicU64,
    pub control_virtual:   AtomicU64,
    pub service_top:       AtomicU64,
    pub phase:             AtomicU64,
    pub step:              AtomicU64,
    pub observed:          AtomicU64,
    pub observed_aux:      AtomicU64,
    pub vector_bytes:      AtomicU64,
    pub hardware_tag_mask: AtomicU64,
    pub pmp_address:       AtomicU64,
    pub pmp_configuration: AtomicU64
}
impl SecondaryStart
{
    pub const fn new() -> Self
    {
        Self { sequence:          AtomicU64::new(0),
               root_register:     AtomicU64::new(0),
               control_virtual:   AtomicU64::new(0),
               service_top:       AtomicU64::new(0),
               phase:             AtomicU64::new(START_PREPARED),
               step:              AtomicU64::new(0),
               observed:          AtomicU64::new(0),
               observed_aux:      AtomicU64::new(0),
               vector_bytes:      AtomicU64::new(0),
               hardware_tag_mask: AtomicU64::new(0),
               pmp_address:       AtomicU64::new(0),
               pmp_configuration: AtomicU64::new(0) }
    }
}
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
    pub hart:              u64,
    pub msip:              u64,
    pub requested:         AtomicU64,
    pub acknowledged:      AtomicU64,
    pub stop:              AtomicU64,
    pub stopped:           AtomicU64,
    pub joined:            AtomicU64,
    pub command:           AtomicU64,
    pub result:            AtomicI64,
    pub entered:           AtomicU64,
    pub start:             SecondaryStart,
    /// Separate from the synchronous command claim: busy publication cannot
    /// discard a wake intent. M injects SSIP; only the actual S receiver ACKs.
    pub wake_requested:    AtomicU64,
    pub wake_acknowledged: AtomicU64,
    pub frame:             Frame
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
               start: SecondaryStart::new(),
               wake_requested: AtomicU64::new(0),
               wake_acknowledged: AtomicU64::new(0),
               frame: Frame { registers: [0; 32],
                              pc:        0,
                              status:    0 } }
    }
}
pub const CONTROL_BYTES: usize = core::mem::size_of::<Control>();
pub const CONTROL_ALIGNMENT: usize = core::mem::align_of::<Control>();
const _: () = assert!(CONTROL_BYTES == 464
                      && CONTROL_ALIGNMENT == 16
                      && core::mem::offset_of!(Control, frame) == 192);
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
    pub timebase_hz: u64,
    /// Permanent firmware physical pointer; the primary seals the atomic cells
    /// once, after validating the actual System load plan.
    pub image:       *const BootImage
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
        let image = BootMemoryRange { base:   self.image as u64,
                                      length: core::mem::size_of::<BootImage>() as u64 };
        let image_end = image.base.checked_add(image.length).ok_or(Error::Layout)?;
        if image.base < firmware.base
           || image_end > firmware_end
           || !image.base.is_multiple_of(core::mem::align_of::<BootImage>() as u64)
           || !memory.reserved(image)
        {
            return Err(Error::Layout);
        }
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
            if entry.storage.base < image_end && image.base < end
            {
                return Err(Error::Storage { hart: entry.hart });
            }
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
    assert!(core::mem::size_of::<Entry>() == 32 && core::mem::size_of::<Directory>() == 40);

const _: () = assert!(core::mem::size_of::<SecondaryStart>() == 96
                      && core::mem::size_of::<BootImage>() == 80
                      && core::mem::offset_of!(Control, start) == 80
                      && core::mem::offset_of!(Control, wake_requested) == 176
                      && core::mem::offset_of!(Control, wake_acknowledged) == 184);
/// Exact shared layout/protocol identity, checked before any expanded monitor view.
/// This is an artifact-layout gate, not a security hash or restart epoch.
pub const LAYOUT_FINGERPRINT: u64 = layout_fingerprint();
const fn layout_fingerprint() -> u64
{
    let words = [core::mem::size_of::<Control>(),
                 core::mem::size_of::<SecondaryStart>(),
                 core::mem::size_of::<BootImage>(),
                 core::mem::size_of::<Directory>(),
                 core::mem::size_of::<Frame>(),
                 core::mem::size_of::<Entry>(),
                 core::mem::align_of::<Control>(),
                 core::mem::align_of::<SecondaryStart>(),
                 core::mem::align_of::<BootImage>(),
                 core::mem::align_of::<Directory>(),
                 core::mem::align_of::<Frame>(),
                 core::mem::align_of::<Entry>(),
                 core::mem::offset_of!(Control, hart),
                 core::mem::offset_of!(Control, msip),
                 core::mem::offset_of!(Control, requested),
                 core::mem::offset_of!(Control, acknowledged),
                 core::mem::offset_of!(Control, stop),
                 core::mem::offset_of!(Control, stopped),
                 core::mem::offset_of!(Control, joined),
                 core::mem::offset_of!(Control, command),
                 core::mem::offset_of!(Control, result),
                 core::mem::offset_of!(Control, entered),
                 core::mem::offset_of!(Control, start),
                 core::mem::offset_of!(Control, wake_requested),
                 core::mem::offset_of!(Control, wake_acknowledged),
                 core::mem::offset_of!(Control, frame),
                 core::mem::offset_of!(SecondaryStart, sequence),
                 core::mem::offset_of!(SecondaryStart, root_register),
                 core::mem::offset_of!(SecondaryStart, control_virtual),
                 core::mem::offset_of!(SecondaryStart, service_top),
                 core::mem::offset_of!(SecondaryStart, phase),
                 core::mem::offset_of!(SecondaryStart, step),
                 core::mem::offset_of!(SecondaryStart, observed),
                 core::mem::offset_of!(SecondaryStart, observed_aux),
                 core::mem::offset_of!(SecondaryStart, vector_bytes),
                 core::mem::offset_of!(SecondaryStart, hardware_tag_mask),
                 core::mem::offset_of!(SecondaryStart, pmp_address),
                 core::mem::offset_of!(SecondaryStart, pmp_configuration),
                 core::mem::offset_of!(BootImage, sealed),
                 core::mem::offset_of!(BootImage, entry),
                 core::mem::offset_of!(BootImage, image_base),
                 core::mem::offset_of!(BootImage, image_bytes),
                 core::mem::offset_of!(BootImage, physical_limit),
                 core::mem::offset_of!(BootImage, required_isa),
                 core::mem::offset_of!(BootImage, vector_bytes),
                 core::mem::offset_of!(BootImage, hardware_tag_mask),
                 core::mem::offset_of!(BootImage, fault_entry),
                 core::mem::offset_of!(BootImage, returned_entry),
                 core::mem::offset_of!(Directory, entries),
                 core::mem::offset_of!(Directory, count),
                 core::mem::offset_of!(Directory, stack_bytes),
                 core::mem::offset_of!(Directory, timebase_hz),
                 core::mem::offset_of!(Directory, image),
                 core::mem::offset_of!(Frame, registers),
                 core::mem::offset_of!(Frame, pc),
                 core::mem::offset_of!(Frame, status),
                 core::mem::offset_of!(Entry, hart),
                 core::mem::offset_of!(Entry, msip),
                 core::mem::offset_of!(Entry, storage),
                 JOIN_PARKED as usize,
                 JOIN_PRIMARY as usize,
                 WAKE_PRIMARY_JOIN_AND_TARGET_EPOCHS,
                 FENCE_I as usize,
                 START as usize,
                 ENTRY_PRIMARY as usize,
                 ENTRY_SECONDARY as usize,
                 START_PREPARED as usize,
                 START_STARTING as usize,
                 START_ADMITTED_IDLE as usize,
                 START_REFUSED as usize,
                 REQUIRED_ISA_BITS as usize,
                 step::ISA as usize,
                 step::WIDTH as usize,
                 step::ENVIRONMENT as usize,
                 step::COUNTERS as usize,
                 step::DELEGATION as usize,
                 step::PMP as usize,
                 step::ROOT as usize,
                 step::ASID as usize,
                 step::VECTOR as usize,
                 step::ENTRY as usize];
    let mut hash = 0xcbf29ce484222325u64;
    let mut index = 0;
    while index < words.len()
    {
        hash = (hash ^ words[index] as u64).wrapping_mul(0x100000001b3);
        index += 1;
    }
    hash
}
