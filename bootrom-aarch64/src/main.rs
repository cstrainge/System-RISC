//! AArch64 EL1 admission entry: initialise the PL011 once, install the checked
//! 39-bit translation regime, report a named marker, and halt. The vector table
//! reports the fault registers instead of spinning silently.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::panic::PanicInfo;

use linked_list_allocator::LockedHeap;
use mac_sdhci::Disk;


#[global_allocator]
static HEAP: LockedHeap = LockedHeap::empty();

/// The filesystem code above the block device needs storage for its own
/// bookkeeping. It comes from the platform record's firmware heap window, which
/// is the region the boot stage owns, so the payload itself stays small enough
/// to fit beside its own stack.
fn init_heap()
{
    // SAFETY: the bootrom is single-threaded on the primary CPU, this runs once
    // before any allocation, the MMU is already on, and the window is RAM the
    // record reserves for exactly this purpose. No reference to it escapes.
    unsafe {
        HEAP.lock().init(FIRMWARE_HEAP_BASE as *mut u8, FIRMWARE_HEAP_BYTES as usize)
    };
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

// The bootstrap translation tables. They are not a section — the entry code
// writes them with plain stores, so the linker cannot place them — so their
// address comes from `link.ld`, which owns it and asserts that the five pages
// clear the stack window and stop short of the firmware heap. That ordering
// matters because the stack grows down from `__stack_top`: tables inside the
// stack window are overwritten by the first deep call, and the result is a data
// abort inside whatever function forced the walk rather than anything that names
// the tables. Stating the address here as well is what once let a comment carry
// one build's stack top into another build.
//
// The boot window and the code boundary inside it are linker symbols for the
// same reason: `link.ld` asserts that `.data` begins at the declared boundary,
// so the page tables and the assertion read one value rather than two copies of
// it that can disagree.
unsafe extern "C"
{
    // The script's own symbol; its address is the table base.
    static __tables_start: u8;
    static __platform_BOOT_START: u8;
    static __platform_BOOT_END: u8;
    static __platform_FIRMWARE_CODE_BYTES: u8;
}

/// One translation table: 512 descriptors of eight bytes.
const TABLE_BYTES: u64 = 4096;
const BLOCK: u64 = 0x20_0000;
const PAGE: u64 = 4096;
const TABLE: u64 = 0x3;
// Descriptor type bits. They differ by walk level and nothing else about the
// entry does: a level-2 block is 0b01 and a level-3 page is 0b11. Reusing one
// level's constant at the other produces a reserved descriptor, which faults on
// the first fetch rather than anywhere that names the mistake.
const BLOCK_TYPE: u64 = 0x1;
const PAGE_TYPE: u64 = 0x3;
// AttrIndx 0 (normal write-back), inner shareable, access flag, writable at EL1.
const NORMAL: u64 = 0x700;
// AP[2]=1: read-only at EL1. With neither PXN nor UXN set, a fetch is permitted.
const READ_ONLY: u64 = 0x80;
// PXN and UXN: never executable at either exception level.
const NEVER_EXECUTE: u64 = (1 << 54) | (1 << 53);
// Code: read-only and executable. It is a page because the boot window is not
// block aligned.
const CODE: u64 = NORMAL | PAGE_TYPE | READ_ONLY;
// Data: writable and never executable. The boot window's data half uses this
// page form; RAM outside the window is one 2 MiB block per entry below.
const DATA: u64 = NORMAL | PAGE_TYPE | NEVER_EXECUTE;
const DATA_BLOCK: u64 = NORMAL | BLOCK_TYPE | NEVER_EXECUTE;
// Valid block, AttrIndx 1 (Device-nGnRnE), AF, never executable.
const DEVICE: u64 = 0x405 | NEVER_EXECUTE;
// 39-bit VA at both halves, 4 KiB granule at both halves, 32-bit physical.
const TCR: u64 = 0x8019_0019;
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
    // The boot window holds code and data, and one permission cannot be right
    // for both: code must be read-only and executable, data writable and not
    // executable. The boundary is the record's `firmware_code_bytes`, which
    // `link.ld` asserts is where the linker places `.data`, so the mapping and
    // the declaration cannot drift apart when the payload grows.
    //
    // This is what a shipping OS does after it turns the MMU on, and it is what
    // lets SCTLR_EL1.WXN below be set rather than cleared: with the split, no
    // executable page is writable, so the architectural rule enforces the intent
    // instead of being switched off to permit it. The instruction-permission
    // fault that made the virt bootloader split its window belongs to QEMU's
    // shared TCG translation path - not to a CPU model, and not to silicon - and
    // it did not reproduce here on `-machine raspi3b` with the unsplit mapping;
    // this is a correctness change, not a fix for that fault.
    let boot_start = core::ptr::addr_of!(__platform_BOOT_START) as u64;
    let boot_end = core::ptr::addr_of!(__platform_BOOT_END) as u64;
    let code_bytes = core::ptr::addr_of!(__platform_FIRMWARE_CODE_BYTES) as u64;
    assert!(boot_start % PAGE == 0 && boot_end % PAGE == 0 && code_bytes % PAGE == 0,
            "the boot window and its declared code boundary must be page aligned");
    assert!(boot_start >= PHYSICAL_RAM_BASE
            && code_bytes != 0
            && boot_start + code_bytes <= boot_end,
            "the declared code boundary must lie inside the boot window");
    assert!(boot_end - PHYSICAL_RAM_BASE <= BLOCK,
            "the boot window must fit the first block, which is walked at page granularity");
    let boot_page = ((boot_start - PHYSICAL_RAM_BASE) / PAGE) as usize;
    let code_pages = (code_bytes / PAGE) as usize;
    // A 39-bit regime with a 4 KiB granule starts its walk at level 1, so
    // TTBR0/TTBR1 name the level-1 tables directly. Every descriptor we touch
    // sets AF; a table walker that reaches a descriptor without AF faults on
    // first access even when the rest of the entry looks right.
    let l1_low = core::ptr::addr_of!(__tables_start) as u64;
    let l2_low = l1_low + TABLE_BYTES;
    let l1_high = l1_low + 2 * TABLE_BYTES;
    let l2_physmap = l1_low + 3 * TABLE_BYTES;
    let l3_boot = l1_low + 4 * TABLE_BYTES;
    unsafe
    {
        for table in [l1_low, l2_low, l1_high, l2_physmap, l3_boot]
        {
            clear(table);
        }
        // The first block holds the boot window, so it is walked at page
        // granularity and each page gets the permission its contents need.
        // Nothing else in RAM is executed, so every other page is writable and
        // non-executable.
        for index in 0..512
        {
            let permission = if index >= boot_page && index < boot_page + code_pages
            {
                CODE
            }
            else
            {
                DATA
            };
            entry(l3_boot, index, PHYSICAL_RAM_BASE + index as u64 * PAGE | permission);
        }
        // One page table serves both roots: the identity map and the physmap
        // alias describe the same physical pages with the same permissions, so a
        // second copy of it would be a second thing to keep in agreement.
        entry(l1_low, 0, l2_low | TABLE);
        entry(l2_low, 0, l3_boot | TABLE);
        for index in 1..ram_blocks
        {
            entry(l2_low, index, PHYSICAL_RAM_BASE + index as u64 * BLOCK | DATA_BLOCK);
        }
        for index in ram_blocks..512
        {
            entry(l2_low, index, index as u64 * BLOCK | DEVICE);
        }
        entry(l1_high, physmap_index, l2_physmap | TABLE);
        entry(l2_physmap, 0, l3_boot | TABLE);
        for index in 1..ram_blocks
        {
            entry(l2_physmap, index, PHYSICAL_RAM_BASE + index as u64 * BLOCK | DATA_BLOCK);
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
            ttbr0 = in(reg) l1_low,
            ttbr1 = in(reg) l1_high,
            options(nostack)
        );
        core::arch::asm!(
            "mrs {sctlr}, sctlr_el1",
            "orr {sctlr}, {sctlr}, #0x80000",
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
    let identity = unsafe { core::ptr::read_volatile(0x8_0000 as *const u64) };
    let aliased = unsafe { core::ptr::read_volatile((PHYSMAP_BASE + 0x8_0000) as *const u64) };
    u64::from(identity == aliased)
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

/// Report a refused probe and stop.
///
/// A bootloader that prints a refusal and then falls through to its ready marker
/// boots as if it had succeeded: the transcript says `BOOT_AARCH64_READY` and
/// nothing distinguishes a card that was read from one that was not. The detail
/// lines below are the useful part and they are printed before this, so this
/// function adds the one line a boot case can assert and then halts.
///
/// This is deliberately terminal rather than a `return 0`: the caller used to be
/// free to ignore the value, and did. `entry.S` still checks the return as defence
/// in depth, but a failed probe can no longer reach the ready marker by any path.
fn halt_probe() -> !
{
    puts("BOOT_AARCH64_PROBE_FAILED: the card was not read, so this boot cannot continue\n");
    loop
    {
        core::hint::spin_loop();
    }
}

/// Reads sector zero from the SD card and reports the partition-table signature

/// it actually found. A card that answers but returns the wrong sector still
/// reaches here, so the signature is the evidence rather than the call returning.
#[unsafe(no_mangle)]
pub extern "C" fn sdhci_probe(tree_address: usize) -> u64
{
    use mac_block::BlockDevice;
    init_heap();
    // Peripheral addresses come from the machine's tree, not from constants, so
    // the record is a declaration to be checked rather than the runtime source.
    let Some(bytes) = firmware_tree(tree_address)
    else
    {
        puts("BOOT_AARCH64_DT_REFUSED: the machine supplied no usable device tree\n");
        halt_probe();
    };
    let tree = match mac_devicetree::Tree::new(bytes)
    {
        Ok(tree) => tree,
        Err(_) =>
        {
            puts("BOOT_AARCH64_DT_REFUSED: the supplied tree has no valid header\n");
            halt_probe();
        }
    };
    // Looked up by the tree's own `compatible`, which is the device tree's
    // vocabulary rather than this platform's inventory name for the part.
    let sdhci = match tree.window("brcm,bcm2835-mmc")
    {
        Ok(Some((base, bytes))) =>
        {
            puts("BOOT_AARCH64_DT_SDHCI: base=");
            puthex(base);
            puts(" bytes=");
            puthex(bytes);
            puts("\n");
            base as usize
        }
        _ =>
        {
            puts("BOOT_AARCH64_DT_REFUSED: no sdhci window in the tree\n");
            halt_probe();
        }
    };
    match tree.window("arm,pl011")
    {
        Ok(Some((base, _))) =>
        {
            puts("BOOT_AARCH64_DT_PL011: base=");
            puthex(base);
            puts("\n");
            // The console was initialised from the record before the tree could
            // be parsed, which is unavoidable: a bad tree has to be reported
            // somewhere. Checking the two agree turns a silent divergence into a
            // visible one.
            if base != PL011_BASE
            {
                puts("BOOT_AARCH64_DT_MISMATCH: the tree's pl011 is not the console in use\n");
            halt_probe();
            }
        }
        _ =>
        {
            puts("BOOT_AARCH64_DT_REFUSED: no pl011 window in the tree\n");
            halt_probe();
        }
    }
    let mut stage = 0u8;
    let mut disk = match Disk::probe(sdhci, &mut stage)
    {
        Ok(disk) => disk,
        Err(failure) =>
        {
            puts("BOOT_AARCH64_SD_REFUSED: stage=");
            puthex(u64::from(stage));
            puts(" what=");
            puts(failure.what);
            puts(" cmd=");
            puthex(u64::from(failure.command));
            puts(" int=");
            puthex(u64::from(failure.normal));
            puts(" err=");
            puthex(u64::from(failure.error));
            puts("\n");
            halt_probe();
        }
    };
    puts("BOOT_AARCH64_SD_READY: sectors=");
    puthex(disk.sector_count());
    puts("\n");
    let mut sector = [0u8; 512];
    if disk.read_sector(0, &mut sector).is_err()
    {
        puts("BOOT_AARCH64_SD_READ_FAILED: sector 0\n");
            halt_probe();
    }
    let signature = u16::from_le_bytes([sector[510], sector[511]]);
    puts("BOOT_AARCH64_SD_MBR: signature=");
    puthex(u64::from(signature));
    puts(" first_lba=");
    puthex(u64::from(u32::from_le_bytes([sector[454], sector[455], sector[456], sector[457]])));
    puts(" type=");
    puthex(u64::from(sector[450]));
    puts("\n");
    // Two conditions, both required. The MBR signature says the card is the one
    // this platform writes; `find_system` says the volume and the blessed System
    // were actually reachable. Returning the signature alone meant a card with a
    // valid table and no System printed the ready marker.
    if signature != 0xAA55 || !find_system(&mut disk)
    {
        halt_probe();
    }
    1
}

/// Locates the blessed System Folder on the card. The partition table, the
/// volume and the folder are all read through the shared filesystem stack, so
/// this exercises the same code the kernel uses rather than a bootloader-only
/// reader. The kernel gains no FAT32 or SD knowledge from it: the bootloader
/// hands on a loaded image.
fn find_system(disk: &mut Disk) -> bool
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
                return false;
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
        // The verdict is whether a System was found, not whether a folder existed:
        // a card with a blessed folder and no System used to reach the ready marker.
        return match volume.find_file(folder.id, b"System")
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
                true
            }
            Err(_) =>
            {
                puts("BOOT_AARCH64_SYSTEM_REFUSED: no System file in the blessed folder\n");
                false
            }
        };
    }
    puts("BOOT_AARCH64_SYSTEM_REFUSED: no mountable volume\n");
    false
}

#[panic_handler]
fn panic(_: &PanicInfo) -> !
{
    loop
    {}
}
