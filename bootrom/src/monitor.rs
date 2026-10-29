//! Custom-ROM parked machine monitor. All returning interrupt code is leaf
//! integer-only assembly; Rust is used only for primary cold preparation.
use alloc::{alloc::{alloc_zeroed, dealloc},
            vec::Vec};
use core::{alloc::Layout,
           arch::{asm, global_asm},
           fmt,
           ptr::{self, NonNull},
           sync::atomic::{AtomicU64, Ordering}};

use mac_abi::{BootMemoryRange,
              boot::{monitor::{Control, Directory, Entry},
                     topology::Cpu}};
use mac_formats::{Error as FormatError, Result as FormatResult};

use crate::memory::*;

#[repr(C)]
struct Anchor
{
    entries: AtomicU64,
    count:   AtomicU64
}
#[unsafe(no_mangle)]
static __monitor_directory: Anchor = Anchor { entries: AtomicU64::new(0),
                                              count:   AtomicU64::new(0) };

struct Storage
{
    base:   NonNull<u8>,
    layout: Layout
}
impl Storage
{
    fn new() -> FormatResult<Self>
    {
        let layout = Layout::from_size_align(MONITOR_STACK_BYTES as usize, PAGE_BYTES as usize)
            .map_err(|_| FormatError::Malformed("invalid checked machine monitor storage layout"))?;
        // SAFETY: the firmware allocator is initialized and admitted before this
        // primary-only preparation; no monitor is published yet.
        let base = NonNull::new(unsafe { alloc_zeroed(layout) })
            .ok_or(FormatError::AllocationFailed("reserved firmware capacity cannot hold per-hart machine monitors"))?;
        Ok(Self { base,
                  layout })
    }
}
impl Drop for Storage
{
    fn drop(&mut self)
    {
        // Only unpublished preparations drop. Published storage is explicitly
        // leaked until whole-machine reset, including refused Join attempts.
        unsafe {
            dealloc(self.base.as_ptr(), self.layout);
        }
    }
}
#[derive(Debug)]
pub(crate) enum Error
{
    Format(FormatError),
    Memory(mac_abi::boot::MemoryError),
    Directory(mac_abi::boot::monitor::Error),
    PrimaryMissing
    {
        hart: u64
    },
    Join
    {
        hart: u64
    },
    DeadlineOverflow
    {
        hart: u64
    },
    Registers
    {
        hart:       u64,
        scratch:    u64,
        vector:     u64,
        interrupts: u64
    }
}
impl From<FormatError> for Error
{
    fn from(error: FormatError) -> Self
    {
        Self::Format(error)
    }
}
impl From<mac_abi::boot::MemoryError> for Error
{
    fn from(error: mac_abi::boot::MemoryError) -> Self
    {
        Self::Memory(error)
    }
}
impl From<mac_abi::boot::monitor::Error> for Error
{
    fn from(error: mac_abi::boot::monitor::Error) -> Self
    {
        Self::Directory(error)
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Format(error) => error.fmt(f),
            Self::Memory(error) => error.fmt(f),
            Self::Directory(error) => error.fmt(f),
            Self::PrimaryMissing { hart } =>
            {
                write!(f, "machine monitor primary hart {hart} is absent")
            }
            Self::Join { hart } =>
            {
                write!(f,
                       "machine monitor hart {hart} did not ACK Join; S release remains disabled")
            }
            Self::DeadlineOverflow { hart } =>
            {
                write!(f, "machine monitor hart {hart} Join deadline overflows")
            }
            Self::Registers { hart,
                              scratch,
                              vector,
                              interrupts } =>
            {
                write!(f,
                       "machine monitor hart {hart} CSR admission refused mscratch={scratch:#x} \
                        mtvec={vector:#x} mie={interrupts:#x}")
            }
        }
    }
}
pub(crate) struct Published
{
    pub(crate) directory: Directory
}
impl Published
{
    pub(crate) fn prepare(tree: &bootrom::devicetree::Tree<'_>, cpus: &[Cpu], primary: u64,
                          memory: &crate::memory::Memory)
                          -> core::result::Result<Self, Error>
    {
        let account = mac_abi::boot::Memory { ram: &memory.ram, reservations: &memory.reserved }
            .checked(PAGE_BYTES, memory.ram[0])
            .and_then(|account| account.exclude_mmio(BootMemoryRange { base: CLINT_BASE, length: CLINT_BYTES }))?;
        let routes = tree.monitor_routes(cpus, CLINT_BASE..CLINT_BASE + CLINT_BYTES)?;
        let mut entries = Vec::new();
        let mut storage = Vec::new();
        entries.try_reserve_exact(cpus.len())
               .map_err(|_| FormatError::AllocationFailed("machine monitor directory capacity"))?;
        storage.try_reserve_exact(cpus.len())
               .map_err(|_| FormatError::AllocationFailed("machine monitor ownership capacity"))?;
        for route in &routes.routes
        {
            let backing = Storage::new()?;
            let base = backing.base.as_ptr() as u64;
            // SAFETY: exclusive unpublished aligned storage covers Control; no
            // ordinary &Control is ever formed after assembly publication.
            unsafe {
                (base as *mut Control).write(Control::new(route.hart, route.msip));
            }
            entries.push(Entry { hart:    route.hart,
                                 msip:    route.msip,
                                 storage: BootMemoryRange { base,
                                                            length: MONITOR_STACK_BYTES } });
            storage.push(backing);
        }
        let directory = Directory { entries:     entries.as_ptr(),
                                    count:       entries.len(),
                                    stack_bytes: MONITOR_STACK_BYTES,
                                    timebase_hz: routes.timebase_hz };
        directory.checked(&entries,
                          cpus,
                          account,
                          MONITOR_STACK_BYTES,
                          BootMemoryRange { base:   FIRMWARE_HEAP_BASE,
                                            length: FIRMWARE_HEAP_BYTES },
                          BootMemoryRange { base:   CLINT_BASE,
                                            length: CLINT_BYTES })?;
        let owner = entries.iter()
                           .find(|entry| entry.hart == primary)
                           .ok_or(Error::PrimaryMissing { hart: primary })?;
        // Clear every target before publication. Current board's whole-machine
        // reset already cleared MSIP; this is not a CPU-only restart protocol.
        for entry in &entries
        {
            unsafe {
                ptr::write_volatile(entry.msip as *mut u32, 0);
            }
        }
        let owner_control = owner.storage.base;
        // No fallible allocation remains after publication. These physical
        // owners stay reserved even if a target never acknowledges Join.
        let entries = entries.leak();
        let _storage = storage.leak();
        __monitor_directory.entries.store(entries.as_ptr() as u64, Ordering::Relaxed);
        __monitor_directory.count.store(entries.len() as u64, Ordering::Release);
        // SAFETY: this actual primary owns M-mode setup and its new independent
        // control. Global MIE remains clear throughout cold Rust preparation.
        unsafe {
            asm!("csrw mscratch,{control}", "csrw mtvec,{vector}", "csrs mie,{msie}",
                control=in(reg)owner_control, vector=in(reg)__monitor_vector as *const () as usize,
                msie=in(reg)8usize, options(nostack));
        }
        let scratch: u64;
        let vector: u64;
        let interrupts: u64;
        unsafe {
            asm!("csrr {},mscratch",out(reg)scratch,options(nomem,nostack));
            asm!("csrr {},mtvec",out(reg)vector,options(nomem,nostack));
            asm!("csrr {},mie",out(reg)interrupts,options(nomem,nostack));
        }
        if scratch != owner_control
           || vector != __monitor_vector as *const () as u64
           || interrupts & 8 == 0
        {
            return Err(Error::Registers { hart: primary,
                                          scratch,
                                          vector,
                                          interrupts });
        }
        unsafe {
            (&*ptr::addr_of!((*(owner_control as *const Control)).joined))
                .store(mac_abi::boot::monitor::JOIN_PRIMARY, Ordering::Release);
            asm!("fence iorw,iorw", options(nostack));
        }
        for entry in entries.iter().filter(|entry| entry.hart != primary)
        {
            // First fresh post-reset hardware notification authorizes only
            // reading the published directory and joining an M-mode park.
            unsafe {
                ptr::write_volatile(entry.msip as *mut u32, 1);
            }
        }
        unsafe {
            asm!("fence iorw,iorw", options(nostack));
        }
        for entry in entries.iter().filter(|entry| entry.hart != primary)
        {
            let deadline = now().checked_add(routes.timebase_hz)
                                .ok_or(Error::DeadlineOverflow { hart: entry.hart })?;
            let joined =
                unsafe { &*ptr::addr_of!((*(entry.storage.base as *const Control)).joined) };
            while joined.load(Ordering::Acquire) != mac_abi::boot::monitor::JOIN_PARKED
            {
                if now() >= deadline
                {
                    return Err(Error::Join { hart: entry.hart });
                }
                core::hint::spin_loop();
            }
        }
        Ok(Self { directory })
    }
}
fn now() -> u64
{
    let ticks;
    // Actual M time counter. Frequency comes from this boot's checked DTB.
    unsafe {
        asm!("rdtime {}", out(reg)ticks, options(nomem,nostack));
    }
    ticks
}
unsafe extern "C" {
    fn __monitor_vector();
}
global_asm!(include_str!("monitor.S"),
    stack=const MONITOR_STACK_BYTES,
    frame=const core::mem::offset_of!(Control,frame)+core::mem::offset_of!(mac_abi::boot::monitor::Frame,registers),
    pc=const core::mem::offset_of!(Control,frame)+core::mem::offset_of!(mac_abi::boot::monitor::Frame,pc),
    status=const core::mem::offset_of!(Control,frame)+core::mem::offset_of!(mac_abi::boot::monitor::Frame,status),
    hart=const core::mem::offset_of!(Control,hart), msip=const core::mem::offset_of!(Control,msip),
    anchor_entries=const core::mem::offset_of!(Anchor,entries), anchor_count=const core::mem::offset_of!(Anchor,count),
    entry_bytes=const core::mem::size_of::<Entry>(), entry_shift=const core::mem::size_of::<Entry>().trailing_zeros(),
    entry_hart=const core::mem::offset_of!(Entry,hart), entry_msip=const core::mem::offset_of!(Entry,msip),
    entry_base=const core::mem::offset_of!(Entry,storage), entry_length=const core::mem::offset_of!(Entry,storage)+core::mem::offset_of!(BootMemoryRange,length),
    requested=const core::mem::offset_of!(Control,requested),
    acknowledged=const core::mem::offset_of!(Control,acknowledged),
    stop=const core::mem::offset_of!(Control,stop), stopped=const core::mem::offset_of!(Control,stopped),
    joined=const core::mem::offset_of!(Control,joined), command=const core::mem::offset_of!(Control,command),
    result=const core::mem::offset_of!(Control,result), entered=const core::mem::offset_of!(Control,entered),
    firmware_base=const FIRMWARE_HEAP_BASE, firmware_end=const FIRMWARE_HEAP_BASE+FIRMWARE_HEAP_BYTES,
    clint_base=const CLINT_BASE, clint_end=const CLINT_BASE+mac_abi::boot::monitor::MSIP_BYTES,
    page=const PAGE_BYTES, fence_i=const mac_abi::boot::monitor::FENCE_I,
    join_parked=const mac_abi::boot::monitor::JOIN_PARKED,
    unsupported=const mac_abi::NOT_SUPPORTED,
    uart_base=const UART_BASE,
    fatal_address=const crate::platform::FATAL_FINISHER_ADDRESS,
    fatal_value=const crate::platform::FATAL_FINISHER_FAILURE);
const _: () = assert!(MONITOR_CONTROL_BYTES == core::mem::size_of::<Control>() as u64
                      && MONITOR_STACK_BYTES >= MONITOR_CONTROL_BYTES
                      && MONITOR_STACK_BYTES >= PAGE_BYTES
                      && MONITOR_STACK_BYTES % PAGE_BYTES == 0);
