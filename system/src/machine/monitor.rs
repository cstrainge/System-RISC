//! Resident owner of the custom-ROM M-monitor transport. No callbacks or SBI.
//! Membership is frozen at boot; sealed core claims alone may release prepared peers.
//! Shared control is raw assembly storage: only individual atomic cells are
//! borrowed, never a Control reference or an interrupted frame reference.
use core::{arch::asm,
           fmt, ptr,
           sync::atomic::{AtomicBool, AtomicU64, Ordering}};

use mac_abi::{BootInfo, BootMemoryRange,
              boot::{CheckedMemory,
                     monitor::{self, BootImage, Control, Entry, SecondaryStart},
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
    timebase_hz: u64,
    image:       &'static BootImage
}
static MONITOR: spin::Once<Admitted> = spin::Once::new();
static ISSUING: AtomicBool = AtomicBool::new(false);
static BROKEN: AtomicBool = AtomicBool::new(false);
static TERMINAL: AtomicBool = AtomicBool::new(false);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
/// Notification refusal retains the original typed failure without allocating
/// in a cleanup producer. MMIO has no recoverable completion result: only the
/// eventual S ACK establishes receiver observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WakeError
{
    Missing,
    Binding
    {
        hart: u64
    },
    ForeignPrimary
    {
        hart: u64
    },
    ReceiverEnabled
    {
        hart: u64
    },
    Stopping,
    TransportRetired,
    EpochExhausted
    {
        hart: u64
    }
}
impl fmt::Display for WakeError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Missing => out.write_str("primary wake has no admitted monitor"),
            Self::Binding { hart } =>
            {
                write!(out, "wake hart {hart} lost its actual JOIN/control binding")
            }
            Self::ForeignPrimary { hart } =>
            {
                write!(out, "hart {hart} is not the admitted primary wake receiver")
            }
            Self::ReceiverEnabled { hart } =>
            {
                write!(out, "hart {hart} primary wake admission requires masked interrupts")
            }
            Self::Stopping => out.write_str("primary wake retained during terminal stop"),
            Self::TransportRetired => out.write_str("primary wake transport is retired"),
            Self::EpochExhausted { hart } =>
            {
                write!(out, "hart {hart} wake epoch is exhausted; reset required")
            }
        }
    }
}
/// The actual primary receiver is prepared masked, then published only after
/// first-frame ownership commits. This capability cannot target a numeric ID.
pub(crate) struct PrimaryWake
{
    key:   super::traps::execution::HartKey,
    entry: &'static Entry
}
impl PrimaryWake
{
    pub(crate) fn is_target(&self, key: super::traps::execution::HartKey) -> bool
    {
        self.key == key
    }

    pub(crate) fn post(&self) -> Result<(), WakeError>
    {
        let transport = MONITOR.get().ok_or(WakeError::Missing)?;
        // Only a genuinely selected HOT row may issue a general wake. Cold
        // producers retain work in the caller until this capability publishes.
        let sender = super::traps::execution::selected_hart();
        checked_wake_entry(transport, sender.binding())?;
        checked_wake_entry(transport, self.key.binding())?;
        post_wake(self.entry).map(|_| ())
    }
}
fn checked_wake_entry(transport: &'static Admitted, key: super::traps::execution::HartBinding)
                      -> Result<&'static Entry, WakeError>
{
    let bad = || WakeError::Binding { hart: key.number() as u64 };
    let entry = transport.entries.get(key.ordinal()).ok_or_else(bad)?;
    if entry.hart != key.number() as u64 || key.is_primary() != (key.number() == transport.primary)
    {
        return Err(bad());
    }
    let joined = field(entry, core::mem::offset_of!(Control, joined)).load(Ordering::Acquire);
    if if key.is_primary()
    {
        joined != monitor::JOIN_PRIMARY
    }
    else
    {
        joined != monitor::JOIN_PARKED || start_phase(entry) != monitor::START_ADMITTED_IDLE
    }
    {
        return Err(bad());
    }
    Ok(entry)
}
pub(crate) fn prepare_primary_wake() -> Result<PrimaryWake, WakeError>
{
    let transport = MONITOR.get().ok_or(WakeError::Missing)?;
    let key = super::traps::execution::selected_hart();
    if !key.is_primary() || !key.dispatch_eligible()
    {
        return Err(WakeError::ForeignPrimary { hart: key.number() as u64 });
    }
    let entry = checked_wake_entry(transport, key.binding())?;
    if TERMINAL.load(Ordering::Acquire)
    {
        return Err(WakeError::Stopping);
    }
    let status: usize;
    // Receiver admission must not expose an interrupt before first-frame
    // ownership commits; preserve the caller's masked startup invariant.
    unsafe {
        asm!("csrr {status}, sstatus", status = out(reg) status, options(nostack));
    }
    if status & 2 != 0
    {
        return Err(WakeError::ReceiverEnabled { hart: key.number() as u64 });
    }
    // This runs once, SIE masked, before the first scheduled claim. Preserve
    // STIE and all other local interrupt enables; no epoch or self-IPI here.
    unsafe {
        asm!("csrs sie, {ssie}", ssie = in(reg) 2usize, options(nostack));
    }
    Ok(PrimaryWake { key,
                     entry })
}
fn post_wake(entry: &Entry) -> Result<u64, WakeError>
{
    if TERMINAL.load(Ordering::Acquire)
    {
        return Err(WakeError::Stopping);
    }
    if BROKEN.load(Ordering::Acquire)
    {
        return Err(WakeError::TransportRetired);
    }
    // CAS on the TARGET cell prevents a delayed sender from overwriting a
    // newer intent. Startup and general wakes use this same monotonic path.
    let sequence = field(entry, core::mem::offset_of!(Control, wake_requested))
        .try_update(Ordering::AcqRel, Ordering::Acquire, |old| old.checked_add(1))
        .map_err(|_| WakeError::EpochExhausted { hart: entry.hart })? + 1;
    // The durable intent exists before MMIO. Posting is not an S ACK; a fault
    // or absent receiver cannot refund work or manufacture completion.
    notify(entry);
    Ok(sequence)
}
#[derive(Debug)]
pub(crate) enum Error
{
    Memory(mac_abi::boot::MemoryError),
    Directory(monitor::Error),
    Physical(mac_kernel_model::pages::physical::Error),
    AlreadyAdmitted,
    Layout,
    Image,
    Entry(super::traps::EntryError),
    StartBinding
    {
        hart: u64
    },
    StartRefused
    {
        hart:     u64,
        step:     u64,
        observed: u64,
        expected: u64
    },
    WakeBinding
    {
        hart: u64
    },
    WakeOrder
    {
        hart:         u64,
        requested:    u64,
        acknowledged: u64
    },
    Wake(WakeError),
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
            Self::Wake(error) => error.fmt(f),
            Self::Memory(error) => error.fmt(f),
            Self::Directory(error) => error.fmt(f),
            Self::Physical(error) => error.fmt(f),
            Self::Layout =>
            {
                f.write_str("machine monitor layout fingerprint disagrees; rebuild ROM and System \
                             together")
            }
            Self::Image =>
            {
                f.write_str("machine monitor sealed System image/profile disagrees with the \
                             actual native boot")
            }
            Self::Entry(cause) => write!(f, "secondary core admission refused: {cause}"),
            Self::StartBinding { hart } =>
            {
                write!(f,
                       "hart {hart} secondary payload/sequence/phase is not its genuine prepared \
                        publication")
            }
            Self::StartRefused { hart,
                                 step,
                                 observed,
                                 expected } =>
            {
                write!(f,
                       "hart {hart} M admission refused step {step}: observed {observed:#x}, \
                        required {expected:#x}; backing retained until reset")
            }
            Self::WakeBinding { hart } =>
            {
                write!(f, "hart {hart} S wake is not bound to its actual admitted monitor row")
            }
            Self::WakeOrder { hart,
                              requested,
                              acknowledged } =>
            {
                write!(f,
                       "hart {hart} wake epoch regressed: requested {requested}, acknowledged \
                        {acknowledged}")
            }
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
            Self::Wake(cause) =>
            {
                match cause
                {
                    WakeError::Stopping => mac_abi::OPERATION_CANCELLED,
                    WakeError::TransportRetired => mac_abi::IO_ERROR,
                    WakeError::EpochExhausted { .. } => mac_abi::NOT_SUPPORTED,
                    WakeError::ForeignPrimary { .. } => mac_abi::ACCESS_DENIED,
                    WakeError::Missing
                    | WakeError::Binding { .. }
                    | WakeError::ReceiverEnabled { .. } => mac_abi::BAD_ABI
                }
            }
            Self::Busy => mac_abi::SERVICE_BUSY,
            Self::ForeignHart => mac_abi::ACCESS_DENIED,
            Self::Stopping => mac_abi::OPERATION_CANCELLED,
            Self::Uncertain { .. }
            | Self::StopUnacknowledged { .. }
            | Self::TransportRetired
            | Self::ClockOverflow => mac_abi::IO_ERROR,
            Self::SequenceExhausted | Self::StartRefused { .. } => mac_abi::NOT_SUPPORTED,
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
    if info.monitor_layout != monitor::LAYOUT_FINGERPRINT
    {
        return Err(Error::Layout);
    }
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
    let image =
        unsafe { super::memory::primary_slice(info, directory.image, 1) }.map_err(Error::Memory)?
                                                                         .first()
                                                                         .ok_or(Error::Image)?;
    let execution =
        unsafe { super::memory::primary_slice(info, info.execution, 1) }.map_err(Error::Memory)?
                                                                        .first()
                                                                        .ok_or(Error::Image)?;
    if image.sealed.load(Ordering::Acquire) != 1
    {
        return Err(Error::Image);
    }
    let image_base = image.image_base.load(Ordering::Acquire);
    let image_bytes = image.image_bytes.load(Ordering::Acquire);
    let entry = image.entry.load(Ordering::Acquire);
    let vector = image.vector_bytes.load(Ordering::Acquire);
    let mask = image.hardware_tag_mask.load(Ordering::Acquire);
    if image_base != execution.virtual_image.base
       || image_bytes != execution.virtual_image.length
       || image_base.checked_add(image_bytes).is_none_or(|end| entry < image_base || entry >= end)
       || entry & 1 != 0
       || image.required_isa.load(Ordering::Acquire) != monitor::REQUIRED_ISA_BITS
       || vector < 16
       || vector > 8192
       || !vector.is_power_of_two()
       || mask != execution.hardware_tag_mask
       || mask > 0xffff
       || !(mask + 1).is_power_of_two()
       || image.physical_limit.load(Ordering::Acquire) != physical_limit(memory)?
    {
        return Err(Error::Image);
    }
    for address in
        [image.fault_entry.load(Ordering::Acquire), image.returned_entry.load(Ordering::Acquire)]
    {
        if address & 1 != 0 || address < FLASH_BASE || address >= FLASH_BASE + FLASH_BYTES
        {
            return Err(Error::Image);
        }
    }
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
                          timebase_hz: directory.timebase_hz,
                          image }
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
/// Convert actual counter intervals/instants using the admitted DT frequency.
/// Instant/interval conversion floors fractional microseconds; deadline
/// conversion below rounds up. There is no second bootstrap clock record.
pub(crate) fn ticks_to_microseconds(ticks: u64) -> Result<u64, Error>
{
    let transport = MONITOR.get().ok_or(Error::Missing)?;
    let micros = (u128::from(ticks) * 1_000_000) / u128::from(transport.timebase_hz);
    u64::try_from(micros).map_err(|_| Error::ClockOverflow)
}

pub(crate) fn ticks_to_nanoseconds(ticks: u64) -> Result<u64, Error>
{
    let transport = MONITOR.get().ok_or(Error::Missing)?;
    let nanos = (u128::from(ticks) * 1_000_000_000) / u128::from(transport.timebase_hz);
    u64::try_from(nanos).map_err(|_| Error::ClockOverflow)
}

/// Classic event waits are expressed in sixtieths of a second. Convert once
/// against the admitted clock, avoiding a machine-specific 10 MHz constant or
/// intermediate microsecond rounding.
pub(crate) fn event_ticks_to_ticks(ticks: u32) -> Result<u64, Error>
{
    let transport = MONITOR.get().ok_or(Error::Missing)?;
    let duration = (u128::from(ticks) * u128::from(transport.timebase_hz)).div_ceil(60);
    u64::try_from(duration).map_err(|_| Error::ClockOverflow)
}

/// A measured callback duration rounds upward for its strict time budget; a
/// fractional microsecond beyond that budget cannot disappear by flooring.
pub(crate) fn ticks_to_microseconds_ceil(ticks: u64) -> Result<u64, Error>
{
    let transport = MONITOR.get().ok_or(Error::Missing)?;
    let micros = (u128::from(ticks) * 1_000_000).div_ceil(u128::from(transport.timebase_hz));
    u64::try_from(micros).map_err(|_| Error::ClockOverflow)
}

/// Deadline/quantum conversion rounds up, never silently arms a shorter
/// interval. This uses the same admitted counter as execution observations.
pub(crate) fn microseconds_to_ticks_ceil(microseconds: u64) -> Result<u64, Error>
{
    let transport = MONITOR.get().ok_or(Error::Missing)?;
    let ticks = (u128::from(microseconds) * u128::from(transport.timebase_hz)).div_ceil(1_000_000);
    u64::try_from(ticks).map_err(|_| Error::ClockOverflow)
}

/// Read-only public CPU metadata uses checked topology position/label, never
/// these scalar arguments as execution authority. No Control view is formed.
pub(crate) fn cpu_observation(ordinal: usize, hardware: u64) -> Option<(u64, bool)>
{
    let transport = MONITOR.get()?;
    let entry = transport.entries.get(ordinal)?;
    if entry.hart != hardware
    {
        return None;
    }
    let stopped = field(entry, core::mem::offset_of!(Control, stopped)).load(Ordering::Acquire);
    if stopped > 1
    {
        return None;
    }
    Some((transport.timebase_hz, stopped == 1))
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
        // Published start claims remain retained by PendingHart/registry until
        // reset; this guard never disposes an uncertain execution owner.
        if !self.complete
        {
            BROKEN.store(true, Ordering::Release);
        }
        ISSUING.store(false, Ordering::Release);
    }
}
/// Called after image writes and before any executable binding is published.
/// The actual image owner remains pinned across this call and any refusal.
/// Every discovered M monitor already joined. Later one-shot S admission
/// preserves that same frozen hardware membership and uses the shared transport.
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
pub(super) fn stop_peers(winner: &super::traps::execution::TerminalWinner) -> Result<(), Error>
{
    let monitor = MONITOR.get().ok_or(Error::Missing)?;
    // The terminal claim carries a genuine current cold/hot selection. Do not
    // enter the ordinary Cold.control lock while that failed execution is
    // suspended; the claim revalidates exact sscratch/binding without a lock.
    if !winner.is_current()
    {
        return Err(Error::ForeignHart);
    }
    let current = winner.number();
    if !monitor.entries.iter().any(|entry| entry.hart == current as u64)
    {
        return Err(Error::ForeignHart);
    }
    TERMINAL.store(true, Ordering::Release);
    let deadline = now().checked_add(monitor.timebase_hz).ok_or(Error::ClockOverflow)?;
    for entry in monitor.entries.iter().filter(|entry| entry.hart != current as u64)
    {
        field(entry, core::mem::offset_of!(Control, stop)).store(1, Ordering::Release);
        notify(entry);
    }
    for entry in monitor.entries.iter().filter(|entry| entry.hart != current as u64)
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

fn physical_limit(memory: CheckedMemory<'_>) -> Result<u64, Error>
{
    let mut end = FLASH_BASE.checked_add(FLASH_BYTES).ok_or(Error::Image)?;
    for bank in memory.banks()
    {
        end = end.max(bank.base.checked_add(bank.length).ok_or(Error::Image)?);
    }
    for &(_, limit) in BOOT_MMIO_REGIONS
    {
        end = end.max(limit);
    }
    for &(_, _, base, bytes, _, _) in super::power::platform::PLATFORM_MMIO_DEVICES
    {
        end = end.max(base.checked_add(bytes).ok_or(Error::Image)?);
    }
    Ok(end)
}
fn start_field(entry: &Entry, offset: usize) -> &'static AtomicU64
{
    field(entry, core::mem::offset_of!(Control, start) + offset)
}
fn start_phase(entry: &Entry) -> u64
{
    start_field(entry, core::mem::offset_of!(SecondaryStart, phase)).load(Ordering::Acquire)
}
fn start_refusal(entry: &Entry) -> Error
{
    Error::StartRefused {hart:entry.hart,
        step:start_field(entry,core::mem::offset_of!(SecondaryStart,step)).load(Ordering::Acquire),
        observed:start_field(entry,core::mem::offset_of!(SecondaryStart,observed)).load(Ordering::Acquire),
        expected:start_field(entry,core::mem::offset_of!(SecondaryStart,observed_aux)).load(Ordering::Acquire)}
}
fn payload(entry: &Entry) -> *const SecondaryStart
{
    // Numeric permanent mapped address only. No &Control or payload borrow is formed.
    (control(entry) as *const u8).wrapping_add(core::mem::offset_of!(Control, start)).cast()
}
/// A copied report carrying its private actual publication binding. It cannot be
/// constructed from caller-supplied fields and never lends the wire allocation.
pub(crate) struct StartReport
{
    pub(crate) root_register:     u64,
    pub(crate) control_virtual:   u64,
    pub(crate) service_top:       u64,
    pub(crate) vector_bytes:      u64,
    pub(crate) hardware_tag_mask: u64,
    pub(crate) pmp_address:       u64,
    pub(crate) pmp_configuration: u64,
    entry:                        &'static Entry,
    sequence:                     u64
}
pub(crate) fn validate_start(proof: &super::traps::StartingHart, pointer: *const SecondaryStart,
                             bytes: usize)
                             -> Result<StartReport, Error>
{
    let transport = MONITOR.get().ok_or(Error::Missing)?;
    let entry =
        transport.entries.get(proof.ordinal()).ok_or(Error::StartBinding { hart: u64::MAX })?;
    if pointer!=payload(entry) || bytes!=core::mem::size_of::<SecondaryStart>()
        || start_phase(entry)!=monitor::START_STARTING
        || start_field(entry,core::mem::offset_of!(SecondaryStart,sequence)).load(Ordering::Acquire)!=proof.sequence()
        || proof.sequence()==0 { return Err(Error::StartBinding{hart:entry.hart}); }
    let report=StartReport {
        root_register:start_field(entry,core::mem::offset_of!(SecondaryStart,root_register)).load(Ordering::Relaxed),
        control_virtual:start_field(entry,core::mem::offset_of!(SecondaryStart,control_virtual)).load(Ordering::Relaxed),
        service_top:start_field(entry,core::mem::offset_of!(SecondaryStart,service_top)).load(Ordering::Relaxed),
        vector_bytes:start_field(entry,core::mem::offset_of!(SecondaryStart,vector_bytes)).load(Ordering::Relaxed),
        hardware_tag_mask:start_field(entry,core::mem::offset_of!(SecondaryStart,hardware_tag_mask)).load(Ordering::Relaxed),
        pmp_address:start_field(entry,core::mem::offset_of!(SecondaryStart,pmp_address)).load(Ordering::Relaxed),
        pmp_configuration:start_field(entry,core::mem::offset_of!(SecondaryStart,pmp_configuration)).load(Ordering::Relaxed),
        entry,sequence:proof.sequence()
    };
    let end = transport.image.physical_limit.load(Ordering::Acquire);
    if report.vector_bytes != transport.image.vector_bytes.load(Ordering::Acquire)
       || report.hardware_tag_mask != transport.image.hardware_tag_mask.load(Ordering::Acquire)
       || report.pmp_configuration & 255 != 0x1f
       || report.pmp_address & report.pmp_address.wrapping_add(1) != 0
       || end == 0
       || (end - 1) >> 3 > report.pmp_address
       || start_field(entry, core::mem::offset_of!(SecondaryStart, step)).load(Ordering::Relaxed)
          != 0
    {
        return Err(Error::StartBinding { hart: entry.hart });
    }
    Ok(report)
}
pub(crate) fn finish_start(proof: &super::traps::StartingHart, report: StartReport)
                           -> Result<(), Error>
{
    let transport = MONITOR.get().ok_or(Error::Missing)?;
    if !transport.entries.get(proof.ordinal()).is_some_and(|entry|core::ptr::eq(entry,report.entry))
        || proof.sequence()!=report.sequence || start_phase(report.entry)!=monitor::START_STARTING
        || start_field(report.entry,core::mem::offset_of!(SecondaryStart,sequence)).load(Ordering::Acquire)!=report.sequence
    { return Err(Error::StartBinding{hart:report.entry.hart}); }
    start_field(report.entry,core::mem::offset_of!(SecondaryStart,phase))
        .store(monitor::START_ADMITTED_IDLE,Ordering::Release);
    Ok(())
}
/// Consumes real native preparation on success. Every prepublication refusal
/// returns that unique owner, allowing its unissued claim to refund normally.
pub(crate) struct StartRefusal
{
    pub(crate) cause:    Error,
    pub(crate) prepared: super::traps::PreparedHart
}
/// Retains the actual published claim on timeout/refusal/drop. No Drop path
/// reports that a potentially running CPU or canonical idle record was freed.
pub(crate) struct PendingHart
{
    prepared: super::traps::PreparedHart,
    entry:    &'static Entry,
    sequence: u64,
    deadline: u64,
    issuer:   Option<Issuer>,
    result:   Option<i64>,
    wake:     Option<u64>
}
pub(crate) fn begin_secondary(mut prepared: super::traps::PreparedHart)
                              -> Result<PendingHart, StartRefusal>
{
    let preflight = (|| {
        let transport = selected()?;
        if TERMINAL.load(Ordering::Acquire)
        {
            return Err(Error::Stopping);
        }
        if BROKEN.load(Ordering::Acquire)
        {
            return Err(Error::TransportRetired);
        }
        let entry = transport.entries
                             .get(prepared.ordinal())
                             .ok_or(Error::StartBinding { hart: prepared.number() as u64 })?;
        if entry.hart!=prepared.number() as u64
            || entry.hart==transport.primary as u64
            || field(entry,core::mem::offset_of!(Control,joined)).load(Ordering::Acquire)!=monitor::JOIN_PARKED
            || start_phase(entry)!=monitor::START_PREPARED
            || start_field(entry,core::mem::offset_of!(SecondaryStart,sequence)).load(Ordering::Acquire)!=0
        {return Err(Error::StartBinding{hart:entry.hart});}
        ISSUING.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
               .map_err(|_| Error::Busy)?;
        let issuer = Issuer { complete: true };
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
        let deadline = now().checked_add(transport.timebase_hz).ok_or(Error::ClockOverflow)?;
        Ok((entry, sequence, deadline, issuer))
    })();
    let (entry, sequence, deadline, mut issuer) = match preflight
    {
        Ok(value) => value,
        Err(cause) =>
        {
            return Err(StartRefusal { cause,
                                      prepared });
        }
    };
    // Reauthenticate and bind on the actual primary in the short native
    // publication tail. No migration, callback, allocation or owning Drop can
    // occur between this irreversible mark and its request/MMIO publication.
    let published = super::interrupts::with_masked(|| {
        selected()?;
        if TERMINAL.load(Ordering::Acquire)
        {
            return Err(Error::Stopping);
        }
        prepared.bind_sequence(sequence).map_err(Error::Entry)?;
        start_field(entry,core::mem::offset_of!(SecondaryStart,root_register)).store(prepared.root_register() as u64,Ordering::Relaxed);
        start_field(entry,core::mem::offset_of!(SecondaryStart,control_virtual)).store(prepared.control() as u64,Ordering::Relaxed);
        start_field(entry,core::mem::offset_of!(SecondaryStart,service_top)).store(prepared.service_top() as u64,Ordering::Relaxed);
        start_field(entry, core::mem::offset_of!(SecondaryStart, sequence)).store(sequence,
                                                                                  Ordering::Relaxed);
        field(entry, core::mem::offset_of!(Control, command)).store(monitor::START,
                                                                    Ordering::Relaxed);
        SEQUENCE.store(sequence, Ordering::Relaxed);
        issuer.complete = false;
        prepared.mark_published();
        field(entry, core::mem::offset_of!(Control, requested)).store(sequence, Ordering::Release);
        notify(entry);
        Ok::<(), Error>(())
    });
    if let Err(cause) = published
    {
        return Err(StartRefusal { cause,
                                  prepared });
    }
    Ok(PendingHart { prepared,
                     entry,
                     sequence,
                     deadline,
                     issuer: Some(issuer),
                     result: None,
                     wake: None })
}
impl PendingHart
{
    pub(crate) fn number(&self) -> usize
    {
        self.prepared.number()
    }

    /// True only after real S admission AND a genuine post-entry S interrupt
    /// acknowledgment. M acceptance/injection alone never means core readiness.
    pub(crate) fn poll(&mut self) -> Result<bool, Error>
    {
        selected()?;
        if TERMINAL.load(Ordering::Acquire)
        {
            return Err(Error::Stopping);
        }
        if self.issuer.is_some()
        {
            if field(self.entry,core::mem::offset_of!(Control,acknowledged)).load(Ordering::Acquire)!=self.sequence {
                if now()>=self.deadline {return Err(Error::Uncertain{hart:self.entry.hart,sequence:self.sequence});}
                return Ok(false);
            }
            // Capture this command's terminal result BEFORE releasing the
            // shared issuer. A later FenceI may reuse the same result cell.
            let result =
                unsafe { &*ptr::addr_of!((*control(self.entry)).result) }.load(Ordering::Relaxed);
            self.result = Some(result);
            if let Some(mut issuer) = self.issuer.take()
            {
                issuer.complete = true;
                drop(issuer);
            }
        }
        match self.result
        {
            Some(0) =>
            {}
            Some(status) =>
            {
                return Err(if start_phase(self.entry) == monitor::START_REFUSED
                           {
                               start_refusal(self.entry)
                           }
                           else
                           {
                               Error::Refused { hart: self.entry.hart,
                                                status }
                           });
            }
            None => return Err(Error::StartBinding { hart: self.entry.hart })
        }
        match start_phase(self.entry)
        {
            monitor::START_REFUSED => return Err(start_refusal(self.entry)),
            monitor::START_STARTING =>
            {}
            monitor::START_ADMITTED_IDLE =>
            {
                let sequence = if let Some(sequence) = self.wake
                {
                    sequence
                }
                else
                {
                    let sequence = post_wake(self.entry).map_err(Error::Wake)?;
                    self.wake = Some(sequence);
                    sequence
                };
                if field(self.entry,core::mem::offset_of!(Control,wake_acknowledged)).load(Ordering::Acquire)>=sequence {return Ok(true);}
            }
            _ => return Err(Error::StartBinding { hart: self.entry.hart })
        }
        if now() >= self.deadline
        {
            return Err(Error::Uncertain { hart:     self.entry.hart,
                                          sequence: self.sequence });
        }
        Ok(false)
    }
}
/// Called only after clearing local SSIP and ending the live mutable-frame
/// borrow. Uses genuine current-row authority, never a caller numeric label.
pub(crate) fn acknowledge_wake(key: super::traps::execution::HartKey) -> Result<bool, Error>
{
    let transport = MONITOR.get().ok_or(Error::Missing)?;
    if key != super::traps::execution::selected_hart()
    {
        return Err(Error::ForeignHart);
    }
    if TERMINAL.load(Ordering::Acquire)
    {
        return Err(Error::Stopping);
    }
    let entry = transport.entries
                         .get(key.ordinal())
                         .ok_or(Error::WakeBinding { hart: key.number() as u64 })?;
    if entry.hart != key.number() as u64
       || (key.number() != transport.primary && start_phase(entry) != monitor::START_ADMITTED_IDLE)
    {
        return Err(Error::WakeBinding { hart: key.number() as u64 });
    }
    let requested =
        field(entry, core::mem::offset_of!(Control, wake_requested)).load(Ordering::Acquire);
    let acknowledged =
        field(entry, core::mem::offset_of!(Control, wake_acknowledged)).load(Ordering::Relaxed);
    if requested < acknowledged
    {
        return Err(Error::WakeOrder { hart: entry.hart,
                                      requested,
                                      acknowledged });
    }
    if requested == acknowledged
    {
        return Ok(false);
    }
    field(entry, core::mem::offset_of!(Control, wake_acknowledged)).store(requested,
                                                                          Ordering::Release);
    Ok(true)
}

/// Unique core-readiness receipt. Construction requires both hardware admission
/// and an actual nonzero post-entry S interrupt ACK. Numeric metadata and the
/// observational poll result cannot construct this proof or enable dispatch.
pub(crate) struct ReadyHart
{
    prepared:   super::traps::PreparedHart,
    sequence:   u64,
    wake_epoch: u64
}
impl ReadyHart
{
    /// Data binding derives from the retained hardware admission receipt. It
    /// cannot select that CPU or substitute for this unique readiness owner.
    pub(crate) fn binding(&self) -> super::traps::execution::HartBinding
    {
        self.prepared.binding()
    }

    /// General work notification through the same monotonic target epoch used
    /// for startup. A ready receipt and the genuine sender are both required.
    pub(crate) fn post_work(&self) -> Result<(), WakeError>
    {
        let transport = MONITOR.get().ok_or(WakeError::Missing)?;
        checked_wake_entry(transport, super::traps::execution::selected_hart().binding())?;
        let entry = checked_wake_entry(transport, self.binding())?;
        post_wake(entry).map(|_| ())
    }

    pub(crate) fn ordinal(&self) -> usize
    {
        self.prepared.ordinal()
    }

    pub(crate) fn number(&self) -> usize
    {
        self.prepared.number()
    }

    pub(crate) fn sequence(&self) -> u64
    {
        self.sequence
    }

    pub(crate) fn wake_epoch(&self) -> u64
    {
        self.wake_epoch
    }
}
impl PendingHart
{
    /// Preserve the real pending owner on every refusal. No readiness boolean,
    /// row label or unissued preparation can mint a ReadyHart.
    pub(crate) fn into_ready(mut self) -> Result<ReadyHart, (Error, Self)>
    {
        match self.poll()
        {
            Ok(true) =>
            {}
            Ok(false) => return Err((Error::Busy, self)),
            Err(cause) => return Err((cause, self))
        }
        let Some(wake_epoch) = self.wake.filter(|epoch| *epoch != 0)
        else
        {
            return Err((Error::StartBinding { hart: self.entry.hart }, self));
        };
        Ok(ReadyHart { prepared: self.prepared,
                       sequence: self.sequence,
                       wake_epoch })
    }
}

/// SSIP Stopping and distinct-peer terminal-claim loser continuation. A loser
/// may arrive just before the winner publishes TERMINAL/stop intent; it waits
/// through that bounded publication gap without killing the winner. A peer
/// never resumes an author or attempts to access the winner's terminal kit.
/// The actual higher-privilege MSIP handler owns stop acknowledgment. S only
/// waits a bounded interval, then reports failure through independent hardware.
pub(crate) fn wait_for_terminal_stop(key: super::traps::execution::HartKey) -> !
{
    // SAFETY: local terminal path never restores S execution. Higher-privilege
    // machine software interrupts remain deliverable with SIE clear.
    unsafe {
        asm!("csrci sstatus,2", options(nostack));
    }
    let deadline = MONITOR.get()
                          .and_then(|transport| {
                              (key == super::traps::execution::selected_hart()).then_some(transport)
                          })
                          .and_then(|transport| transport.timebase_hz.checked_mul(2))
                          .and_then(|ticks| now().checked_add(ticks));
    if let Some(deadline) = deadline
    {
        while now() < deadline
        {
            core::hint::spin_loop();
        }
    }
    crate::machine::emergency::serial(format_args!("TERMINAL_PEER_STOP_FAILED hart={}: no \
                                                    actual machine stop arrived within the \
                                                    admitted bound; direct shutdown\n",
                                                   key.number()));
    crate::machine::power::fatal_shutdown()
}
