//! AArch64 EL1 admission entry: initialise the PL011 once, install the checked
//! 39-bit translation regime, report a named marker, and halt. The vector table
//! reports the fault registers instead of spinning silently.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::panic::PanicInfo;

use linked_list_allocator::LockedHeap;

mod devicetree;

/// The filesystem code above the block device needs storage for its own
/// bookkeeping. This is a bounded firmware heap in the image's own BSS, not a
/// claim about the machine's RAM.
const HEAP_BYTES: usize = 256 * 1024;

#[global_allocator]
static HEAP: LockedHeap = LockedHeap::empty();

static mut HEAP_STORAGE: [u8; HEAP_BYTES] = [0; HEAP_BYTES];

fn init_heap()
{
    // SAFETY: the bootrom is single-threaded on the primary CPU and this runs
    // once before any allocation; no reference to the storage escapes.
    unsafe { HEAP.lock().init(core::ptr::addr_of_mut!(HEAP_STORAGE) as *mut u8, HEAP_BYTES) };
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
    let tree = match devicetree::Tree::new(bytes)
    {
        Some(tree) => tree,
        None =>
        {
            puts("BOOT_AARCH64_DT_REFUSED: the machine's tree has no valid header\n");
            return 0;
        }
    };
    let (pl011, _) = match tree.window(PL011_COMPATIBLE)
    {
        Some(window) => window,
        None =>
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
    let Some((virtio, _)) = tree.window("virtio,mmio")
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
    tree.visit_nodes(|compatible, base, _bytes| {
          if compatible != "virtio,mmio"
          {
              return;
          }
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
