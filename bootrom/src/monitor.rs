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
              boot::{monitor::{BootImage, Control, Directory, Entry},
                     topology::Cpu}};
use mac_formats::{Error as FormatError, Result as FormatResult};

use crate::memory::*;

#[repr(C)]
struct Anchor
{
    entries: AtomicU64,
    count:   AtomicU64,
    image:   AtomicU64
}
#[unsafe(no_mangle)]
static __monitor_directory: Anchor = Anchor { entries: AtomicU64::new(0),
                                              count:   AtomicU64::new(0),
                                              image:   AtomicU64::new(0) };

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
    ImageAlreadySealed,
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
            Self::ImageAlreadySealed =>
            {
                f.write_str("machine monitor System image was already sealed; reset required")
            }
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
    pub(crate) directory: Directory,
    image:                &'static BootImage
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
        let mut images = Vec::new();
        images.try_reserve_exact(1)
              .map_err(|_| {
                  FormatError::AllocationFailed("machine monitor boot-image seal capacity")
              })?;
        images.push(BootImage::new());
        let directory = Directory { entries:     entries.as_ptr(),
                                    count:       entries.len(),
                                    stack_bytes: MONITOR_STACK_BYTES,
                                    timebase_hz: routes.timebase_hz,
                                    image:       images.as_ptr() };
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
        let images = images.leak();
        let entries = entries.leak();
        let _storage = storage.leak();
        __monitor_directory.image.store(images.as_ptr() as u64, Ordering::Relaxed);
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
        Ok(Self { directory,
                  image: &images[0] })
    }

    /// Last ROM action before the nonreturning primary handoff. Every input
    /// comes from the actual admitted load plan/profile; no callback address or
    /// restartable publication is accepted from S mode.
    pub(crate) fn seal(&self, entry: u64, image: BootMemoryRange, physical_limit: u64,
                       vector_bytes: u64, hardware_tag_mask: u64)
                       -> Result<(), Error>
    {
        if self.image.sealed.load(Ordering::Acquire) != 0
        {
            return Err(Error::ImageAlreadySealed);
        }
        self.image.entry.store(entry, Ordering::Relaxed);
        self.image.image_base.store(image.base, Ordering::Relaxed);
        self.image.image_bytes.store(image.length, Ordering::Relaxed);
        self.image.physical_limit.store(physical_limit, Ordering::Relaxed);
        self.image.required_isa.store(mac_abi::boot::monitor::REQUIRED_ISA_BITS, Ordering::Relaxed);
        self.image.vector_bytes.store(vector_bytes, Ordering::Relaxed);
        self.image.hardware_tag_mask.store(hardware_tag_mask, Ordering::Relaxed);
        self.image.fault_entry.store(crate::supervisor::fault_entry(), Ordering::Relaxed);
        self.image.returned_entry.store(crate::supervisor::returned_entry(), Ordering::Relaxed);
        self.image.sealed.store(1, Ordering::Release);
        Ok(())
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
global_asm!(concat!(include_str!("../../platforms/riscv64-supervisor-probe.S"), include_str!("monitor.S")),
    stack=const MONITOR_STACK_BYTES,
    required_isa=const mac_abi::boot::monitor::REQUIRED_ISA_BITS,
    exceptions=const (1u64<<0)|(1u64<<1)|(1u64<<2)|(1u64<<4)|(1u64<<5)|(1u64<<6)|(1u64<<7)|(1u64<<8)|(1u64<<9)|(1u64<<12)|(1u64<<13)|(1u64<<15),
    primary_timer=const crate::supervisor::PRIMARY_TIMER_OFFSET, primary_timer_valid=const crate::supervisor::PRIMARY_TIMER_VALID_OFFSET,
    step_isa=const mac_abi::boot::monitor::step::ISA,
    step_width=const mac_abi::boot::monitor::step::WIDTH,
    step_environment=const mac_abi::boot::monitor::step::ENVIRONMENT,
    step_counters=const mac_abi::boot::monitor::step::COUNTERS,
    step_delegation=const mac_abi::boot::monitor::step::DELEGATION,
    step_pmp=const mac_abi::boot::monitor::step::PMP,
    step_root=const mac_abi::boot::monitor::step::ROOT,
    step_asid=const mac_abi::boot::monitor::step::ASID,
    step_vector=const mac_abi::boot::monitor::step::VECTOR,
    step_entry=const mac_abi::boot::monitor::step::ENTRY,
    start_step=const core::mem::offset_of!(mac_abi::boot::monitor::SecondaryStart,step),
    start_observed=const core::mem::offset_of!(mac_abi::boot::monitor::SecondaryStart,observed),
    start_aux=const core::mem::offset_of!(mac_abi::boot::monitor::SecondaryStart,observed_aux),
    start_pmp_configuration=const core::mem::offset_of!(mac_abi::boot::monitor::SecondaryStart,pmp_configuration),
    start_pmp_address=const core::mem::offset_of!(mac_abi::boot::monitor::SecondaryStart,pmp_address),
    start_tag_mask=const core::mem::offset_of!(mac_abi::boot::monitor::SecondaryStart,hardware_tag_mask),
    start_vector_bytes=const core::mem::offset_of!(mac_abi::boot::monitor::SecondaryStart,vector_bytes),
    start_sequence=const core::mem::offset_of!(mac_abi::boot::monitor::SecondaryStart,sequence),
    start_root=const core::mem::offset_of!(mac_abi::boot::monitor::SecondaryStart,root_register),
    start_control=const core::mem::offset_of!(mac_abi::boot::monitor::SecondaryStart,control_virtual),
    start_top=const core::mem::offset_of!(mac_abi::boot::monitor::SecondaryStart,service_top),
    start_phase=const core::mem::offset_of!(mac_abi::boot::monitor::SecondaryStart,phase),
    image_isa=const core::mem::offset_of!(BootImage,required_isa),
    image_physical_limit=const core::mem::offset_of!(BootImage,physical_limit),
    image_sealed=const core::mem::offset_of!(BootImage,sealed),
    image_tag_mask=const core::mem::offset_of!(BootImage,hardware_tag_mask),
    image_vector_bytes=const core::mem::offset_of!(BootImage,vector_bytes),
    image_entry=const core::mem::offset_of!(BootImage,entry),
    image_base=const core::mem::offset_of!(BootImage,image_base),
    image_bytes=const core::mem::offset_of!(BootImage,image_bytes),
    image_fault=const core::mem::offset_of!(BootImage,fault_entry),
    image_returned=const core::mem::offset_of!(BootImage,returned_entry),
    start=const core::mem::offset_of!(Control,start),
    start_bytes=const core::mem::size_of::<mac_abi::boot::monitor::SecondaryStart>(),
    start_command=const mac_abi::boot::monitor::START,
    start_prepared=const mac_abi::boot::monitor::START_PREPARED,
    start_starting=const mac_abi::boot::monitor::START_STARTING,
    start_idle=const mac_abi::boot::monitor::START_ADMITTED_IDLE,
    start_refused=const mac_abi::boot::monitor::START_REFUSED,
    secondary_tag=const mac_abi::boot::monitor::ENTRY_SECONDARY,
    wake_requested=const core::mem::offset_of!(Control,wake_requested),
    wake_acknowledged=const core::mem::offset_of!(Control,wake_acknowledged),
    physmap=const crate::virtual_memory::VM_PHYSMAP_START,
    physical_base=const crate::virtual_memory::VM_PHYSMAP_PHYSICAL_BASE,
    image_bytes_total=const core::mem::size_of::<BootImage>(),
    bad_abi=const mac_abi::BAD_ABI,

    frame=const core::mem::offset_of!(Control,frame)+core::mem::offset_of!(mac_abi::boot::monitor::Frame,registers),
    pc=const core::mem::offset_of!(Control,frame)+core::mem::offset_of!(mac_abi::boot::monitor::Frame,pc),
    status=const core::mem::offset_of!(Control,frame)+core::mem::offset_of!(mac_abi::boot::monitor::Frame,status),
    hart=const core::mem::offset_of!(Control,hart), msip=const core::mem::offset_of!(Control,msip),
    anchor_entries=const core::mem::offset_of!(Anchor,entries), anchor_count=const core::mem::offset_of!(Anchor,count),
    anchor_image=const core::mem::offset_of!(Anchor,image),
    entry_bytes=const core::mem::size_of::<Entry>(), entry_shift=const core::mem::size_of::<Entry>().trailing_zeros(),
    entry_hart=const core::mem::offset_of!(Entry,hart), entry_msip=const core::mem::offset_of!(Entry,msip),
    entry_base=const core::mem::offset_of!(Entry,storage), entry_length=const core::mem::offset_of!(Entry,storage)+core::mem::offset_of!(BootMemoryRange,length),
    requested=const core::mem::offset_of!(Control,requested),
    acknowledged=const core::mem::offset_of!(Control,acknowledged),
    stop=const core::mem::offset_of!(Control,stop), stopped=const core::mem::offset_of!(Control,stopped),
    join_primary=const mac_abi::boot::monitor::JOIN_PRIMARY,
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
