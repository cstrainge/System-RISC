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
/// Valid block, AttrIndx 1 (Device-nGnRnE), AF, never executable.
const DEVICE: u64 = 0x405 | (1 << 54) | (1 << 53);
/// 39-bit VA at TTBR0, 4 KiB granule, write-back walks, EPD1 set, 40-bit PA.
const TCR: u64 = 0x2_0099_3519;
/// MAIR[0] normal write-back, MAIR[1] Device-nGnRnE.
const MAIR: u64 = 0xff;

#[repr(align(4096))]
struct Table([u64; 512]);

static mut L1: Table = Table([0; 512]);
static mut L2: Table = Table([0; 512]);

/// Builds the map and turns the MMU and caches on. Returns false when the
/// record's RAM base is not zero, because the identity map below assumes it.
pub fn enable() -> bool
{
    if DECLARED_RAM_BASE != 0
    {
        return false;
    }
    // SAFETY: the tables are statics owned by this single-threaded boot; taking
    // their addresses here does not create a reference and the primary core is
    // the only one running.
    let (l1, l2) = unsafe {
        (core::ptr::addr_of_mut!(L1.0) as *mut u64, core::ptr::addr_of_mut!(L2.0) as *mut u64)
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
