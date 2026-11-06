//! AArch64 EL1 admission entry: initialise the PL011 once, install the checked
//! 39-bit translation regime, report a named marker, and halt. The vector table
//! reports the fault registers instead of spinning silently.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::panic::PanicInfo;

use linked_list_allocator::LockedHeap;

mod memory;
mod monitor;

#[global_allocator]
static HEAP: LockedHeap = LockedHeap::empty();

/// The record declares where the loader may work, and the loader uses exactly
/// that region. It is not an in-image array: System's code resource is several
/// megabytes, so a fixed buffer inside the boot window cannot hold it, and a
/// heap the memory map does not know about is a region the kernel could hand to
/// something else.
fn init_heap()
{
    // SAFETY: the bootrom is single-threaded on the primary CPU and this runs
    // once before any allocation; the region is the record's declared firmware
    // heap, reserved in the same memory account the kernel receives, and no
    // reference to it escapes this function.
    unsafe { HEAP.lock().init(FW_HEAP_BASE as *mut u8, FW_HEAP_BYTES as usize) };
}

#[alloc_error_handler]
fn allocation_failed(_: core::alloc::Layout) -> !
{
    loop
    {}
}

core::arch::global_asm!(include_str!(concat!(env!("OUT_DIR"), "/platform-hardware.inc")),
                        include_str!("entry.S"));

include!(concat!(env!("OUT_DIR"), "/platform-memory.rs"));

/// The machine's device tree, as a checked slice.
///
/// The pointer comes from the machine, so nothing about it is trusted: it must
/// be non-zero, eight-byte aligned as the specification requires, carry the
/// header magic, and state a total size that fits inside the reservation the
/// record declares. A machine that supplies nothing is a named refusal, never a
/// reason to build a tree of our own: an invented tree is a claim nothing can
/// check, and a wrong one sends the probe at hardware that is not there.
fn firmware_tree(address: usize) -> Option<&'static [u8]>
{
    if address == 0 || address % 8 != 0
    {
        return None;
    }
    let header = address as *const u8;
    // SAFETY: the machine handed us this address and the header is the first
    // thing a blob must contain; the reads are bounded by the two words of the
    // header, and the full slice is only taken once `total` is known to fit the
    // reservation.
    let magic = u32::from_be(unsafe { core::ptr::read_volatile(header as *const u32) });
    if magic != 0xd00d_feed
    {
        return None;
    }
    let total = u32::from_be(unsafe {
                      core::ptr::read_volatile(header.add(4) as *const u32)
                  }) as usize;
    if total < 40 || total as u64 > FDT_TAIL_BYTES
    {
        return None;
    }
    puts("BOOT_AARCH64_DT_MAGIC: magic=");
    puthex(magic as u64);
    puts(" bytes=");
    puthex(total as u64);
    puts("\n");
    // SAFETY: `total` was read from the header and bounded by the declared
    // reservation, and the machine guarantees the blob is resident for the
    // bootloader's lifetime.
    Some(unsafe { core::slice::from_raw_parts(header, total) })
}

// Translation tables live inside the declared boot window, clear of the loaded
// image and its stack. A 39-bit regime with a 4 KiB granule starts its walk at
// level 1, so TTBR0/TTBR1 name the level-1 tables directly. Every descriptor we
// touch sets AF; a table walker that reaches a descriptor without AF faults on
// first access even when the rest of the entry looks right.
const BLOCK: u64 = 0x20_0000;
const TABLE: u64 = 0x3;
// Valid block, AttrIndx 0 (normal write-back), inner shareable, AF.
const NORMAL: u64 = 0x701;
// A level-3 leaf is a page, so bit 1 is set where a block leaves it clear.
const PAGE_NORMAL: u64 = 0x703;
// Valid block, AttrIndx 1 (Device-nGnRnE), AF, never executable.
const DEVICE: u64 = 0x405 | (1 << 54) | (1 << 53);
// 39-bit VA at both halves, 4 KiB granule at both halves, 32-bit physical.
// T0SZ/T1SZ 25 (39-bit halves), IRGN0/ORGN0 write-back, SH0 inner shareable,
// TG0 0b00 and TG1 0b10 for a 4 KiB granule, IPS 0b010 for 40-bit physical
// addresses. Cacheable walks are what the resident kernel expects.
const TCR: u64 = 0x2_8019_3519;
// MAIR[0] normal write-back, MAIR[1] Device-nGnRnE.
const MAIR: u64 = 0xFF;
const KERNEL_HALF: u64 = 0xffff_ff80_0000_0000;

unsafe fn clear(table: u64)
{
    for index in 0..512
    {
        unsafe { core::ptr::write_volatile((table as *mut u64).add(index), 0) };
    }
}

unsafe fn entry(table: u64, index: usize, value: u64)
{
    unsafe { core::ptr::write_volatile((table as *mut u64).add(index), value) };
}

/// Builds the 39-bit translation tables and turns the MMU on. Called with the
/// MMU off; every address below is physical. Returns with the identity map and
/// the declared physmap active.
#[unsafe(no_mangle)]
pub extern "C" fn mmu_init()
{
    let ram_blocks = (PHYSICAL_RAM_BYTES / BLOCK) as usize;
    let physmap_index = ((PHYSMAP_BASE - KERNEL_HALF) / (1 << 30)) as usize;
    // The device aperture begins where RAM ends, so the mapping needs the RAM
    // extent rather than any individual peripheral address.
    assert!(PHYSICAL_RAM_BYTES % BLOCK == 0, "RAM must end on a block boundary");
    assert!(physmap_index < 512, "the physmap must sit inside the kernel half");
    unsafe
    {
        for table in [L1_LOW, L2_LOW, L1_HIGH, L2_PHYSMAP, L2_LOWDEV, L2_IMAGE]
        {
            clear(table);
        }
        // The identity map belongs at the level-1 index that covers the RAM
        // base, not at zero. A machine whose RAM does not start at zero has no
        // usable low addresses, so placing it at zero would leave the code's own
        // address unmapped the moment the MMU came on.
        let ram_index = (PHYSICAL_RAM_BASE >> 30) as usize;
        // The low gigabyte carries the machine's peripherals: the console and the
        // virtio bank live there, and neither is RAM.
        entry(L1_LOW, 0, L2_LOWDEV | TABLE);
        for index in 0..512
        {
            entry(L2_LOWDEV, index, index as u64 * BLOCK | DEVICE);
        }
        entry(L1_LOW, ram_index, L2_LOW | TABLE);
        for index in 0..ram_blocks
        {
            entry(L2_LOW, index, PHYSICAL_RAM_BASE + index as u64 * BLOCK | NORMAL);
        }
        for index in ram_blocks..512
        {
            entry(L2_LOW, index, PHYSICAL_RAM_BASE + index as u64 * BLOCK | DEVICE);
        }
        entry(L1_HIGH, physmap_index, L2_PHYSMAP | TABLE);
        for index in 0..ram_blocks
        {
            entry(L2_PHYSMAP, index, PHYSICAL_RAM_BASE + index as u64 * BLOCK | NORMAL);
        }
        // The kernel's image window. Its base is not on a gigabyte boundary - it
        // sits 2 MiB into a gigabyte - so the level-2 index has to carry that
        // offset. Deriving it from the virtual address rather than assuming zero
        // is the difference between mapping the image and mapping the gigabyte
        // below it.
        let image_l1 = ((VM_SYSTEM_IMAGE_BASE - KERNEL_HALF) / (1 << 30)) as usize;
        let image_first = ((VM_SYSTEM_IMAGE_BASE & 0x3fff_ffff) / BLOCK) as usize;
        // 4 KiB pages, not 2 MiB blocks. The window's virtual and physical bases
        // are not congruent modulo a block, so no block descriptor can express
        // this translation; a page descriptor can, because the offset between
        // the two windows is constant.
        //
        // This extent is the mapped window, not the record's reserved image
        // window: those are different quantities and the record's
        // `memory.images.bytes` is 256 MiB, which would need 128 page tables
        // and only 112 fit between the table base and the loaded image at
        // 0x40080000. Deriving this from the loaded image length is the correct
        // fix and it arrives with the image decode, which the loader does not
        // do yet; deriving it from the reserved window would overwrite the
        // loader's own code.
        let image_tables = 16usize; // 32 MiB of window, covering the System image
        assert!(image_l1 < 512, "the image window must sit inside the kernel half");
        assert!(image_first + image_tables <= 512,
                "the image window must fit one level-2 table");
        assert!(L3_BASE + image_tables as u64 * 0x1000 <= IMAGE_BASE,
                "the image window's page tables must stay clear of the loaded image");
        entry(L1_HIGH, image_l1, L2_IMAGE | TABLE);
        for k in 0..image_tables
        {
            let table = L3_BASE + (k as u64) * 0x1000;
            clear(table);
            entry(L2_IMAGE, image_first + k, table | TABLE);
            for j in 0..512u64
            {
                let offset = (k as u64) * BLOCK + j * 0x1000;
                entry(table, j as usize, IMAGE_BASE + offset | PAGE_NORMAL);
            }
        }
        core::arch::asm!(
            "dsb sy",
            "msr mair_el1, {mair}",
            "msr tcr_el1, {tcr}",
            "msr ttbr0_el1, {ttbr0}",
            "msr ttbr1_el1, {ttbr1}",
            "isb",
            mair = in(reg) MAIR,
            tcr = in(reg) TCR,
            ttbr0 = in(reg) L1_LOW,
            ttbr1 = in(reg) L1_HIGH,
            options(nostack)
        );
        core::arch::asm!(
            "mrs {sctlr}, sctlr_el1",
            "bic {sctlr}, {sctlr}, #0x80000",
            "orr {sctlr}, {sctlr}, #1",
            "orr {sctlr}, {sctlr}, #4",
            "orr {sctlr}, {sctlr}, #4096",
            "msr sctlr_el1, {sctlr}",
            "isb",
            sctlr = out(reg) _,
            options(nostack)
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn physmap_check() -> u64
{
    // The probe reads a word inside this machine's own RAM. A fixed low address
    // is the board's layout, not this machine's: RAM begins at the record's
    // ram_base, so the probe has to start from there.
    let at = PHYSICAL_RAM_BASE + 0x1000;
    let identity = unsafe { core::ptr::read_volatile(at as *const u64) };
    let aliased = unsafe { core::ptr::read_volatile((PHYSMAP_BASE + (at - PHYSICAL_RAM_BASE)) as *const u64) };
    u64::from(identity == aliased)
}

/// Confirms the kernel image window resolves to the physical image window.
///
/// A mapping is only correct if a value written through the physical address is
/// visible through the virtual one, so this writes a sentinel and reads it back
/// rather than trusting the tables it just built.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_map_check() -> u64
{
    let offset = 0x10_0000u64;
    let physical = IMAGE_BASE + offset;
    let virtual_address = VM_SYSTEM_IMAGE_BASE + offset;
    const SENTINEL: u64 = 0x4b4d_4150_5f4f_4b21;
    unsafe {
        core::ptr::write_volatile(physical as *mut u64, SENTINEL);
        core::arch::asm!("dsb sy", options(nostack));
        let seen = core::ptr::read_volatile(virtual_address as *const u64);
        u64::from(seen == SENTINEL)
    }
}

/// The capabilities the shared virtio transport asks for, supplied by this
/// machine. The transport never guesses them.
struct BootMachine;

impl mac_bootloader::virtio_blk::Machine for BootMachine
{
    fn fence()
    {
        // SAFETY: a full barrier has no memory operands; it is the device/CPU
        // publication ordering virtio requires.
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };
    }

    fn counter() -> u64
    {
        let value: u64;
        // SAFETY: reading the virtual counter has no side effects.
        unsafe { core::arch::asm!("mrs {value}, cntvct_el0", value = out(reg) value, options(nomem, nostack)) };
        value
    }
}

fn putc(byte: u8)
{
    while unsafe { core::ptr::read_volatile((PL011_BASE + 0x18) as *const u32) } & 0x20 != 0
    {}
    unsafe { core::ptr::write_volatile(PL011_BASE as *mut u32, u32::from(byte)) };
}

fn puts(text: &str)
{
    for byte in text.bytes()
    {
        putc(byte);
    }
}

fn puthex(value: u64)
{
    for shift in (0..64).step_by(4).rev()
    {
        let nibble = ((value >> shift) & 0xf) as u8;
        putc(if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 });
    }
}

/// Reads sector zero from the SD card and reports the partition-table signature
/// it actually found. A card that answers but returns the wrong sector still
/// reaches here, so the signature is the evidence rather than the call returning.
#[unsafe(no_mangle)]
pub extern "C" fn virt_probe(tree_address: usize) -> u64
{
    use mac_block::BlockDevice;
    init_heap();
    let Some(bytes) = firmware_tree(tree_address)
    else
    {
        puts("BOOT_AARCH64_DT_REFUSED: the machine supplied no usable device tree\n");
        return 0;
    };
    // Peripheral addresses come from the machine's tree, not from constants, so
    // the record is a declaration to be checked rather than the runtime source.
    // Early output had to use the record's console constant before any tree
    // could be read, so the first thing the tree is asked is whether it agrees
    // with the console actually in use. A divergence is reported by name rather
    // than discovered later as garbled output.
    let tree = match mac_devicetree::Tree::new(bytes)
    {
        Ok(tree) => tree,
        Err(cause) =>
        {
            puts("BOOT_AARCH64_DT_REFUSED: ");
            for byte in alloc::format!("{cause}").bytes()
            {
                putc(byte);
            }
            puts("\n");
            return 0;
        }
    };
    let (pl011, _) = match tree.window(PL011_COMPATIBLE)
    {
        Ok(Some(window)) => window,
        _ =>
        {
            puts("BOOT_AARCH64_DT_REFUSED: no console window in the tree\n");
            return 0;
        }
    };
    puts("BOOT_AARCH64_DT_PL011: base=");
    puthex(pl011);
    puts("\n");
    if pl011 != PL011_BASE
    {
        puts("BOOT_AARCH64_CONSOLE_MISMATCH: the tree's pl011 is not the console in use\n");
        return 0;
    }
    // Every peripheral address now comes from the tree. The loader holds no
    // address of its own, so the record is the only place this machine's
    // hardware is written down.
    let Ok(Some((virtio, _))) = tree.window("virtio,mmio")
    else
    {
        puts("BOOT_AARCH64_DT_REFUSED: no virtio-mmio window in the tree\n");
        return 0;
    };
    puts("BOOT_AARCH64_DT_VIRTIO: base=");
    puthex(virtio);
    puts("\n");
    // AArch64 states its counter frequency in a system register rather than in
    // the tree, which is why the machine's tree has no timebase-frequency and
    // the 24 MHz clock it does declare belongs to the peripheral bus. Reading
    // the register is the authoritative answer and needs no record field; a
    // machine that leaves it zero is refused rather than given a guessed rate.
    let counter: u64;
    // SAFETY: reading an identification register has no side effects.
    unsafe { core::arch::asm!("mrs {}, cntfrq_el0", out(reg) counter, options(nomem, nostack)) };
    puts("BOOT_AARCH64_COUNTER: hz=");
    puthex(counter);
    puts("\n");
    // CPU topology and RAM come from the machine's tree, not from the record.
    // These are the two facts BootInfo carries that the loader must not invent:
    // a machine that describes four harts has four, and a machine that reports
    // its RAM at a different base has it there.
    let mut harts = 0u64;
    let mut primary_hart = 0u64;
    let counted = tree.cpus(|hart| {
                        if harts == 0
                        {
                            primary_hart = hart;
                        }
                        harts += 1;
                    });
    if counted.is_err()
    {
        puts("BOOT_AARCH64_DT_REFUSED: the machine's tree is malformed while reading CPUs\n");
        return 0;
    }
    puts("BOOT_AARCH64_CPUS: count=");
    puthex(harts);
    puts(" first=");
    puthex(primary_hart);
    puts("\n");
    if harts == 0
    {
        puts("BOOT_AARCH64_CPUS_REFUSED: the machine's tree describes no CPU\n");
        return 0;
    }
    let mut ram_base = 0u64;
    let mut ram_bytes = 0u64;
    let mut banks = 0u64;
    if tree.memory(|base, bytes| {
               if banks == 0
               {
                   ram_base = base;
                   ram_bytes = bytes;
               }
               banks += 1;
           })
           .is_err()
    {
        puts("BOOT_AARCH64_DT_REFUSED: the machine's tree is malformed while reading RAM\n");
        return 0;
    }
    puts("BOOT_AARCH64_RAM: banks=");
    puthex(banks);
    puts(" base=");
    puthex(ram_base);
    puts(" bytes=");
    puthex(ram_bytes);
    puts("\n");
    if banks == 0 || ram_bytes < PHYSICAL_RAM_BYTES
    {
        puts("BOOT_AARCH64_RAM_REFUSED: the machine's RAM is absent or below the platform \
              minimum\n");
        return 0;
    }
    // The account the kernel will validate: the machine's banks, and every
    // extent this boot occupies. The tree itself is reserved where the machine
    // put it, which on this machine is part way into RAM rather than in a tail.
    let total = u32::from_be_bytes(bytes.get(4..8).unwrap_or(&[0; 4]).try_into().unwrap()) as u64;
    let account = match memory::Memory::discover(&tree, tree_address as u64, total)
    {
        Ok(account) => account,
        Err(cause) =>
        {
            puts("BOOT_AARCH64_MEMORY_REFUSED: ");
            puts(match cause
            {
                memory::Error::NoRam => "the machine describes no RAM",
                memory::Error::Unaligned => "a RAM bank is not page aligned",
                memory::Error::Overlap => "two RAM banks overlap",
                memory::Error::Reservation => "a reservation is empty, unaligned or outside RAM",
                memory::Error::Duplicate => "two reservations overlap",
                memory::Error::TreeOutsideRam => "the tree lies outside the RAM it describes",
                memory::Error::Allocation => "allocation failed",
                memory::Error::TreeUnreadable => "the machine's tree could not be walked"
            });
            puts("\n");
            return 0;
        }
    };
    match account.checked(memory::PAGE_BYTES)
    {
        Ok(_) => {}
        Err(_) =>
        {
            puts("BOOT_AARCH64_MEMORY_REFUSED: the account does not satisfy the kernel's checks\n");
            return 0;
        }
    }
    puts("BOOT_AARCH64_MEMORY_ACCOUNT: banks=");
    puthex(account.ram.len() as u64);
    puts(" reservations=");
    puthex(account.reserved.len() as u64);
    puts("\n");
    // The interrupt controller the monitor signals through, from the machine's
    // tree. A machine that declares none cannot be admitted: the kernel needs a
    // route it can publish and check, and inventing one would be a claim about
    // hardware this platform may not have.
    let mut controller = (0u64, 0u64);
    let _ = tree.for_each_node(|node| {
          if controller.1 == 0
             && node.compatible.is_some_and(|name| {
                 name == "arm,cortex-a15-gic" || name == "arm,gic-400"
                 || name == "arm,gic-v3"
             })
             && let Some((base, bytes)) = node.window()
          {
              controller = (base, bytes);
          }
      });
    puts("BOOT_AARCH64_CONTROLLER: base=");
    puthex(controller.0);
    puts(" bytes=");
    puthex(controller.1);
    puts("\n");
    // Sorted enabled harts, as the kernel's topology check requires.
    let mut cpus: alloc::vec::Vec<mac_abi::boot::topology::Cpu> = alloc::vec::Vec::new();
    let _ = tree.cpus(|hart| cpus.push(mac_abi::boot::topology::Cpu { hart }));
    cpus.sort_unstable_by_key(|cpu| cpu.hart);
    if mac_abi::boot::topology::Topology::checked(&cpus, primary_hart).is_err()
    {
        puts("BOOT_AARCH64_CPUS_REFUSED: the machine's topology is not admissible\n");
        return 0;
    }
    let monitor = match monitor::prepare(&account, &cpus,
                                         mac_abi::BootMemoryRange { base:   controller.0,
                                                                    length: controller.1 },
                                         counter)
    {
        Ok(monitor) => monitor,
        Err(cause) =>
        {
            puts("BOOT_AARCH64_MONITOR_REFUSED: ");
            puts(match cause
            {
                monitor::Error::NoController => "the machine declares no interrupt controller",
                monitor::Error::Allocation => "monitor storage could not be allocated",
                monitor::Error::Directory => "the directory fails the kernel's admission checks"
            });
            puts("\n");
            return 0;
        }
    };
    puts("BOOT_AARCH64_MONITOR_READY: harts=");
    puthex(monitor.directory.count as u64);
    puts(" stack=");
    puthex(monitor.directory.stack_bytes);
    puts(" timebase=");
    puthex(monitor.directory.timebase_hz);
    puts("\n");
    if counter == 0
    {
        puts("BOOT_AARCH64_COUNTER_REFUSED: the machine reports no counter frequency\n");
        return 0;
    }
    // Every peripheral address comes from the machine's tree. The loader holds
    // no address of its own, so the record is a declaration to be checked
    // rather than the runtime source of truth.
    let mut disks: alloc::vec::Vec<mac_bootloader::virtio_blk::Disk<BootMachine>> =
        alloc::vec::Vec::new();
    // The machine says which transport slots exist, so this walks the nodes it
    // declares rather than assuming a bank of a fixed size. A machine that
    // models one node per slot and one that models a single bank are both
    // answered by the same walk.
    let _ = tree.for_each_node(|node| {
          if node.compatible != Some("virtio,mmio") || !node.enabled()
          {
              return;
          }
          let Some((base, _bytes)) = node.window()
          else
          {
              return;
          };
          if !mac_bootloader::virtio_blk::is_block(base as usize)
          {
              return;
          }
          match mac_bootloader::virtio_blk::Disk::<BootMachine>::open(base as usize)
          {
              Ok(disk) =>
              {
                  puts("BOOT_AARCH64_VIRTIO_BLK: base=");
                  puthex(base);
                  puts(" sectors=");
                  puthex(disk.sector_count());
                  puts("\n");
                  disks.push(disk);
              }
              Err(error) =>
              {
                  puts("BOOT_AARCH64_VIRTIO_BLK_REFUSED: base=");
                  puthex(base);
                  puts(" cause=");
                  for byte in alloc::format!("{error}").bytes()
                  {
                      putc(byte);
                  }
                  puts("\n");
              }
          }
      });
    if disks.is_empty()
    {
        puts("BOOT_AARCH64_NO_DISK: the virtio-MMIO bank presents no block device\n");
        return 0;
    }
    for disk in disks.iter_mut()
    {
        find_system(disk);
    }
    1
}

/// Reads System's code and ABI resources and builds the load plan the kernel
/// will be admitted with.
///
/// The plan is the shared one the RISC-V loader uses: the code resource is
/// decoded with the native image format rather than an ELF reader, and the
/// relocation records come from the same crate. Nothing here is aarch64-specific
/// beyond the container it finds, so a defect in the plan is a defect on both
/// platforms.
fn load_system<D: mac_filesystem::BlockDevice>(volume: &mut mac_filesystem::Volume<D>,
                                               file: mac_filesystem::File)
{
    use mac_formats::{native::{Descriptor, Role, image},
                      resource};
    let mut fork = file.resources;
    let budget = resource::stream::Budget { payload_bytes: 256 * 1024 * 1024,
                                            ..Default::default() };
    let resources =
        match resource::stream::read_selected(fork.length,
                                              budget,
                                              |kind, id| {
                                                  id == 0
                                                  && matches!(&kind, b"NABI" | b"RV64" | b"RLOC")
                                              },
                                              |offset, output| {
                                                  let n = volume.read(&mut fork, offset, output)?;
                                                  if n == output.len()
                                                  {
                                                      Ok(())
                                                  }
                                                  else
                                                  {
                                                      Err(mac_formats::Error::Malformed("short \
                                                                                         System \
                                                                                         resource \
                                                                                         read"))
                                                  }
                                              })
        {
            Ok(resources) => resources,
            Err(error) =>
            {
                puts("BOOT_AARCH64_SYSTEM_REFUSED: resources unreadable: ");
                puts(match error
                {
                    _ => "resource stream refused"
                });
                puts("\n");
                return;
            }
        };
    let find = |kind: [u8; 4]| resources.iter().find(|row| row.kind == kind && row.id == 0);
    let Some(abi) = find(*b"NABI")
    else
    {
        puts("BOOT_AARCH64_SYSTEM_REFUSED: no native ABI resource\n");
        return;
    };
    let descriptor = match Descriptor::decode(&abi.data)
    {
        Ok(descriptor) => descriptor,
        Err(_) =>
        {
            puts("BOOT_AARCH64_SYSTEM_REFUSED: the ABI resource is malformed\n");
            return;
        }
    };
    if descriptor.role != Role::System || descriptor.abi_version != mac_abi::ABI_VERSION
    {
        puts("BOOT_AARCH64_SYSTEM_REFUSED: the executable is not a compatible System\n");
        return;
    }
    let Some(code) = find(*b"RV64")
    else
    {
        puts("BOOT_AARCH64_SYSTEM_REFUSED: no code resource\n");
        return;
    };
    let plan = match image::LoadPlan::new(&code.data, &descriptor, None, descriptor.load_base)
    {
        Ok(plan) => plan,
        Err(_) =>
        {
            puts("BOOT_AARCH64_SYSTEM_REFUSED: the code resource does not match its ABI record\n");
            return;
        }
    };
    puts("BOOT_AARCH64_PLAN: entry=");
    puthex(plan.entry());
    puts(" length=");
    puthex(plan.length() as u64);
    puts(" load_base=");
    puthex(descriptor.load_base);
    puts(" load_limit=");
    puthex(descriptor.load_limit);
    puts("\n");
}

/// Locates the blessed System Folder on the card. The partition table, the
/// volume and the folder are all read through the shared filesystem stack, so
/// this exercises the same code the kernel uses rather than a bootloader-only
/// reader. The kernel gains no FAT32 or SD knowledge from it: the bootloader
/// hands on a loaded image.
fn find_system<D: mac_filesystem::BlockDevice>(disk: &mut D)
{
    use mac_block::BlockDevice;
    use mac_filesystem::{Format, Partition, Volume};
    // The table format comes from the shared reader, not from the filesystem
    // crate: an MBR medium is not an Apple partition map, and reading it with
    // the wrong parser reports a valid disk as having no partitions.
    let partitions = match mac_block::mbr_partitions(disk)
    {
        Ok(partitions) => partitions,
        Err(_) =>
        {
            // An unpartitioned medium is a whole-disk volume, not a refusal.
            puts("BOOT_AARCH64_NO_PARTITION_TABLE: trying the whole disk\n");
            alloc::vec::Vec::new()
        }
    };
    puts("BOOT_AARCH64_PARTITIONS: count=");
    puthex(partitions.len() as u64);
    for partition in &partitions
    {
        puts(" type=");
        puthex(u64::from(partition.kind));
        puts(" start=");
        puthex(partition.range.start);
    }
    puts("\n");
    // A partitioned medium states its own extents; a whole-disk volume is
    // mounted as one partition covering the card.
    let mut candidates: alloc::vec::Vec<Partition> =
        partitions.into_iter().map(|partition| partition.range).collect();
    candidates.push(Partition { start:   0,
                                sectors: disk.sector_count() });
    for candidate in candidates
    {
        let mut volume = match Volume::mount(disk, candidate)
        {
            Ok(volume) => volume,
            Err(error) =>
            {
                puts("BOOT_AARCH64_MOUNT_FAILED: start=");
                puthex(candidate.start);
                puts(" sectors=");
                puthex(candidate.sectors);
                puts(" cause=");
                for byte in alloc::format!("{error}").bytes()
                {
                    putc(byte);
                }
                puts("\n");
                continue;
            }
        };
        puts("BOOT_AARCH64_VOLUME: start=");
        puthex(candidate.start);
        puts(" name=");
        let name_end = volume.name.iter().position(|byte| *byte == 0).unwrap_or(volume.name.len());
        for byte in &volume.name[..name_end]
        {
            putc(*byte);
        }
        puts(" format=");
        puts(if volume.format == Format::HfsB { "hfsb" } else { "other" });
        puts("\n");
        if volume.format != Format::HfsB
        {
            continue;
        }
        let folder = match volume.blessed_folder()
        {
            Ok(folder) => folder,
            Err(_) =>
            {
                puts("BOOT_AARCH64_SYSTEM_REFUSED: no blessed System Folder\n");
                return;
            }
        };
        puts("BOOT_AARCH64_BLESSED: cnid=");
        puthex(u64::from(folder.id));
        puts(" name=");
        let folder_end = folder.name.iter().position(|byte| *byte == 0).unwrap_or(folder.name.len());
        for byte in &folder.name[..folder_end]
        {
            putc(*byte);
        }
        puts("\n");
        match volume.find_file(folder.id, b"System")
        {
            Ok(file) =>
            {
                puts("BOOT_AARCH64_SYSTEM_FOUND: type=");
                for byte in file.file_type
                {
                    putc(byte);
                }
                puts(" data=");
                puthex(file.data.length);
                puts(" resource=");
                puthex(file.resources.length);
                puts("\n");
                load_system(&mut volume, file);
            }
            Err(_) => puts("BOOT_AARCH64_SYSTEM_REFUSED: no System file in the blessed folder\n")
        }
        return;
    }
    puts("BOOT_AARCH64_SYSTEM_REFUSED: no mountable volume\n");
}

#[panic_handler]
fn panic(_: &PanicInfo) -> !
{
    loop
    {}
}
