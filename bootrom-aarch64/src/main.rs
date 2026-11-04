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
mod sdhci;

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

/// The machine description, generated from the platform record at build time and
/// linked in read-only. The bootloader is a consumer of this tree, not its
/// author, so nothing here hardcodes a peripheral address.
static DEVICE_TREE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/devicetree.dtb"));

// Translation tables live inside the declared boot window, clear of the loaded
// image and its stack. A 39-bit regime with a 4 KiB granule starts its walk at
// level 1, so TTBR0/TTBR1 name the level-1 tables directly. Every descriptor we
// touch sets AF; a table walker that reaches a descriptor without AF faults on
// first access even when the rest of the entry looks right.
const L1_LOW: u64 = 0x10_1000;
const L2_LOW: u64 = 0x10_2000;
const L1_HIGH: u64 = 0x10_3000;
const L2_PHYSMAP: u64 = 0x10_4000;
const BLOCK: u64 = 0x20_0000;
const TABLE: u64 = 0x3;
// Valid block, AttrIndx 0 (normal write-back), inner shareable, AF.
const NORMAL: u64 = 0x701;
// Valid block, AttrIndx 1 (Device-nGnRnE), AF, never executable.
const DEVICE: u64 = 0x405 | (1 << 54) | (1 << 53);
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
    let first_device = (PL011_BASE.min(SDHCI_BASE).min(MAILBOX_BASE) / BLOCK) as usize;
    let physmap_index = ((PHYSMAP_BASE - KERNEL_HALF) / (1 << 30)) as usize;
    assert!(ram_blocks == first_device, "the device aperture must follow RAM");
    assert!(physmap_index < 512, "the physmap must sit inside the kernel half");
    unsafe
    {
        for table in [L1_LOW, L2_LOW, L1_HIGH, L2_PHYSMAP]
        {
            clear(table);
        }
        entry(L1_LOW, 0, L2_LOW | TABLE);
        for index in 0..ram_blocks
        {
            entry(L2_LOW, index, PHYSICAL_RAM_BASE + index as u64 * BLOCK | NORMAL);
        }
        for index in ram_blocks..512
        {
            entry(L2_LOW, index, index as u64 * BLOCK | DEVICE);
        }
        entry(L1_HIGH, physmap_index, L2_PHYSMAP | TABLE);
        for index in 0..ram_blocks
        {
            entry(L2_PHYSMAP, index, PHYSICAL_RAM_BASE + index as u64 * BLOCK | NORMAL);
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

/// Reads sector zero from the SD card and reports the partition-table signature
/// it actually found. A card that answers but returns the wrong sector still
/// reaches here, so the signature is the evidence rather than the call returning.
#[unsafe(no_mangle)]
pub extern "C" fn sdhci_probe() -> u64
{
    use mac_filesystem::BlockDevice;
    init_heap();
    // Peripheral addresses come from the generated tree, not from constants, so
    // the record remains the only place this board's hardware is written down.
    let tree = match devicetree::Tree::new(DEVICE_TREE)
    {
        Some(tree) => tree,
        None =>
        {
            puts("BOOT_AARCH64_DT_REFUSED: the linked tree has no valid header\n");
            return 0;
        }
    };
    let sdhci = match tree.window("sdhci")
    {
        Some((base, bytes)) =>
        {
            puts("BOOT_AARCH64_DT_SDHCI: base=");
            puthex(base);
            puts(" bytes=");
            puthex(bytes);
            puts("\n");
            base as usize
        }
        None =>
        {
            puts("BOOT_AARCH64_DT_REFUSED: no sdhci window in the tree\n");
            return 0;
        }
    };
    match tree.window("pl011")
    {
        Some((base, _)) =>
        {
            puts("BOOT_AARCH64_DT_PL011: base=");
            puthex(base);
            puts("\n");
        }
        None =>
        {
            puts("BOOT_AARCH64_DT_REFUSED: no pl011 window in the tree\n");
            return 0;
        }
    }
    let mut stage = 0u8;
    let mut disk = match sdhci::Disk::probe(sdhci, &mut stage)
    {
        Ok(disk) => disk,
        Err(_) =>
        {
            puts("BOOT_AARCH64_SD_REFUSED: stage=");
            puthex(u64::from(stage));
            puts("\n");
            return 0;
        }
    };
    puts("BOOT_AARCH64_SD_READY: sectors=");
    puthex(disk.sector_count());
    puts("\n");
    let mut sector = [0u8; 512];
    if disk.read_sector(0, &mut sector).is_err()
    {
        puts("BOOT_AARCH64_SD_READ_FAILED: sector 0\n");
        return 0;
    }
    let signature = u16::from_le_bytes([sector[510], sector[511]]);
    puts("BOOT_AARCH64_SD_MBR: signature=");
    puthex(u64::from(signature));
    puts(" first_lba=");
    puthex(u64::from(u32::from_le_bytes([sector[454], sector[455], sector[456], sector[457]])));
    puts(" type=");
    puthex(u64::from(sector[450]));
    puts("\n");
    u64::from(signature == 0xAA55)
}

#[panic_handler]
fn panic(_: &PanicInfo) -> !
{
    loop
    {}
}
