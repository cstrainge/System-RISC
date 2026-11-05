//! A 39-bit identity map, installed once the mailbox work is finished.
//!
//! RAM gets normal write-back attributes and the peripheral aperture gets
//! Device attributes, so the framebuffer and the experiment's buffer are
//! genuinely cached and a missing clean is genuinely observable. The tables are
//! statics in the image; with the MMU off their addresses are already physical.
use core::arch::asm;

use crate::platform::{DECLARED_RAM_BASE, DEVICE_BASE};

const BLOCK: u64 = 0x20_0000;
const TABLE: u64 = 0x3;
/// Valid block, AttrIndx 0 (normal write-back), inner shareable, AF.
///
/// The attribute index is bits[4:2], so AttrIndx 0 means those three bits are
/// clear: 0x400 gives AF, 0x300 gives inner shareable, 0x1 gives a level-2
/// block. `0x705` looks similar but sets bit 2, which is AttrIndx 1 -- the
/// Device-nGnRnE slot in MAIR -- and that maps RAM uncached, so every store
/// goes straight to DRAM and no cache experiment can observe anything.
const NORMAL: u64 = 0x701;
/// Valid 4 KiB page, AttrIndx 2 (normal non-cacheable), inner shareable, AF.
const PAGE_NON_CACHEABLE: u64 = 0x70b;
/// MAIR: index 0 normal write-back, index 1 Device-nGnRnE, index 2 normal
/// non-cacheable. Index 2 is the alias attribute: a read through it reaches
/// memory itself, which is what the scanout and every other device see.
const MAIR: u64 = 0xff | (0x44 << 16);
/// Valid block, AttrIndx 1 (Device-nGnRnE), AF, never executable.
const DEVICE: u64 = 0x405 | (1 << 54) | (1 << 53);
/// 39-bit VA at TTBR0, 4 KiB granule, write-back walks, EPD1 set, 40-bit PA.
const TCR: u64 = 0x2_0099_3519;

#[repr(align(4096))]
struct Table([u64; 512]);

static mut L1: Table = Table([0; 512]);
static mut L2: Table = Table([0; 512]);
static mut L2_ALIAS: Table = Table([0; 512]);
static mut L3_ALIAS: Table = Table([0; 512]);

/// The non-cacheable view of the test page. Chosen above the peripheral
/// aperture, where the level-1 table had no entry, so nothing is displaced.
pub const ALIAS_BASE: usize = 0x4000_0000;

/// Builds the map and turns the MMU and caches on. `alias_page` is a 4 KiB
/// aligned physical address that also gets a non-cacheable second VA, so the
/// cache experiment can read what a device would see. Returns false when the
/// record's RAM base is not zero, because the identity map assumes it.
pub fn enable(alias_page: usize) -> bool
{
    if DECLARED_RAM_BASE != 0 || alias_page & 0xfff != 0
    {
        return false;
    }
    let (l1, l2, l2a, l3a) = unsafe {
        (core::ptr::addr_of_mut!(L1.0) as *mut u64,
         core::ptr::addr_of_mut!(L2.0) as *mut u64,
         core::ptr::addr_of_mut!(L2_ALIAS.0) as *mut u64,
         core::ptr::addr_of_mut!(L3_ALIAS.0) as *mut u64)
    };
    let ram_blocks = (DEVICE_BASE / BLOCK) as usize;
    if ram_blocks >= 512
    {
        return false;
    }
    // SAFETY: the tables are statics owned by this single-threaded boot and the
    // primary core is the only one running.
    unsafe {
        for index in 0..512
        {
            core::ptr::write_volatile(l1.add(index), 0);
            core::ptr::write_volatile(l2.add(index), 0);
            core::ptr::write_volatile(l2a.add(index), 0);
            core::ptr::write_volatile(l3a.add(index), 0);
        }
        core::ptr::write_volatile(l1, l2 as u64 | TABLE);
        for index in 0..ram_blocks
        {
            core::ptr::write_volatile(l2.add(index), index as u64 * BLOCK | NORMAL);
        }
        for index in ram_blocks..512
        {
            core::ptr::write_volatile(l2.add(index), index as u64 * BLOCK | DEVICE);
        }
        // One page of RAM, reachable a second time without caching. Reads
        // through this alias see memory rather than the cache, which is the
        // only way this core can observe what the scanout would show.
        core::ptr::write_volatile(l1.add(1), l2a as u64 | TABLE);
        core::ptr::write_volatile(l2a, l3a as u64 | TABLE);
        core::ptr::write_volatile(l3a, alias_page as u64 | PAGE_NON_CACHEABLE);
        asm!("dsb sy", options(nostack));
        asm!("msr mair_el1, {}", in(reg) MAIR, options(nostack));
        asm!("msr tcr_el1, {}", in(reg) TCR, options(nostack));
        asm!("msr ttbr0_el1, {}", in(reg) l1 as u64, options(nostack));
        asm!("msr ttbr1_el1, {}", in(reg) 0u64, options(nostack));
        asm!("isb", options(nostack));
        asm!("tlbi vmalle1", options(nostack));
        asm!("dsb sy", options(nostack));
        asm!("isb", options(nostack));
        let mut sctlr: u64;
        asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nostack));
        // M (translation), C (data cache), I (instruction cache). WXN stays
        // clear so the image remains executable from normal memory.
        sctlr |= 1 | (1 << 2) | (1 << 12);
        asm!("msr sctlr_el1, {}", in(reg) sctlr, options(nostack));
        asm!("isb", options(nostack));
    }
    true
}

/// The SCTLR bits this module is responsible for, so the report can say what
/// actually got enabled rather than what was requested.
pub fn enabled() -> bool
{
    let sctlr: u64;
    // SAFETY: reading the system control register has no side effects.
    unsafe { asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nostack)) };
    sctlr & (1 | (1 << 2) | (1 << 12)) == 1 | (1 << 2) | (1 << 12)
}
