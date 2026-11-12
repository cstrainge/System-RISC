//! AArch64 EL1 admission entry: initialise the PL011 once, install the checked
//! 39-bit translation regime, report a named marker, and halt. The vector table
//! reports the fault registers instead of spinning silently.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::panic::PanicInfo;

use linked_list_allocator::LockedHeap;
use mac_bootloader::report;
use mac_sdhci::Disk;

mod cache;
mod display;
mod led;
mod memory;
mod monitor;

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
    unsafe { HEAP.lock().init(FIRMWARE_HEAP_BASE as *mut u8, FIRMWARE_HEAP_BYTES as usize) };
}

#[alloc_error_handler]
fn allocation_failed(layout: core::alloc::Layout) -> !
{
    // Said rather than spun on: an exhausted firmware heap in the middle of
    // reading System is otherwise a boot that stops with nothing on the line.
    puts("BOOT_AARCH64_HEAP_EXHAUSTED: bytes=");
    puthex(layout.size() as u64);
    puts("\n");
    halt()
}

core::arch::global_asm!(include_str!(concat!(env!("OUT_DIR"), "/platform-hardware.inc")),
                        include_str!("entry.S"));

include!(concat!(env!("OUT_DIR"), "/platform-memory.rs"));
include!(concat!(env!("OUT_DIR"), "/platform-handoff.rs"));
include!(concat!(env!("OUT_DIR"), "/platform-virtual.rs"));

/// The bounded resource read a startup companion gets.
///
/// The payload is retained only for as long as the check runs, so the bound is
/// what the firmware heap can pay for at that moment; the larger System image is
/// read after the check returns and drops it.
fn startup_resource_budget() -> mac_formats::resource::stream::Budget
{
    mac_formats::resource::stream::Budget { payload_bytes: HEAP.lock().free(),
                                            ..Default::default() }
}

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
    let total =
        u32::from_be(unsafe { core::ptr::read_volatile(header.add(4) as *const u32) }) as usize;
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
unsafe extern "C" {
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
// A level-3 leaf in the System's image window: writable and executable at EL1,
// because the image's code and data share the window until the kernel installs
// its own tables. This is why SCTLR_EL1.WXN is left clear below.
const PAGE_NORMAL: u64 = NORMAL | PAGE_TYPE;
// T0SZ/T1SZ 25 (39-bit halves), IRGN0/ORGN0 write-back, SH0 inner shareable,
// TG0 0b00 and TG1 0b10 for a 4 KiB granule, IPS 0b010 for 40-bit physical
// addresses. The same value the virt bootstrap programs, so the System is
// entered under one regime on both machines.
//
// Both halves walk their tables through the cache (IRGN/ORGN write-back, inner
// shareable). The upper half used to be left at zero, which is a non-cacheable
// walk: correct for the tables this bootstrap writes with the cache off, and
// wrong for the ones System builds later with it on. System writes its root
// through a cacheable mapping and then switches to it, and a walker that reads
// around the cache finds whatever was in RAM before - on a board, not the
// tables. An emulator has no cache, so it never showed.
const TCR: u64 = 0x2_b519_3519;
/// Level-3 tables the image window is given, so the window is 32 MiB.
///
/// The mapping is installed before the image is decoded, so this is a declared
/// ceiling rather than a derived length: `enter_system` refuses a plan larger
/// than the window. `link.ld` reserves the pages.
const IMAGE_TABLES: usize = 16;
// MAIR[0] normal write-back, MAIR[1] Device-nGnRnE, MAIR[2] normal
// non-cacheable, which the System maps its pool for devices outside the
// CPU's caches with.
const MAIR: u64 = 0x44_00_ff;
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
    // Three spans, not two. RAM the ARM owns ends at the record's figure; the
    // peripheral registers begin at the record's lowest MMIO window; and the
    // memory between them belongs to the VideoCore, which is where the
    // firmware allocates the framebuffer. That middle span is mapped as
    // ordinary non-executable memory in the identity map so the boot screen
    // can be drawn with ordinary stores - a Device mapping there would fault
    // on the unaligned and vector accesses a fill or copy is free to use. It
    // is not aliased in the physmap, which describes the ARM's RAM only.
    assert!(PHYSICAL_RAM_BYTES % BLOCK == 0, "RAM must end on a block boundary");
    assert!(DEVICE_APERTURE_BASE % BLOCK == 0
            && DEVICE_APERTURE_BASE >= PHYSICAL_RAM_BASE + PHYSICAL_RAM_BYTES
            && DEVICE_APERTURE_BASE < 1 << 30,
            "the peripheral aperture must be block aligned, above RAM, and inside the first gigabyte");
    let device_blocks = (DEVICE_APERTURE_BASE / BLOCK) as usize;
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
    let l2_image = l1_low + 5 * TABLE_BYTES;
    let l3_image = l1_low + 6 * TABLE_BYTES;
    // The System's image window in the kernel half. Its virtual and physical
    // bases are not congruent modulo a block, so it is mapped in pages.
    let image_l1 = ((VM_SYSTEM_IMAGE_BASE - KERNEL_HALF) / (1 << 30)) as usize;
    let image_first = ((VM_SYSTEM_IMAGE_BASE & 0x3fff_ffff) / BLOCK) as usize;
    assert!(image_l1 < 512 && image_l1 != physmap_index,
            "the image window must sit inside the kernel half, clear of the physmap");
    assert!(image_first + IMAGE_TABLES <= 512, "the image window must fit one level-2 table");
    unsafe {
        for table in [l1_low, l2_low, l1_high, l2_physmap, l3_boot, l2_image]
        {
            clear(table);
        }
        // The first block holds the boot window, so it is walked at page
        // granularity and each page gets the permission its contents need.
        // Nothing else in RAM is executed, so every other page is writable and
        // non-executable.
        for index in 0..512
        {
            let permission =
                if index >= boot_page && index < boot_page + code_pages { CODE } else { DATA };
            entry(l3_boot, index, PHYSICAL_RAM_BASE + index as u64 * PAGE | permission);
        }
        // One page table serves both roots: the identity map and the physmap
        // alias describe the same physical pages with the same permissions, so a
        // second copy of it would be a second thing to keep in agreement.
        entry(l1_low, 0, l2_low | TABLE);
        entry(l2_low, 0, l3_boot | TABLE);
        for index in 1..device_blocks
        {
            entry(l2_low, index, index as u64 * BLOCK | DATA_BLOCK);
        }
        for index in device_blocks..512
        {
            entry(l2_low, index, index as u64 * BLOCK | DEVICE);
        }
        // The second gigabyte holds no RAM on this board, only the per-core
        // interrupt controller at its base. System reads that controller from
        // its trap path, and a trap can arrive while this root is the active
        // one - during a storage callback, say - so it is mapped here too, as
        // one device block.
        entry(l1_low, 1, (1 << 30) | DEVICE);
        entry(l1_high, physmap_index, l2_physmap | TABLE);
        entry(l2_physmap, 0, l3_boot | TABLE);
        for index in 1..ram_blocks
        {
            entry(l2_physmap, index, PHYSICAL_RAM_BASE + index as u64 * BLOCK | DATA_BLOCK);
        }
        entry(l1_high, image_l1, l2_image | TABLE);
        for k in 0..IMAGE_TABLES
        {
            let table = l3_image + (k as u64) * TABLE_BYTES;
            clear(table);
            entry(l2_image, image_first + k, table | TABLE);
            for j in 0..512u64
            {
                let offset = (k as u64) * BLOCK + j * PAGE;
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
            ttbr0 = in(reg) l1_low,
            ttbr1 = in(reg) l1_high,
            options(nostack)
        );
        core::arch::asm!(
            "mrs {sctlr}, sctlr_el1",
            // WXN clear: the image window above is writable and executable.
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

/// One byte to the console, waiting for room in the transmit FIFO.
fn transmit(byte: u8)
{
    while unsafe { core::ptr::read_volatile((PL011_BASE + 0x18) as *const u32) } & 0x20 != 0
    {}
    unsafe { core::ptr::write_volatile(PL011_BASE as *mut u32, u32::from(byte)) };
}

/// A line feed alone moves a terminal down without returning it to the left
/// margin, so every marker after the first started where the last one ended.
/// The carriage return is added here, once, so no marker has to carry it; the
/// entry code's `uart_putc` does the same.
fn putc(byte: u8)
{
    if byte == b'\n'
    {
        transmit(b'\r');
    }
    transmit(byte);
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
    // One blink per stage passed; the count is the last stage reached.
    led::led_blink();
    let tree = match mac_devicetree::Tree::new(bytes)
    {
        Ok(tree) => tree,
        Err(_) =>
        {
            puts("BOOT_AARCH64_DT_REFUSED: the supplied tree has no valid header\n");
            halt_probe();
        }
    };
    led::led_blink();
    // The translation tables were built from the record's RAM figure before the
    // tree could be read. The firmware decides the real one - it keeps part of
    // the board's memory for the VideoCore - so the tree's answer is compared
    // with the record's rather than assumed to match it. A machine with less
    // than the record states is refused: everything placed above the real end
    // of RAM would land in memory the firmware owns.
    let mut banks = 0u64;
    let mut ram_base = 0u64;
    let mut ram_bytes = 0u64;
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
        halt_probe();
    }
    puts("BOOT_AARCH64_RAM: banks=");
    puthex(banks);
    puts(" base=");
    puthex(ram_base);
    puts(" bytes=");
    puthex(ram_bytes);
    puts(" record=");
    puthex(PHYSICAL_RAM_BYTES);
    puts("\n");
    if banks == 0 || ram_base != PHYSICAL_RAM_BASE || ram_bytes < PHYSICAL_RAM_BYTES
    {
        puts("BOOT_AARCH64_RAM_REFUSED: the machine's RAM is absent or below the platform \
              minimum\n");
        halt_probe();
    }
    // The firmware leaves the tree in the middle of RAM, where it splits the
    // largest free extent this board has: with it there, no aligned block big
    // enough for the System heap exists. The firmware cannot be told to put it
    // elsewhere - `device_tree_address` makes it fail to load the blob at all,
    // observed on this board - so the bootstrap moves it to the tail the record
    // reserves for it, now that RAM is known to reach that far, and everything
    // from here on reads the moved copy. The old bytes become ordinary RAM.
    let tail = PHYSICAL_RAM_BASE + PHYSICAL_RAM_BYTES - FDT_TAIL_BYTES;
    let (tree_address, bytes, tree) = if tree_address as u64 + bytes.len() as u64 <= tail
                                         && bytes.len() as u64 <= FDT_TAIL_BYTES
    {
        // SAFETY: the destination is RAM the tree just confirmed exists, the
        // boot translation maps it writable, nothing else has been placed
        // there, and the source ends below it so the two cannot overlap.
        let moved = unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), tail as *mut u8, bytes.len());
            core::slice::from_raw_parts(tail as *const u8, bytes.len())
        };
        match mac_devicetree::Tree::new(moved)
        {
            Ok(tree) =>
            {
                puts("BOOT_AARCH64_DT_MOVED: to=");
                puthex(tail);
                puts("\n");
                (tail as usize, moved, tree)
            }
            Err(_) =>
            {
                puts("BOOT_AARCH64_DT_REFUSED: the moved tree does not read back\n");
                halt_probe();
            }
        }
    }
    else
    {
        (tree_address, bytes, tree)
    };
    // Looked up by the tree's own `compatible`, which is the device tree's
    // vocabulary rather than this platform's inventory name for the part. The
    // lookup ignores `status`: the base tree disables this node because the SD
    // slot belongs to the SDHOST controller in Linux's driver assignment, but
    // the bootrom reading its own boot device is not a driver-binding question.
    let sdhci = match tree.window_including_disabled("brcm,bcm2835-mmc")
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
    led::led_blink();
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
    led::led_blink();
    // The display comes up before the card is probed, so a failure here is
    // visible on the screen while the rest of the boot proceeds. A display
    // that refuses does not stop the boot: the kernel's driver is the
    // authoritative display path and may succeed where this one did not.
    match display::init(&tree)
    {
        Ok(framebuffer) =>
        {
            puts("BOOT_AARCH64_DISPLAY_READY: ");
            puthex(u64::from(framebuffer.width));
            puts("x");
            puthex(u64::from(framebuffer.height));
            puts(" pitch=");
            puthex(u64::from(framebuffer.pitch));
            puts(" base=");
            puthex(u64::from(framebuffer.base));
            puts("\n");
        }
        Err(error) =>
        {
            puts("BOOT_AARCH64_DISPLAY_REFUSED: ");
            for byte in alloc::format!("{error}").bytes()
            {
                putc(byte);
            }
            puts("\n");
        }
    }
    // The display init returned either way; the blink marks that, not that
    // it succeeded.
    led::led_blink();
    mac_bootloader::install_log(console);
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
        halt_probe();
    }
    puts("BOOT_AARCH64_CPUS: count=");
    puthex(harts);
    puts(" first=");
    puthex(primary_hart);
    puts("\n");
    if harts == 0
    {
        puts("BOOT_AARCH64_CPUS_REFUSED: the machine's tree describes no CPU\n");
        halt_probe();
    }
    // The account the kernel will validate: the machine's banks, and every
    // extent this boot occupies. The tree itself is reserved where the machine
    // put it, which on this machine is part way into RAM rather than in a tail.
    let total = bytes.len() as u64;
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
                     memory::Error::Reservation =>
                     {
                         "a reservation is empty, unaligned or outside RAM"
                     }
                     memory::Error::Duplicate => "two reservations overlap",
                     memory::Error::TreeOutsideRam => "the tree lies outside the RAM it describes",
                     memory::Error::Allocation => "allocation failed",
                     memory::Error::TreeUnreadable => "the machine's tree could not be walked"
                 });
            puts("\n");
            halt_probe();
        }
    };
    match account.checked(memory::PAGE_BYTES)
    {
        Ok(_) =>
        {}
        Err(_) =>
        {
            puts("BOOT_AARCH64_MEMORY_REFUSED: the account does not satisfy the kernel's checks\n");
            halt_probe();
        }
    }
    puts("BOOT_AARCH64_MEMORY_ACCOUNT: banks=");
    puthex(account.ram.len() as u64);
    puts(" reservations=");
    puthex(account.reserved.len() as u64);
    puts("\n");
    // This board has no GIC: its interrupt controller is the BCM2836 local
    // controller, which is also what releases the other cores.
    // The interrupt controller the monitor signals through, from the machine's
    // tree. A machine that declares none cannot be admitted: the kernel needs a
    // route it can publish and check, and inventing one would be a claim about
    // hardware this platform may not have.
    let mut controller = (0u64, 0u64);
    let _ = tree.for_each_node(|node| {
                    if controller.1 == 0
                       && node.compatible == Some("brcm,bcm2836-l1-intc")
                       && let Ok(Some((base, bytes))) = tree.cpu_window(node)
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
        halt_probe();
    }
    let monitor = match monitor::prepare(&account,
                                         &cpus,
                                         mac_abi::BootMemoryRange { base:   controller.0,
                                                                    length: controller.1 },
                                         counter,
                                         primary_hart)
    {
        Ok(monitor) => monitor,
        Err(cause) =>
        {
            puts("BOOT_AARCH64_MONITOR_REFUSED: ");
            puts(match cause
                 {
                     monitor::Error::NoController => "the machine declares no interrupt controller",
                     monitor::Error::Allocation => "monitor storage could not be allocated",
                     monitor::Error::Directory =>
                     {
                         "the directory fails the kernel's admission checks"
                     }
                 });
            puts("\n");
            halt_probe();
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
        halt_probe();
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
    led::led_blink();
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
    if signature != 0xaa55
    {
        halt_probe();
    }
    led::led_blink();
    let Some(loaded) = find_system(&mut disk)
    else
    {
        puts(&alloc::format!("{}\n", report::no_system(1)));
        halt_probe();
    };
    // The storage contract is expressed over trait objects because the ABI
    // names no device type; the handoff takes ownership of the card.
    let mut storage: alloc::vec::Vec<alloc::boxed::Box<dyn mac_block::WritableBlockDevice>> =
        alloc::vec::Vec::new();
    storage.push(alloc::boxed::Box::new(disk));
    enter_system(&account, &cpus, &monitor, &mut storage, &tree, tree_address as u64, &loaded)
}

/// The shared bootstrap code's console: this machine's serial line.
fn console(arguments: core::fmt::Arguments<'_>)
{
    struct Line;
    impl core::fmt::Write for Line
    {
        fn write_str(&mut self, text: &str) -> core::fmt::Result
        {
            puts(text);
            Ok(())
        }
    }
    let _ = core::fmt::Write::write_fmt(&mut Line, arguments);
}

/// Locates the blessed System Folder on the card. The partition table, the
/// volume and the folder are all read through the shared filesystem stack, so
/// this exercises the same code the kernel uses rather than a bootloader-only
/// reader. The kernel gains no FAT32 or SD knowledge from it: the bootloader
/// hands on a loaded image.
fn find_system(disk: &mut Disk) -> Option<Loaded>
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
                return None;
            }
        };
        puts("BOOT_AARCH64_BLESSED: cnid=");
        puthex(u64::from(folder.id));
        puts(" name=");
        let folder_end =
            folder.name.iter().position(|byte| *byte == 0).unwrap_or(folder.name.len());
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
                // A bootable installation includes a compatible Finder, and
                // the shared admission decides it before System is entered.
                puts("BOOT_AARCH64_FINDER: checking\n");
                if let Err(error) =
                    mac_bootloader::finder::admit(&mut volume,
                                                  folder.id,
                                                  startup_resource_budget(),
                                                  mac_bootloader::finder::Application {
                                                      image_base: VM_APPLICATION_IMAGE_BASE,
                                                      end:        VM_APPLICATION_END,
                                                      page_bytes: memory::PAGE_BYTES })
                {
                    puts(&alloc::format!("{}\n", report::refused(error)));
                    return None;
                }
                puts("BOOT_AARCH64_LOADING: reading System\n");
                load_system(&mut volume, file).map(|mut loaded| {
                                                  loaded.partition_start = candidate.start;
                                                  loaded
                                              })
            }
            Err(_) =>
            {
                puts("BOOT_AARCH64_SYSTEM_REFUSED: no System file in the blessed folder\n");
                None
            }
        };
    }
    puts("BOOT_AARCH64_SYSTEM_REFUSED: no mountable volume\n");
    None
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
                                               -> Option<Loaded>
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
                // The cause comes from the shared reader, so the same unreadable
                // fork reads the same on either bootstrap.
                puts(&alloc::format!("{}\n", report::refused(error)));
                return None;
            }
        };
    let find = |kind: [u8; 4]| resources.iter().find(|row| row.kind == kind && row.id == 0);
    let Some(abi) = find(*b"NABI")
    else
    {
        puts(&alloc::format!("{}\n", report::refused(report::NO_NATIVE_ABI)));
        return None;
    };
    let descriptor = match Descriptor::decode(&abi.data)
    {
        Ok(descriptor) => descriptor,
        Err(error) =>
        {
            puts(&alloc::format!("{}\n", report::refused(error)));
            return None;
        }
    };
    if descriptor.role != Role::System || descriptor.abi_version != mac_abi::ABI_VERSION
    {
        puts(&alloc::format!("{}\n", report::refused(report::NOT_COMPATIBLE_SYSTEM)));
        return None;
    }
    // The image must occupy the window this platform reserved for it; a System
    // linked for another machine is refused here, in the shared words, before
    // its code is read.
    if let Err(error) =
        mac_bootloader::reservation::system(&descriptor,
                                            mac_bootloader::reservation::Window {
                                                base:  VM_SYSTEM_IMAGE_BASE,
                                                limit: VM_SYSTEM_IMAGE_LIMIT
                                            })
    {
        puts(&alloc::format!("{}\n", report::refused(error)));
        return None;
    }
    let Some(code) = find(*b"RV64")
    else
    {
        puts(&alloc::format!("{}\n", report::refused(report::NO_CODE_RESOURCE)));
        return None;
    };
    let plan = match image::LoadPlan::new(&code.data, &descriptor, None, descriptor.load_base)
    {
        Ok(plan) => plan,
        Err(error) =>
        {
            puts(&alloc::format!("{}\n", report::refused(error)));
            return None;
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
    // The translation window `mmu_init` installed already points at
    // `IMAGE_BASE`, so writing here is what makes that window resolve to System
    // rather than to whatever the machine left in RAM. The window is mapped in
    // 4 KiB pages because its virtual and physical bases are not congruent
    // modulo a 2 MiB block, which no block descriptor can express.
    //
    // SAFETY: `IMAGE_BASE` names the reserved physical image window the record
    // declares and the boot translation maps writable, `plan.length()` is the
    // decoded extent that window was sized for, and no reference into this
    // region exists yet.
    let output = unsafe { core::slice::from_raw_parts_mut(IMAGE_BASE as *mut u8, plan.length()) };
    if plan.write(output).is_err()
    {
        puts(&alloc::format!("{}\n", report::refused("the decoded plan does not fit its window")));
        return None;
    }
    // The image was written through the identity map and will be fetched
    // through the kernel-half window. Those are two names for the same bytes,
    // and on hardware the instruction side does not see a store until the line
    // has been cleaned and the instruction caches invalidated; an emulator
    // without caches never shows the difference.
    cache::clean(IMAGE_BASE as usize, plan.length());
    // SAFETY: instruction-cache invalidation and barriers have no memory
    // operands and change no architectural state the bootstrap depends on.
    unsafe { core::arch::asm!("ic iallu", "dsb sy", "isb", options(nostack, preserves_flags)) };
    // Read back through the window rather than trusting the write: the point of
    // this step is that the *virtual* address resolves to the loaded image, and
    // only a read at that address tests the translation rather than the copy.
    let entry_word = unsafe { core::ptr::read_volatile(plan.entry() as *const u32) };
    puts("BOOT_AARCH64_IMAGE_LOADED: bytes=");
    puthex(plan.length() as u64);
    puts(" entry_word=");
    puthex(u64::from(entry_word));
    puts("\n");
    Some(Loaded { entry:           plan.entry(),
                  load_base:       descriptor.load_base,
                  load_limit:      descriptor.load_limit,
                  extent:          plan.length() as u64,
                  segments:        plan.segments().to_vec(),
                  partition_start: 0,
                  disk_index:      0 })
}

/// What the load produced, so the handoff runs where the memory account, the
/// CPU identities and the monitor are all in scope rather than being threaded
/// through the volume walk.
struct Loaded
{
    entry:           u64,
    load_base:       u64,
    load_limit:      u64,
    extent:          u64,
    segments:        alloc::vec::Vec<mac_formats::native::image::Segment>,
    partition_start: u64,
    disk_index:      usize
}

/// The firmware power conduit the machine's tree declares.
///
/// PSCI defines two conduits, `hvc` and `smc`, and names them in the `/psci`
/// node's `method` property. The answer is reported by name whatever it is, so a
/// machine that declares something else is named rather than mistaken for one
/// with no conduit at all, and a refusal further down can be read against what
/// the machine actually said.
fn power_conduit(tree: &mac_devicetree::Tree<'_>) -> u64
{
    use mac_abi::boot::conduit;
    let node = match tree.find(|candidate| candidate.depth == 1 && candidate.name == "psci")
    {
        Ok(node) => node,
        Err(_) =>
        {
            // The CPUs and the RAM were read out of this same blob, so a walk
            // that fails here is reported rather than treated as a machine
            // without a power interface.
            puts("BOOT_AARCH64_PSCI_REFUSED: the machine's tree could not be read at /psci\n");
            return conduit::NONE;
        }
    };
    let Some(node) = node
    else
    {
        puts("BOOT_AARCH64_PSCI: absent\n");
        return conduit::NONE;
    };
    let Some(method) = node.method
    else
    {
        puts("BOOT_AARCH64_PSCI_REFUSED: the machine's /psci node states no method, so no \
              conduit is named\n");
        return conduit::NONE;
    };
    puts("BOOT_AARCH64_PSCI: method=");
    for byte in method.bytes()
    {
        putc(byte);
    }
    puts("\n");
    match method
    {
        "hvc" => conduit::HVC,
        "smc" => conduit::SMC,
        _ =>
        {
            puts("BOOT_AARCH64_PSCI_REFUSED: the PSCI binding defines hvc and smc only\n");
            conduit::OTHER
        }
    }
}

/// Builds the record System is admitted with and enters it at EL1.
///
/// Everything here is derived rather than declared: the mapping record comes
/// from the decoded image, the CPU identities from the machine's tree, the
/// monitor from its own admission checks, and the storage contract from the
/// shared module. The only constants are the ones the record states, and each is
/// checked by the kernel against the same value on the other side of the jump.
fn enter_system(memory: &memory::Memory, cpus: &[mac_abi::boot::topology::Cpu],
                monitor: &monitor::Published,
                disks: &mut alloc::vec::Vec<alloc::boxed::Box<dyn mac_block::WritableBlockDevice>>,
                tree: &mac_devicetree::Tree<'_>, tree_address: u64, loaded: &Loaded)
                -> !
{
    use mac_abi::{BootInfo, BootMemoryRange, Header,
                  boot::execution::{EXECUTE, Execution, FIRMWARE_ONLY, READ, Region, WRITE}};
    let account = match memory.checked(memory::PAGE_BYTES)
    {
        Ok(account) => account,
        Err(error) =>
        {
            puts(&alloc::format!("{}\n",
                                 report::refused(alloc::format!("the memory account does not \
                                                                 satisfy the kernel: {error}"))));
            halt();
        }
    };
    let physical_image = BootMemoryRange { base:   IMAGE_BASE,
                                           length: loaded.extent }; // The window was mapped before the image was decoded, so the plan is checked
    // against the ceiling rather than the ceiling being derived from the plan. A
    // plan that outgrew the mapping would be written past the last table and
    // fault on first instruction, which reads as a code defect rather than as a
    // window that was too small.
    if loaded.extent > IMAGE_TABLES as u64 * BLOCK
    {
        puts(&alloc::format!("{}\n",
                             report::refused("the image is larger than the mapped window")));
        halt();
    }
    let virtual_image = BootMemoryRange { base:   loaded.load_base,
                                          length: loaded.extent };
    // The image's own segments, converted by the shared record builder so this
    // bootstrap and the RISC-V one cannot round an extent differently.
    let mut regions: alloc::vec::Vec<Region> =
        match mac_bootloader::image_regions::system_regions(&loaded.segments,
                                                            loaded.load_base,
                                                            loaded.load_limit,
                                                            IMAGE_BASE,
                                                            memory::PAGE_BYTES)
        {
            Ok(regions) => regions,
            Err(error) =>
            {
                // The record builder names the region it rejected, so the cause
                // is carried rather than replaced with "not admissible".
                puts(&alloc::format!("{}\n", report::refused(error)));
                halt();
            }
        };
    // The firmware window is described as well, so the kernel knows which bytes
    // of RAM are this bootstrap rather than free memory. `FIRMWARE_ONLY` is what
    // says so: without it the kernel validates the region as part of the image,
    // where an identity mapping at this address is not inside the image window.
    // Its virtual base equals its physical base because the low half is an
    // identity map, which is what a firmware-only region requires.
    //
    // `EXECUTE` because this window is code the kernel calls into: `kernel::log`
    // hands bytes to the firmware callback through `with_firmware`, which
    // switches to the firmware root for exactly that call. Declared without it,
    // the call took a permission fault at level 3 - the memory was mapped, the
    // fetch was not permitted - which is a fault the declaration caused rather
    // than one the machine had.
    // The firmware window holds code and data, and one permission cannot be
    // right for both. Code is read-only and executable; data is writable and
    // not executable. The boundary is the record's `firmware_code_bytes`, which
    // is where the linker places .data, so it is declared rather than guessed
    // from a section symbol at run time.
    //
    // This matters beyond hygiene: this core refuses to execute a page that is
    // also writable, even with SCTLR_EL1.WXN clear. A single writable-and-
    // executable window made every call into firmware take a permission fault
    // on fetch, which is why the window is split here.
    regions.push(Region { physical:     BootMemoryRange { base:   BOOT_BASE,
                                                          length: FIRMWARE_CODE_BYTES },
                          virtual_base: BOOT_BASE,
                          flags:        READ | EXECUTE | FIRMWARE_ONLY,
                          reserved:     0 });
    regions.push(Region { physical:     BootMemoryRange { base:   BOOT_BASE + FIRMWARE_CODE_BYTES,
                                                   length: BOOT_BYTES - FIRMWARE_CODE_BYTES },
                   virtual_base: BOOT_BASE + FIRMWARE_CODE_BYTES,
                   flags:        READ | WRITE | FIRMWARE_ONLY,
                   reserved:     0 });
    // The firmware heap the record declares, identity mapped for the same
    // reason the window above is: the bootstrap allocates from it by its own
    // address, and the physmap alias is not where its allocator points.
    regions.push(Region { physical:     BootMemoryRange { base:   FW_HEAP_BASE,
                                                          length: FW_HEAP_BYTES },
                          virtual_base: FW_HEAP_BASE,
                          flags:        READ | WRITE | FIRMWARE_ONLY,
                          reserved:     0 });
    let execution = Execution { hart: 0,
                                // TTBR0_EL1 is the root the kernel authenticates;
                                // the kernel half is selected separately by the
                                // layout, which is why it is not named here.
                                root_physical: core::ptr::addr_of!(__tables_start) as u64,
                                // Sixteen bits of address-space tag is what this
                                // layout carries, and the kernel refuses a mask
                                // that is not a power-of-two span.
                                hardware_tag_mask: 0xffff,
                                physical_image,
                                virtual_image,
                                regions: regions.as_ptr(),
                                region_count: regions.len() };
    let flash = BootMemoryRange { base:   BOOT_BASE,
                                  length: BOOT_BYTES };
    if let Err(cause) = execution.checked(account, &regions, physical_image, virtual_image, flash)
    {
        // Named rather than summarised: "the record is not admissible" would
        // send a reader back to this function with no idea which field the
        // kernel rejected.
        puts(&alloc::format!("{}\n", report::refused(cause)));
        halt();
    }
    let mut context = mac_bootloader::volume::BootVolume { disks:      disks.as_mut_ptr(),
                                                           disk_count: disks.len() };
    // A machine that powers off through firmware says so in the same tree that
    // describes its peripherals, and this is the boot that reads it. The node
    // has no register window, so nothing enumerates it as a device: its
    // `method` property names the conduit instead. The bootstrap reports what
    // the machine declared and hands the declaration on; whether this System can
    // use it is the power driver's answer, so a conduit this build does not
    // implement leaves the machine running rather than claiming a capability.
    let declared_conduit = power_conduit(tree);
    let info = BootInfo { header:                  Header::for_type::<BootInfo>(),
                          ram_base:                memory.ram[0].base,
                          ram_size:                memory.ram[0].length,
                          log:                     Some(log),
                          storage_context:
                              (&mut context as *mut mac_bootloader::volume::BootVolume).cast(),
                          disk_info:               Some(mac_bootloader::volume::disk_info),
                          read_sectors:            Some(mac_bootloader::volume::read_sectors),
                          write_sector:            Some(mac_bootloader::volume::write_sector),
                          flush_disk:              Some(mac_bootloader::volume::flush_disk),
                          startup_disk:            loaded.disk_index as u32,
                          // Derived from the record's declared display, not
                          // hardcoded: a machine that declares no display gets a
                          // headless boot, and one that declares a virtio GPU
                          // gets a graphical one. Whether the display path then
                          // works is the driver's question, not this flag's.
                          flags:                   BOOT_FLAGS,
                          startup_partition_start: loaded.partition_start,
                          ram_regions:             memory.ram.as_ptr(),
                          ram_region_count:        memory.ram.len(),
                          reservations:            memory.reserved.as_ptr(),
                          reservation_count:       memory.reserved.len(),
                          display_scratch:         BootMemoryRange { base:   DISPLAY_SCRATCH_BASE,
                                                                     length: DISPLAY_SCRATCH_BYTES },
                          execution:               &execution,
                          cpus:                    cpus.as_ptr(),
                          cpu_count:               cpus.len(),
                          monitor:                 &monitor.directory,
                          monitor_layout:          mac_abi::boot::monitor::LAYOUT_FINGERPRINT,
                          device_tree:             tree_address as *const u8,
                          device_tree_bytes:       tree.total_size(),
                          power_conduit:           declared_conduit };
    // Seal the profile System checks against its own architecture adapter. The
    // kernel compares this against a constant it holds rather than against
    // anything this bootstrap said, so the two statements agreeing is the check:
    // a loader built for a different profile is refused, not admitted on its own
    // word. The seal is written last, because an unsealed profile is refused.
    let vector = vector_address();
    let image = monitor.image();
    image.required_isa.store(REQUIRED_ISA, core::sync::atomic::Ordering::Release);
    image.entry.store(loaded.entry, core::sync::atomic::Ordering::Release);
    image.image_base.store(loaded.load_base, core::sync::atomic::Ordering::Release);
    image.image_bytes.store(loaded.extent, core::sync::atomic::Ordering::Release);
    let limit = physical_limit(memory, tree);
    puts("BOOT_AARCH64_PHYSICAL_LIMIT: ");
    puthex(limit);
    puts("\n");
    image.physical_limit.store(limit, core::sync::atomic::Ordering::Release);
    image.vector_bytes.store(VECTOR_BYTES, core::sync::atomic::Ordering::Release);
    image.hardware_tag_mask.store(0xffff, core::sync::atomic::Ordering::Release);
    // Both stubs are this bootstrap's own trap vector: it receives a fault taken
    // by System, and it is what an unexpected return lands on. The kernel
    // requires only that they are instruction-aligned and inside firmware, which
    // the boot window is declared to be.
    image.fault_entry.store(vector, core::sync::atomic::Ordering::Release);
    image.returned_entry.store(vector, core::sync::atomic::Ordering::Release);
    image.sealed.store(1, core::sync::atomic::Ordering::Release);
    // System reads the record through the physmap, not through the identity map:
    // the kernel half is where its own address space lives, and the identity map
    // is firmware's. Both aliases name the same bytes, and the record is left on
    // this stack, which the kernel inherits rather than rebases.
    let physical = |address: u64| -> u64 {
        PHYSMAP_BASE + address.checked_sub(PHYSICAL_RAM_BASE).unwrap_or(0)
    };
    let info_virtual = physical((&info as *const BootInfo) as u64);
    let stack: u64;
    // SAFETY: reading the stack pointer has no side effects.
    unsafe { core::arch::asm!("mov {}, sp", out(reg) stack, options(nomem, nostack)) };
    let stack_virtual = physical(stack);
    puts("BOOT_AARCH64_HANDOFF: entry=");
    puthex(loaded.entry);
    puts(" info=");
    puthex(info_virtual);
    puts(" stack=");
    puthex(stack_virtual);
    puts(" regions=");
    puthex(regions.len() as u64);
    puts("\n");
    puts(report::ENTERING_SYSTEM);
    puts(" at ");
    puthex(loaded.entry);
    puts(" privilege=EL1\n");
    // The image must not inherit a return address from here. `br` does not set
    // LR, so whatever the last `bl` in this bootstrap left in it survives into
    // System - a bootrom address that the kernel space does not map for
    // execution. A stray `ret` in the image would then branch straight back
    // into firmware and take a permission fault there.
    let link: u64;
    // SAFETY: reading the link register has no side effects.
    unsafe { core::arch::asm!("mov {}, x30", out(reg) link, options(nomem, nostack)) };
    puts("BOOT_AARCH64_LINK: x30=");
    puthex(link);
    puts("\n");
    // SAFETY: the image is loaded and mapped at `entry`, the record and its
    // borrowed arrays stay pinned for the System lifetime, and this branch never
    // returns. Interrupts are masked because the kernel installs its own vector
    // table and expects to unmask on its own terms.
    unsafe {
        core::arch::asm!(
            "msr daifset, #0xf",
            "mov x0, {info}",
            "mov x1, {bytes}",
            "mov x2, {role}",
            "mov sp, {stack}",
            // The image's entry returns a services pointer, so it ends in `ret`.
            // LR must therefore name somewhere valid: a halt loop here, so a
            // returned entry stops in firmware instead of jumping to zero. This
            // was previously zeroed, which made every boot fault at address 0.
            "adr x30, 1f",
            "br {entry}",
            "1:",
            "wfe",
            "b 1b",
            info = in(reg) info_virtual,
            bytes = in(reg) core::mem::size_of::<BootInfo>() as u64,
            role = in(reg) mac_abi::boot::monitor::ENTRY_PRIMARY,
            stack = in(reg) stack_virtual,
            entry = in(reg) loaded.entry,
            options(noreturn)
        );
    }
}

/// System's console.
///
/// `serial!` inside System writes through `BootInfo.log` rather than to a UART of
/// its own, so a bootstrap that passes `None` silently discards every diagnostic
/// System emits - including the markers that say whether it booted. Passing a
/// sink is not optional decoration; it is the only channel System has.
unsafe extern "C" fn log(bytes: *const u8, length: usize) -> mac_abi::Status
{
    for index in 0..length
    {
        // SAFETY: the caller supplies a readable buffer of `length` bytes, and
        // the console window is the one this bootstrap initialised.
        let byte = unsafe { core::ptr::read_volatile(bytes.add(index)) };
        putc(byte);
    }
    mac_abi::OK
}

/// The ISA profile this bootstrap states it provides.
///
/// Written here independently of the kernel's constant on purpose. The kernel
/// compares the sealed value against its own architecture adapter's statement,
/// so two independent statements agreeing is the check; reading one source on
/// both sides would make the comparison vacuous and admit a loader built for a
/// different profile.
///
/// AArch64 has no MISA. These bits state what System's code assumes: EL1 with a
/// 4 KiB granule, the floating-point and NEON unit, and the generic timer.
const REQUIRED_ISA: u64 = (1 << 0) | (1 << 1) | (1 << 2);

/// The exception vector table: sixteen slots of 128 bytes.
const VECTOR_BYTES: u64 = 2048;

/// The address of this bootstrap's trap vector.
fn vector_address() -> u64
{
    unsafe extern "C" {
        fn vectors();
    }
    // SAFETY: taking the address of a defined symbol has no effect.
    vectors as *const () as u64
}

/// The exclusive maximum of admitted RAM and every peripheral window this
/// machine declares.
///
/// System computes the same quantity from the same inputs and compares, so a
/// bootstrap that described a smaller machine than it mapped is refused rather
/// than trusted. The windows come from the machine's tree rather than from a
/// compiled list, and the walk takes the highest end over every node of each
/// kind, so a machine that models one node per slot and one that models a single
/// bank are both answered.
fn physical_limit(memory: &memory::Memory, tree: &mac_devicetree::Tree<'_>) -> u64
{
    // The bootstrap windows the record declares for System, which System
    // counts whether or not the tree repeats them.
    let mut end = BOOTSTRAP_MMIO_LIMIT;
    for bank in &memory.ram
    {
        end = end.max(bank.base.saturating_add(bank.length));
    }
    // Every peripheral the machine describes, not a list of the parts this
    // bootstrap happens to know: the kernel derives the same limit from the same
    // tree, and a limit that disagreed with it would refuse the boot. Naming
    // parts here would make a machine with an unfamiliar peripheral fail its own
    // seal.
    let _ = tree.for_each_node(|node| {
                    if !node.enabled()
                    {
                        return;
                    }
                    // The same nodes System's inventory admits, at the
                    // addresses the CPU finds them: a part that names itself
                    // and states a window some bus maps.
                    if node.compatible.is_some()
                       && let Ok(Some((base, bytes))) = tree.cpu_window(node)
                       && base != 0
                       && bytes != 0
                    {
                        end = end.max(base.saturating_add(bytes));
                    }
                });
    end
}

fn halt() -> !
{
    loop
    {
        // SAFETY: nothing below can make progress; waiting is the only action.
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> !
{
    puts("BOOT_AARCH64_PANIC");
    if let Some(location) = info.location()
    {
        puts(": ");
        puts(location.file());
        puts(" line=");
        puthex(u64::from(location.line()));
    }
    puts("\n");
    halt()
}
