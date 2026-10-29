//! Resident owner of the custom-ROM M-monitor transport. No callbacks or SBI.
//! Membership is frozen at boot; all secondary CPUs remain parked in M mode.
//! Shared control is raw assembly storage: only individual atomic cells are
//! borrowed, never a Control reference or an interrupted frame reference.
use core::{arch::asm,
           fmt, ptr,
           sync::atomic::{AtomicBool, AtomicU64, Ordering}};

use mac_abi::{BootInfo, BootMemoryRange,
              boot::{CheckedMemory,
                     monitor::{self, Control, Entry},
                     topology::Cpu}};

use super::memory::*;

// Build-host policy and the target's actual wire layout must agree before a
// native binding can form any control-field pointer in firmware backing.
const _: () = assert!(MONITOR_CONTROL_BYTES == core::mem::size_of::<Control>() as u64
                      && MONITOR_STACK_BYTES >= MONITOR_CONTROL_BYTES
                      && MONITOR_STACK_BYTES.is_multiple_of(PAGE_BYTES));

struct Admitted
{
    entries:     &'static [Entry],
    primary:     usize,
    timebase_hz: u64
}
static MONITOR: spin::Once<Admitted> = spin::Once::new();
static ISSUING: AtomicBool = AtomicBool::new(false);
static BROKEN: AtomicBool = AtomicBool::new(false);
static TERMINAL: AtomicBool = AtomicBool::new(false);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
#[derive(Debug)]
pub(crate) enum Error
{
    Memory(mac_abi::boot::MemoryError),
    Directory(monitor::Error),
    Physical(mac_kernel_model::pages::physical::Error),
    AlreadyAdmitted,
    Missing,
    ForeignHart,
    Busy,
    Stopping,
    TransportRetired,
    SequenceExhausted,
    ClockOverflow,
    State
    {
        hart: u64
    },
    Uncertain
    {
        hart:     u64,
        sequence: u64
    },
    Refused
    {
        hart:   u64,
        status: i64
    },
    StopUnacknowledged
    {
        hart: u64
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Memory(error) => error.fmt(f),
            Self::Directory(error) => error.fmt(f),
            Self::Physical(error) => error.fmt(f),
            Self::AlreadyAdmitted =>
            {
                f.write_str("machine monitor cannot be readmitted without reset")
            }
            Self::Missing =>
            {
                f.write_str("mandatory machine monitor is unavailable; rebuild ROM and System")
            }
            Self::ForeignHart =>
            {
                f.write_str("machine monitor issuer is not the admitted primary executor")
            }
            Self::Busy =>
            {
                f.write_str("another executable publication owns the monitor command slot")
            }
            Self::Stopping => f.write_str("terminal stop has closed machine monitor publication"),
            Self::TransportRetired =>
            {
                f.write_str("a previous unacknowledged/refused monitor command retired the \
                             transport until reset")
            }
            Self::SequenceExhausted =>
            {
                f.write_str("machine monitor command sequence exhausted; reset required")
            }
            Self::ClockOverflow =>
            {
                f.write_str("machine monitor deadline overflows the admitted time counter")
            }
            Self::State { hart } =>
            {
                write!(f,
                       "machine monitor hart {hart} has not acknowledged the fresh parked boot \
                        state")
            }
            Self::Uncertain { hart,
                              sequence } =>
            {
                write!(f,
                       "machine monitor hart {hart} command {sequence} lacks ACK; transport \
                        retained until reset")
            }
            Self::Refused { hart,
                            status } =>
            {
                write!(f, "machine monitor hart {hart} refused command with status {status}")
            }
            Self::StopUnacknowledged { hart } =>
            {
                write!(f,
                       "machine monitor hart {hart} did not acknowledge irreversible terminal stop")
            }
        }
    }
}
impl Error
{
    pub(crate) fn status(&self) -> mac_abi::Status
    {
        match self
        {
            Self::Busy => mac_abi::SERVICE_BUSY,
            Self::ForeignHart => mac_abi::ACCESS_DENIED,
            Self::Stopping => mac_abi::OPERATION_CANCELLED,
            Self::Uncertain { .. }
            | Self::StopUnacknowledged { .. }
            | Self::TransportRetired
            | Self::ClockOverflow => mac_abi::IO_ERROR,
            Self::SequenceExhausted => mac_abi::NOT_SUPPORTED,
            Self::Refused { status,
                            .. } if *status <= 0 && *status >= i32::MIN as i64 => *status as i32,
            _ => mac_abi::BAD_ABI
        }
    }
}
/// # Safety
/// The actual ROM lent immutable physical wire records and isolated Control
/// allocations until reset. Memory admission has validated their reservations;
/// this function creates new mapped views without rebasing existing references.
pub(crate) unsafe fn admit(info: &'static BootInfo, memory: CheckedMemory<'static>,
                           cpus: &'static [Cpu], primary: usize,
                           ram: &mac_kernel_model::pages::physical::DirectMap<'static>)
                           -> Result<(), Error>
{
    if MONITOR.get().is_some()
    {
        return Err(Error::AlreadyAdmitted);
    }
    let directory =
        unsafe { super::memory::primary_slice(info, info.monitor, 1) }.map_err(Error::Memory)?
                                                                      .first()
                                                                      .ok_or(Error::Missing)?;
    let entries = unsafe { super::memory::primary_slice(info, directory.entries, directory.count) }
        .map_err(Error::Memory)?;
    directory.checked(entries,
                      cpus,
                      memory,
                      MONITOR_STACK_BYTES,
                      BootMemoryRange { base:   FIRMWARE_HEAP_BASE,
                                        length: FIRMWARE_HEAP_BYTES },
                      BootMemoryRange { base:   CLINT_BASE,
                                        length: CLINT_BYTES })
             .map_err(Error::Directory)?;
    for entry in entries
    {
        ram.virtual_address(mac_kernel_model::pages::PhysicalAddress(entry.storage.base),
                            entry.storage.length)
           .map_err(Error::Physical)?;
        let control = control(entry);
        let expected =
            if entry.hart == primary as u64 { monitor::JOIN_PRIMARY } else { monitor::JOIN_PARKED };
        // Only immutable identity words are read directly. No &Control is formed.
        let identity_matches = unsafe {
            ptr::read_volatile(ptr::addr_of!((*control).hart)) == entry.hart
            && ptr::read_volatile(ptr::addr_of!((*control).msip)) == entry.msip
        };
        if !identity_matches
           || field(entry, core::mem::offset_of!(Control, joined)).load(Ordering::Acquire)
              != expected
           || field(entry, core::mem::offset_of!(Control, stop)).load(Ordering::Acquire) != 0
           || field(entry, core::mem::offset_of!(Control, stopped)).load(Ordering::Acquire) != 0
           || field(entry, core::mem::offset_of!(Control, requested)).load(Ordering::Acquire) != 0
           || field(entry, core::mem::offset_of!(Control, acknowledged)).load(Ordering::Acquire)
              != 0
           || field(entry, core::mem::offset_of!(Control, command)).load(Ordering::Acquire) != 0
           || field(entry, core::mem::offset_of!(Control, entered)).load(Ordering::Acquire) != 0
        {
            return Err(Error::State { hart: entry.hart });
        }
    }
    MONITOR.call_once(|| {
               Admitted { entries,
                          primary,
                          timebase_hz: directory.timebase_hz }
           });
    Ok(())
}
fn control(entry: &Entry) -> *const Control
{
    (VM_PHYSMAP_START + entry.storage.base - VM_PHYSMAP_PHYSICAL_BASE) as *const Control
}
fn field(entry: &Entry, offset: usize) -> &'static AtomicU64
{
    // SAFETY: admit checked the disjoint, aligned permanently reserved Control
    // extent in the permanent physmap. Offset comes only from this shared type.
    unsafe { &*((control(entry) as *const u8).add(offset).cast::<AtomicU64>()) }
}
fn now() -> u64
{
    let value;
    unsafe {
        asm!("rdtime {}",out(reg)value,options(nomem,nostack));
    }
    value
}
fn notify(entry: &Entry)
{
    // Release alone does not order MMIO. Neither operation calls foreign code.
    unsafe {
        asm!("fence iorw,iorw", options(nostack));
        ptr::write_volatile(entry.msip as *mut u32, 1);
        asm!("fence iorw,iorw", options(nostack));
    }
}
fn selected() -> Result<&'static Admitted, Error>
{
    let monitor = MONITOR.get().ok_or(Error::Missing)?;
    if super::hart_id() != monitor.primary
    {
        return Err(Error::ForeignHart);
    }
    Ok(monitor)
}
struct Issuer
{
    complete: bool
}
impl Drop for Issuer
{
    fn drop(&mut self)
    {
        // Uncertain publication cannot silently reuse the only command cell.
        // The only current payload is a fence, so no foreign buffer is loaned.
        if !self.complete
        {
            BROKEN.store(true, Ordering::Release);
        }
        ISSUING.store(false, Ordering::Release);
    }
}
/// Called after image writes and before any executable binding is published.
/// The actual image owner remains pinned across this call and any refusal.
/// No hart can join/release after this phase's frozen boot membership.
pub(crate) fn publish_instructions() -> Result<(), Error>
{
    let monitor = selected()?;
    if TERMINAL.load(Ordering::Acquire)
    {
        return Err(Error::Stopping);
    }
    if BROKEN.load(Ordering::Acquire)
    {
        return Err(Error::TransportRetired);
    }
    ISSUING.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
           .map_err(|_| Error::Busy)?;
    let mut issuer = Issuer { complete: true };
    if BROKEN.load(Ordering::Acquire)
    {
        return Err(Error::TransportRetired);
    }
    if TERMINAL.load(Ordering::Acquire)
    {
        return Err(Error::Stopping);
    }
    let sequence =
        SEQUENCE.load(Ordering::Relaxed).checked_add(1).ok_or(Error::SequenceExhausted)?;
    let deadline = now().checked_add(monitor.timebase_hz).ok_or(Error::ClockOverflow)?;
    SEQUENCE.store(sequence, Ordering::Relaxed);
    // The writer orders prior stores and its own instruction fetches. Remote
    // monitor ACKs follow each remote hart's actual fence.i.
    unsafe {
        asm!("fence rw,rw", "fence.i", options(nostack));
    }
    issuer.complete = false;
    for entry in monitor.entries.iter().filter(|entry| entry.hart != monitor.primary as u64)
    {
        if TERMINAL.load(Ordering::Acquire)
        {
            return Err(Error::Stopping);
        }
        field(entry, core::mem::offset_of!(Control, command)).store(monitor::FENCE_I,
                                                                    Ordering::Relaxed);
        field(entry, core::mem::offset_of!(Control, requested)).store(sequence, Ordering::Release);
        notify(entry);
    }
    for entry in monitor.entries.iter().filter(|entry| entry.hart != monitor.primary as u64)
    {
        while field(entry, core::mem::offset_of!(Control, acknowledged)).load(Ordering::Acquire)
              != sequence
        {
            if TERMINAL.load(Ordering::Acquire)
            {
                return Err(Error::Stopping);
            }
            if now() >= deadline
            {
                return Err(Error::Uncertain { hart: entry.hart,
                                              sequence });
            }
            core::hint::spin_loop();
        }
        let result = unsafe { &*ptr::addr_of!((*control(entry)).result) }.load(Ordering::Acquire);
        if result != 0
        {
            return Err(Error::Refused { hart:   entry.hart,
                                        status: result });
        }
    }
    issuer.complete = true;
    Ok(())
}
/// Irreversible terminal operation. Bypasses every normal command/owner lock;
/// peers may be interrupted while holding those locks. The caller must abandon
/// ordinary execution and must use direct shutdown on any refusal.
pub(crate) fn stop_peers() -> Result<(), Error>
{
    let monitor = selected()?;
    TERMINAL.store(true, Ordering::Release);
    let deadline = now().checked_add(monitor.timebase_hz).ok_or(Error::ClockOverflow)?;
    for entry in monitor.entries.iter().filter(|entry| entry.hart != monitor.primary as u64)
    {
        field(entry, core::mem::offset_of!(Control, stop)).store(1, Ordering::Release);
        notify(entry);
    }
    for entry in monitor.entries.iter().filter(|entry| entry.hart != monitor.primary as u64)
    {
        while field(entry, core::mem::offset_of!(Control, stopped)).load(Ordering::Acquire) != 1
        {
            if now() >= deadline
            {
                return Err(Error::StopUnacknowledged { hart: entry.hart });
            }
            core::hint::spin_loop();
        }
    }
    Ok(())
}
