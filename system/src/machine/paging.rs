//! RV64 supervisor bootstrap and permanently owned translation roots. No
//! existing reference changes its address; every RAM view starts in physmap.
pub(crate) mod inspection;
mod alias;
mod heap_partition;
use core::{arch::asm, fmt, marker::PhantomData, ptr};

use heap_partition::HeapPartition;
pub(crate) use heap_partition::{ProtectedHeapStatistics, ProtectedRange};
use mac_abi::{BootInfo, BootMemoryRange,
              boot::{CheckedMemory, MemoryError,
                     execution::{self, EXECUTE, FIRMWARE_ONLY, READ, Region, WRITE},
                     topology}};
use mac_kernel_model::{DomainId,
                       pages::{Access, AddressSpace, Architecture, AsidTag, PageBox,
                               PageGeometry, PageLayout, PageOwnership, Permissions,
                               PhysicalAddress, VirtualAddress,
                               physical::{DirectMap, Error as PhysicalError}},
                       policy::{AddressRange, ModelError}};
use mac_riscv_pages::{Entry, Sv39,
                      mapping::{MapError, Mapper, Tables}};

use super::{interrupts::{self, NativeInvocationGate},
            memory::*,
            pages::{ArrayError, Backing, OwnedArray, Stack}};
use crate::allocator::foundation::{self, Foundation};
const MODE: usize = 8usize << 60;
const PPN: usize = (1usize << 44) - 1;
type RootPage = PageBox<'static, 'static, [u64; 512], NativeInvocationGate>;
static BOOT: spin::Once<Bootstrap> = spin::Once::new();
static SPACES: spin::Once<Spaces> = spin::Once::new();
struct Bootstrap
{
    ram:            DirectMap<'static>,
    memory:         CheckedMemory<'static>,
    regions:        &'static [Region],
    topology:       topology::Topology<'static>,
    root:           usize,
    hart:           usize,
    tag_mask:       u16,
    image_physical: BootMemoryRange,
    image_virtual:  BootMemoryRange
}
#[derive(Debug)]
pub(crate) enum Error
{
    Memory(MemoryError),
    Execution(execution::Error),
    Topology(topology::Error),
    Monitor(super::monitor::Error),
    Physical(PhysicalError),
    Policy(ModelError),
    StackStorage(ArrayError<foundation::Error>),
    StackSlots
    {
        count: usize
    },
    HeapPartition(heap_partition::Error),
    Tables(MapError<FrameError>),
    Missing,
    AlreadyActive,
    StartingRoot
    {
        expected: usize,
        observed: usize
    },
    ForeignHart,
    Admission
    {
        root:   usize,
        status: usize,
        hart:   u64,
        mask:   u64
    },
    ForeignRoot
    {
        physical: u64
    },
    HeapAdmission
    {
        requested: usize,
        cause:     super::pages::Error
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::StartingRoot { expected,
                                 observed } =>
            {
                write!(f,
                       "warm root transition refused: expected SATP {expected:#x}, observed \
                        {observed:#x}")
            }
            Self::Memory(e) => e.fmt(f),
            Self::Execution(e) => e.fmt(f),
            Self::Topology(e) => e.fmt(f),
            Self::Monitor(e) => e.fmt(f),
            Self::Physical(e) => e.fmt(f),
            Self::Policy(e) => e.fmt(f),
            Self::StackStorage(e) => e.fmt(f),
            Self::StackSlots { count } =>
            {
                write!(f,
                       "{count} discovered CPUs do not fit the guarded System service-stack \
                        window or its selected stack geometry")
            }
            Self::HeapPartition(e) => e.fmt(f),
            Self::Tables(e) => e.fmt(f),
            Self::Missing =>
            {
                f.write_str("supervisor boot/root ownership has not been admitted; reset and \
                             rebuild ROM/System together")
            }
            Self::ForeignHart =>
            {
                f.write_str("opaque firmware access requires its admitted owner hart; foreign CPU \
                             refused")
            }
            Self::AlreadyActive =>
            {
                f.write_str("supervisor roots are already active; bootstrap cannot be repeated \
                             without reset")
            }
            Self::Admission { root,
                              status,
                              hart,
                              mask } =>
            {
                write!(f,
                       "supervisor admission refused satp={root:#x} sstatus={status:#x} \
                        hart={hart} tag_mask={mask:#x}; expected masked Sv39 bootstrap on hart0 \
                        with the admitted root")
            }
            Self::ForeignRoot { physical } =>
            {
                write!(f, "prepared translation root {physical:#x} has no owned PageBox")
            }
            Self::HeapAdmission { requested,
                                  cause } =>
            {
                write!(f,
                       "fixed System heap admission refused: {requested} requested bytes: \
                        {cause}; increase RAM or select a smaller platform system_heap_bytes and \
                        rebuild ROM/System together; no reduced heap was installed")
            }
        }
    }
}
fn root_register() -> usize
{
    let value;
    unsafe {
        asm!("csrr {},satp",out(reg)value,options(nostack));
    }
    value
}
/// # Safety
/// Native entry supplies pinned physical wire arrays through the already active
/// firmware root's permanent RAM map. No allocator or mutable pool exists yet.
pub(crate) unsafe fn admit(info: &'static BootInfo) -> Result<(), Error>
{
    // Entry already checked the exact BootInfo size. Authenticate the expanded
    // monitor layout before validate_boot forms any Directory/Entry view.
    if info.monitor_layout != mac_abi::boot::monitor::LAYOUT_FINGERPRINT
    {
        return Err(Error::Monitor(super::monitor::Error::Layout));
    }
    if BOOT.get().is_some()
    {
        return Err(Error::AlreadyActive);
    }
    let memory = unsafe { super::memory::validate_boot(info) }.map_err(Error::Memory)?;
    let record = unsafe { super::memory::execution(info) }.map_err(Error::Memory)?;
    // validate_boot checked the whole physical array's reservation before this
    // mapped view is formed. Discovery metadata never selects the current hart.
    let cpus = unsafe { super::memory::primary_slice(info, info.cpus, info.cpu_count) }
        .map_err(Error::Memory)?;
    let topology = topology::Topology::checked(cpus, record.hart).map_err(Error::Topology)?;
    let regions = unsafe {
                      super::memory::primary_slice(info, record.regions, record.region_count)
                  }.map_err(Error::Memory)?;
    record.checked(memory,
                   regions,
                   BootMemoryRange { base:   IMAGE_BASE,
                                     length: VM_SYSTEM_IMAGE_LIMIT - VM_SYSTEM_IMAGE_BASE },
                   BootMemoryRange { base:   VM_SYSTEM_IMAGE_BASE,
                                     length: VM_SYSTEM_IMAGE_LIMIT - VM_SYSTEM_IMAGE_BASE },
                   BootMemoryRange { base:   FLASH_BASE,
                                     length: FLASH_BYTES })
          .map_err(Error::Execution)?;
    let root = root_register();
    let status: usize;
    unsafe {
        asm!("csrr {},sstatus",out(reg)status,options(nostack));
    }
    if root >> 60 != 8
       || root & PPN != record.root_physical as usize >> 12
       || (root >> 44) & 0xffff != 0
       || status & 2 != 0
       || record.hart != 0
       || record.hardware_tag_mask > 0xffff
       || !(record.hardware_tag_mask + 1).is_power_of_two()
    {
        return Err(Error::Admission { root,
                                      status,
                                      hart: record.hart,
                                      mask: record.hardware_tag_mask });
    }
    let ram=DirectMap::new::<Sv39>(memory,AddressRange::new(VM_PHYSMAP_START,VM_PHYSMAP_END).map_err(Error::Policy)?,PhysicalAddress(VM_PHYSMAP_PHYSICAL_BASE)).map_err(Error::Physical)?;
    // ROM already installed the admitted permanent RAM physmap; DirectMap is
    // its checked software view, not a hardware mapping operation. Monitor
    // admission checks every isolated control extent against that view before
    // borrowing atomic cells; BOOT/global allocator publication happens later.
    unsafe { super::monitor::admit(info, memory, cpus, record.hart as usize, &ram) }.map_err(Error::Monitor)?;
    let boot = BOOT.call_once(|| {
                       Bootstrap { ram,
                                   memory,
                                   regions,
                                   topology,
                                   root,
                                   hart: record.hart as usize,
                                   tag_mask: record.hardware_tag_mask as u16,
                                   image_physical: record.physical_image,
                                   image_virtual: record.virtual_image }
                   });
    // BOOT_LOG and the cold execution selector are not installed yet. Admission
    // diagnostics use the independent UART path, never a firmware callback.
    super::emergency::serial(format_args!("CPU_TOPOLOGY enabled={} primary={} \
                                           admission=primary-only",
                                          boot.topology.cpus().len(),
                                          boot.hart));
    Ok(())
}
/// Fixed opaque-firmware owner from checked boot metadata. This is not a
/// current-CPU selector and must never supply runtime execution identity.
pub(crate) fn firmware_owner_hart() -> Option<usize>
{
    BOOT.get().map(|boot| boot.hart)
}
/// Immutable discovery order used for storage ordinals, never current execution
/// authority. Sparse hardware IDs do not become array or virtual-slot indices.
pub(crate) fn enabled_cpus() -> Option<&'static [topology::Cpu]>
{
    Some(BOOT.get()?.topology.cpus())
}
pub(crate) fn memory() -> Result<(CheckedMemory<'static>, DirectMap<'static>), Error>
{
    let boot = BOOT.get().ok_or(Error::Missing)?;
    Ok((boot.memory, boot.ram))
}
/// Terminal admission reporting needs no heap, firmware callback or root switch.
/// The bootstrap root declares this checked board UART window before entry.
pub(crate) fn report_refusal(error: &impl fmt::Display)
{
    use core::fmt::Write;
    struct Serial;
    impl Write for Serial
    {
        fn write_str(&mut self, s: &str) -> fmt::Result
        {
            for &byte in s.as_bytes()
            {
                unsafe {
                    while ptr::read_volatile((UART_BASE + 5) as *const u8) & 0x20 == 0
                    {
                        core::hint::spin_loop();
                    }
                    ptr::write_volatile(UART_BASE as *mut u8, byte);
                }
            }
            Ok(())
        }
    }
    let _ = writeln!(Serial, "BOOT_SUPERVISOR_REJECTED: {error}");
}
#[derive(Debug)]
pub(crate) enum FrameError
{
    Pages(super::pages::Error),
    Foreign
    {
        physical: u64
    },
    Slot
    {
        index: usize
    }
}
impl fmt::Display for FrameError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Pages(e) => e.fmt(f),
            Self::Foreign { physical } =>
            {
                write!(f, "page table {physical:#x} is not retained by this root")
            }
            Self::Slot { index } => write!(f, "page-table slot {index} exceeds512 entries")
        }
    }
}
struct Node
{
    table:    Option<RootPage>,
    physical: PhysicalAddress,
    cpu:      usize,
    next:     usize,
    _storage: Backing
}
/// Nodes and tables own separate page leases, so root preparation cannot recurse
/// into the not-yet-initialized heap. Raw node walks are confined to this binding.
/// After publication these owners are immutable and their physmap addresses agree
/// in both roots. No unsafe Send/Sync implementation or pointer-mode flag exists.
struct Frames
{
    head:  usize,
    count: usize
}
impl Frames
{
    fn node(&self, physical: PhysicalAddress) -> Result<&Node, FrameError>
    {
        let mut next = self.head;
        while next != 0
        {
            let node = unsafe { &*(next as *const Node) };
            if node.physical == physical && node.table.is_some()
            {
                return Ok(node);
            }
            next = node.next;
        }
        Err(FrameError::Foreign { physical: physical.0 })
    }

    fn take_root(&mut self, physical: PhysicalAddress) -> Result<RootPage, Error>
    {
        let mut next = self.head;
        while next != 0
        {
            let node = unsafe { &mut *(next as *mut Node) };
            if node.physical == physical
            {
                return node.table.take().ok_or(Error::ForeignRoot { physical: physical.0 });
            }
            next = node.next;
        }
        Err(Error::ForeignRoot { physical: physical.0 })
    }
}
impl Tables for Frames
{
    type Error = FrameError;

    fn allocate_zeroed(&mut self) -> Result<PhysicalAddress, FrameError>
    {
        let backing = Backing::allocate(4096).map_err(FrameError::Pages)?;
        let cpu = backing.cpu_base();
        let physical = backing.physical();
        let table = backing.into_table().map_err(FrameError::Pages)?;
        let storage = Backing::allocate(size_of::<Node>()).map_err(FrameError::Pages)?;
        let pointer = storage.cpu_base();
        // SAFETY: exclusive aligned zeroed node storage is disjoint from table RAM;
        // moving its unique leases here precedes publishing the private chain link.
        unsafe {
            (pointer as *mut Node).write(Node { table: Some(table),
                                                physical,
                                                cpu,
                                                next: self.head,
                                                _storage: storage });
        }
        self.head = pointer;
        self.count += 1;
        Ok(physical)
    }

    fn load(&self, table: PhysicalAddress, index: usize) -> Result<u64, FrameError>
    {
        if index >= 512
        {
            return Err(FrameError::Slot { index });
        }
        let node = self.node(table)?;
        Ok(unsafe { ptr::read_volatile((node.cpu as *const u64).add(index)) })
    }

    fn store(&mut self, table: PhysicalAddress, index: usize, entry: Entry)
             -> Result<(), FrameError>
    {
        if index >= 512
        {
            return Err(FrameError::Slot { index });
        }
        let node = self.node(table)?;
        unsafe {
            ptr::write_volatile((node.cpu as *mut u64).add(index), entry.bits());
        }
        Ok(())
    }
}
impl Drop for Frames
{
    fn drop(&mut self)
    {
        while self.head != 0
        {
            let node = unsafe { (self.head as *const Node).read() };
            self.head = node.next;
            drop(node);
        }
    }
}
struct Rva;
impl PageLayout for Rva
{
    fn geometry() -> PageGeometry
    {
        Sv39::geometry()
    }

    fn leaf_sizes() -> &'static [u64]
    {
        Sv39::leaf_sizes()
    }

    fn valid_virtual_range(range: AddressRange) -> bool
    {
        Sv39::valid_virtual_range(range)
    }
}
impl Architecture for Rva
{
    type InvocationGate = NativeInvocationGate;
    type PageTable = [u64; 512];
    type TrapFrame = super::traps::TrapFrame;
}
struct Space
{
    owner:    AddressSpace<'static, 'static, Rva>,
    tables:   Frames,
    register: usize,
    leaves:   u64
}
struct Spaces
{
    kernel:        Space,
    firmware:      Space,
    stacks:        OwnedArray<Foundation<Stack>>,
    heap:          HeapPartition,
    distinct_tags: bool
}
/// Validate the complete reserved window before allocating any stack backing.
/// Each ordinal owns one configured stride, including its unmapped guards.
fn check_stack_slots(count: usize) -> Result<(), Error>
{
    let invalid = || Error::StackSlots { count };
    let minimum = PAGE_BYTES.checked_mul(2)
                            .and_then(|guards| SYSTEM_STACK_BYTES.checked_add(guards))
                            .ok_or_else(invalid)?;
    let span = u64::try_from(count).ok()
                                   .and_then(|n| n.checked_mul(VM_SYSTEM_STACK_STRIDE))
                                   .ok_or_else(invalid)?;
    if count == 0
       || SYSTEM_STACK_BYTES == 0
       || !SYSTEM_STACK_BYTES.is_multiple_of(PAGE_BYTES)
       || !VM_SYSTEM_STACKS_START.is_multiple_of(PAGE_BYTES)
       || !VM_SYSTEM_STACK_STRIDE.is_multiple_of(PAGE_BYTES)
       || VM_SYSTEM_STACK_STRIDE < minimum
       || VM_SYSTEM_STACKS_START.checked_add(span).is_none_or(|end| end > VM_SYSTEM_STACKS_END)
    {
        return Err(invalid());
    }
    Ok(())
}
fn stack_slot_base(ordinal: usize) -> Result<u64, Error>
{
    u64::try_from(ordinal).ok()
                          .and_then(|n| n.checked_mul(VM_SYSTEM_STACK_STRIDE))
                          .and_then(|offset| VM_SYSTEM_STACKS_START.checked_add(offset))
                          .and_then(|base| base.checked_add(PAGE_BYTES))
                          .ok_or(Error::StackSlots { count: ordinal.saturating_add(1) })
}
/// The only stack table is owned by the same permanent roots that map it.
/// Borrowed elements remain pinned until reset, including failed later S release.
pub(crate) fn system_stacks() -> Option<&'static [Foundation<Stack>]>
{
    Some(SPACES.get()?.stacks.as_slice())
}
/// Actual permanent roots and one canonical service-stack slot. Construction
/// borrows the owning Spaces, never caller-supplied SATP values. The proof
/// neither selects a hart nor grants execution; the native row binding does.
pub(in crate::machine) struct PreparedRoots
{
    spaces: &'static Spaces,
    boot:   &'static Bootstrap,
    stack:  &'static Stack
}
impl PreparedRoots
{
    pub(in crate::machine) fn firmware_register(&self) -> usize
    {
        self.spaces.firmware.register
    }

    pub(in crate::machine) fn hardware_tag_mask(&self) -> u16
    {
        self.boot.tag_mask
    }

    pub(in crate::machine) fn service_stack(&self) -> &'static Stack
    {
        self.stack
    }

    /// # Safety
    /// Authenticated Starting entry owns this actual slot, executes masked on
    /// it in the permanent firmware root, and has installed the poisoned local
    /// System vector. Both roots map this same stack and all live Rust views.
    /// This is a permanent local transition: no guard restores firmware root.
    pub(in crate::machine) unsafe fn activate_kernel(&self) -> Result<(), Error>
    {
        if root_register() != self.spaces.firmware.register
        {
            return Err(Error::StartingRoot { expected: self.spaces.firmware.register,
                                             observed: root_register() });
        }
        unsafe {
            asm!("csrw satp,{}", "sfence.vma zero,zero",
                 in(reg)self.spaces.kernel.register, options(nostack));
        }
        if root_register() != self.spaces.kernel.register
        {
            return Err(Error::StartingRoot { expected: self.spaces.kernel.register,
                                             observed: root_register() });
        }
        Ok(())
    }
}
pub(in crate::machine) fn prepared_roots(ordinal: usize) -> Option<PreparedRoots>
{
    let spaces = SPACES.get()?;
    let stack: &'static Stack = spaces.stacks.as_slice().get(ordinal)?;
    Some(PreparedRoots { spaces,
                         boot: BOOT.get()?,
                         stack })
}
fn system_stack_containing(base: u64) -> Option<&'static Stack>
{
    let offset = base.checked_sub(VM_SYSTEM_STACKS_START.checked_add(PAGE_BYTES)?)?;
    let ordinal = usize::try_from(offset.checked_div(VM_SYSTEM_STACK_STRIDE)?).ok()?;
    let stack: &Stack = system_stacks()?.get(ordinal)?;
    let top = stack.top() as u64;
    (base >= top.checked_sub(stack.bytes())? && base < top).then_some(stack)
}
fn permissions(flags: u32, global: bool) -> Permissions
{
    Permissions { read: flags & READ != 0,
                  write: flags & WRITE != 0,
                  execute: flags & EXECUTE != 0,
                  access: Access::Supervisor,
                  global }
}
fn map(mapper: &mut Mapper<Frames>, virtual_base: u64, physical: u64, bytes: u64, flags: u32,
       global: bool)
       -> Result<u64, Error>
{
    mapper.map_range(virtual_base,
                     PhysicalAddress(physical),
                     bytes,
                     permissions(flags, global),
                     PageOwnership::Manual)
          .map(|(_, changed)| changed)
          .map_err(Error::Tables)
}
fn prepare_space(boot: &Bootstrap, firmware: bool, stacks: &[Foundation<Stack>],
                 heap: PhysicalAddress, tag: usize)
                 -> Result<Space, Error>
{
    let mut mapper = Mapper::new(Frames { head:  0,
                                          count: 0 }).map_err(Error::Tables)?;
    let mut leaves = 0;
    for bank in boot.ram.banks()
    {
        leaves += map(&mut mapper,
                      bank.virtual_address.0,
                      bank.physical.0,
                      bank.bytes,
                      READ | WRITE,
                      true)?;
    }
    for region in boot.regions
    {
        if firmware || region.flags & FIRMWARE_ONLY == 0
        {
            leaves += map(&mut mapper,
                          region.virtual_base,
                          region.physical.base,
                          region.physical.length,
                          region.flags,
                          region.flags & FIRMWARE_ONLY == 0)?;
        }
    }
    for (ordinal, stack) in stacks.iter().enumerate()
    {
        leaves += map(&mut mapper,
                      stack_slot_base(ordinal)?,
                      stack.physical().0,
                      stack.bytes(),
                      READ | WRITE,
                      true)?;
    }
    leaves += map(&mut mapper,
                  VM_SYSTEM_HEAP_BASE,
                  heap.0,
                  VM_SYSTEM_HEAP_LIMIT - VM_SYSTEM_HEAP_BASE,
                  READ | WRITE,
                  true)?;
    // Trusted flat application/module arenas remain an explicit temporary gate.
    leaves += map(&mut mapper,
                  APPLICATION_PRIMARY_BASE,
                  APPLICATION_PRIMARY_BASE,
                  IMAGE_LIMIT - APPLICATION_PRIMARY_BASE,
                  READ | WRITE | EXECUTE,
                  false)?;
    leaves += map(&mut mapper,
                  MODULE_BASE,
                  MODULE_BASE,
                  MODULE_LIMIT - MODULE_BASE,
                  READ | WRITE | EXECUTE,
                  false)?;
    for &(base, end) in BOOT_MMIO_REGIONS
    {
        leaves += map(&mut mapper, base, base, end - base, READ | WRITE, true)?;
    }
    for &(_, _, base, bytes, _, _) in super::power::platform::PLATFORM_MMIO_DEVICES
    {
        leaves += map(&mut mapper, base, base, bytes, READ | WRITE, true)?;
    }
    let (physical, mut tables) = mapper.into_tables();
    let root = tables.take_root(physical)?;
    let owner=AddressSpace::<Rva>::reserve(DomainId::SYSTEM,root,AsidTag::Permanent{hardware_value:tag as u64}).map_err(Error::Policy)?;
    let register = MODE | (tag << 44) | (owner.root_physical().0 as usize >> 12);
    Ok(Space { owner,
               tables,
               register,
               leaves })
}
/// # Safety
/// Firmware's physmap is active before pool construction. Both prepared roots
/// own initialized PageBox tables and agree on every surviving Rust address.
pub(crate) unsafe fn initialize<R>(call: impl FnOnce(&'static Stack) -> R) -> Result<R, Error>
{
    if SPACES.get().is_some()
    {
        return Err(Error::AlreadyActive);
    }
    let boot = BOOT.get().ok_or(Error::Missing)?;
    let cpus = boot.topology.cpus();
    check_stack_slots(cpus.len())?;
    let primary = cpus.binary_search_by_key(&(boot.hart as u64), |cpu| cpu.hart)
                      .map_err(|_| Error::Topology(topology::Error::MissingPrimary {
                          hart: boot.hart as u64
                      }))?;
    // The fixed heap is admitted as one unique buddy extent before any heap
    // references or permanent roots exist. Its physmap alias remains stable;
    // only the configured usable bytes become allocator storage. A rounded
    // tail stays owned by Spaces and is never offered as extra heap capacity.
    let heap = Backing::reserve_uninitialized(SYSTEM_HEAP_BYTES).map_err(|cause| {
                   Error::HeapAdmission { requested: SYSTEM_HEAP_BYTES,
                                          cause }
               })?;
    let heap = HeapPartition::new(heap).map_err(Error::HeapPartition)?;
    let stack_bytes =
        usize::try_from(SYSTEM_STACK_BYTES).map_err(|_| Error::StackSlots { count: cpus.len() })?;
    let mut stacks = OwnedArray::try_new(cpus.len(), |_| {
                         Foundation::<Stack>::allocate(stack_bytes)
                     }).map_err(Error::StackStorage)?;
    let distinct_tags = boot.tag_mask & 1 != 0;
    let kernel =
        prepare_space(boot, false, stacks.as_slice(), heap.physical(), usize::from(distinct_tags))?;
    let firmware = prepare_space(boot, true, stacks.as_slice(), heap.physical(), 0)?;
    for (ordinal, stack) in stacks.as_mut_slice().iter_mut().enumerate()
    {
        let base =
            usize::try_from(stack_slot_base(ordinal)?).map_err(|_| {
                                                          Error::StackSlots { count: cpus.len() }
                                                      })?;
        // SAFETY: both fresh owned roots now map this exact unique physical
        // lease at the checked guarded slot. No stack has been published or
        // used for execution, CPU-frame pointers, or storage references. Only
        // the future numeric executor address changes, never physmap backing.
        // Any prepublication refusal drops both roots and the entire array.
        unsafe { stack.bind_mapping(base) }.map_err(|cause| {
            Error::StackStorage(ArrayError::Element { index: ordinal,
                                                      cause })
        })?;
    }
    let spaces = SPACES.call_once(|| {
                           Spaces { kernel,
                                    firmware,
                                    stacks,
                                    heap,
                                    distinct_tags }
                       });
    // Every post-activation refusal must return under the permanently owned
    // firmware root. Keep restoration armed across entry preparation and the
    // complete headless service call; no null return can use an unmapped ROM RA.
    let _restore = RootScope { previous: spaces.firmware.register,
                               changed:  true,
                               fence:    !spaces.distinct_tags,
                               _hart:    PhantomData };
    // Publish real owners before installing their actual root. One full fence
    // retires the boot-owned ASID0/global translations; no old tree is freed here.
    unsafe {
        asm!("csrw satp,{root}","sfence.vma zero,zero",root=in(reg)spaces.kernel.register,options(nostack));
    }
    serial!("MMU_READY mode=sv39 actual={:#x} kernel_root={:#x} firmware_root={:#x} tables={} \
             leaves={} stack={:#x}+{} guards=2 service_stacks={} stack_table={:#x}+{}",
            root_register(),
            spaces.kernel.owner.root_physical().0,
            spaces.firmware.owner.root_physical().0,
            spaces.kernel.tables.count + spaces.firmware.tables.count,
            spaces.kernel.leaves,
            stack_slot_base(primary)?,
            SYSTEM_STACK_BYTES,
            spaces.stacks.as_slice().len(),
            spaces.stacks.physical().0,
            spaces.stacks.bytes());
    serial!("SYSTEM_HEAP_READY base={:#x} bytes={} physical={:#x} backing_bytes={} \
             permissions=supervisor-rw-nx roots=2 protected_reserve=true ordinary_bytes={} \
             reserve_bytes={}",
            VM_SYSTEM_HEAP_BASE,
            SYSTEM_HEAP_BYTES,
            spaces.heap.physical().0,
            spaces.heap.bytes(),
            spaces.heap.ordinary_range().1,
            SYSTEM_RECOVERY_BYTES);
    Ok(call(&spaces.stacks.as_slice()[primary]))
}
/// The allocation range is valid only once both permanent roots own the exact
/// mapping and its physical lease. Numeric facts confer no independent owner.
pub(crate) fn heap_range() -> Option<(usize, usize)>
{
    Some(SPACES.get()?.heap.ordinary_range())
}
/// Full mapped bytes, including protected storage. This does not grant allocator access.
pub(crate) fn mapped_heap_range() -> Option<(usize, usize)>
{
    SPACES.get()?;
    Some((VM_SYSTEM_HEAP_BASE as usize, SYSTEM_HEAP_BYTES))
}
pub(crate) fn protected_heap_statistics() -> Option<ProtectedHeapStatistics>
{
    Some(SPACES.get()?.heap.statistics())
}
pub(crate) fn reserve_terminal_storage(frame_bytes: usize, stack_bytes: usize)
                                       -> Result<(ProtectedRange, ProtectedRange), Error>
{
    SPACES.get()
          .ok_or(Error::Missing)?
          .heap
          .terminal_storage(frame_bytes, stack_bytes)
          .map_err(Error::HeapPartition)
}
pub(crate) fn reserve_recovery_storage() -> Result<ProtectedRange, Error>
{
    SPACES.get().ok_or(Error::Missing)?.heap.recovery_storage().map_err(Error::HeapPartition)
}

struct RootScope
{
    previous: usize,
    changed:  bool,
    fence:    bool,
    _hart:    PhantomData<*mut ()>
}
impl RootScope
{
    fn enter(register: usize, fence: bool) -> Self
    {
        Self::from_previous(register, fence, root_register())
    }

    fn from_previous(register: usize, fence: bool, previous: usize) -> Self
    {
        let changed = previous != register;
        if changed
        {
            unsafe {
                asm!("csrw satp,{}",in(reg)register,options(nostack));
                if fence
                {
                    asm!("sfence.vma zero,zero", options(nostack));
                }
            }
        }
        Self { previous,
               changed,
               fence,
               _hart: PhantomData }
    }
}
impl Drop for RootScope
{
    fn drop(&mut self)
    {
        if self.changed
        {
            unsafe {
                asm!("csrw satp,{}",in(reg)self.previous,options(nostack));
                if self.fence
                {
                    asm!("sfence.vma zero,zero", options(nostack));
                }
            }
        }
    }
}
pub(crate) fn with_firmware<R>(call: impl FnOnce() -> R) -> Result<R, Error>
{
    interrupts::with_masked(|| {
        let boot = BOOT.get().ok_or(Error::Missing)?;
        if super::traps::execution::hart_id() != Some(boot.hart)
        {
            return Err(Error::ForeignHart);
        }
        let (register, fence) = SPACES.get().map_or((boot.root, true), |spaces| {
                                                (spaces.firmware.register, !spaces.distinct_tags)
                                            });
        let _scope = RootScope::enter(register, fence);
        Ok(call())
    })
}
/// # Safety
/// Called only by the saved native trap-entry boundary, with SIE already clear
/// and common System stack/CPU backing active in either admitted root. No
/// ordinary callback may enter here without that architecture admission.
pub(crate) unsafe fn with_kernel<R>(call: impl FnOnce() -> R) -> Result<R, Error>
{
    let spaces = SPACES.get().ok_or(Error::Missing)?;
    let current = root_register();
    // Ordinary application/interrupt entry already uses this root. Do not
    // rewrite sstatus/satp or construct a restoration for a no-change call.
    if current == spaces.kernel.register
    {
        return Ok(call());
    }
    let _scope = RootScope::from_previous(spaces.kernel.register, !spaces.distinct_tags, current);
    Ok(call())
}

pub(crate) fn bus_address(cpu: u64, bytes: u64) -> Option<u64>
{
    let boot = BOOT.get()?;
    let end = cpu.checked_add(bytes)?;
    if bytes == 0
    {
        return None;
    }
    if let Some(address) = alias::physical(cpu, bytes, boot.image_virtual.base,
                                           boot.image_physical.base, boot.image_virtual.length)
    {
        return Some(address);
    }
    if cpu >= VM_SYSTEM_HEAP_BASE && end <= VM_SYSTEM_HEAP_LIMIT
    {
        return alias::physical(cpu,
                               bytes,
                               VM_SYSTEM_HEAP_BASE,
                               SPACES.get()?.heap.physical().0,
                               VM_SYSTEM_HEAP_LIMIT - VM_SYSTEM_HEAP_BASE);
    }
    if let Ok(physical) = boot.ram.physical_address(VirtualAddress(cpu), bytes)
    {
        return Some(physical.0);
    }
    if (cpu >= MODULE_BASE && end <= MODULE_LIMIT)
       || (cpu >= APPLICATION_PRIMARY_BASE && end <= IMAGE_LIMIT)
    {
        boot.ram.virtual_address(PhysicalAddress(cpu), bytes).ok()?;
        return Some(cpu);
    }
    None
}

/// Only resident aliases that both permanent roots map identically may cross a
/// callback directly. Application/module buffers use bounded resident copies.
pub(crate) fn firmware_visible(cpu: u64, bytes: u64) -> bool
{
    let Some(boot) = BOOT.get()
    else
    {
        return false;
    };
    let Some(end) = cpu.checked_add(bytes)
    else
    {
        return false;
    };
    (cpu >= boot.image_virtual.base && end <= boot.image_virtual.base + boot.image_virtual.length)
    || boot.ram.physical_address(VirtualAddress(cpu), bytes).is_ok()
    || system_stack_containing(cpu).is_some_and(|stack| end <= stack.top() as u64)
    || (SPACES.get().is_some()
        && bytes != 0
        && cpu >= VM_SYSTEM_HEAP_BASE
        && end <= VM_SYSTEM_HEAP_LIMIT)
}
