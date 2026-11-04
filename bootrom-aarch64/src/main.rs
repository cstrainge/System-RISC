//! AArch64 EL1 admission entry: initialise the PL011 once, report a named
//! marker, and halt. The vector table reports an exception instead of spinning
//! silently. The MMU is deliberately off for this first admission; the checked
//! virtual layout is the next layer.
#![no_std]
#![no_main]

use core::panic::PanicInfo;

core::arch::global_asm!(include_str!(concat!(env!("OUT_DIR"), "/platform-hardware.inc")),
                        include_str!("entry.S"));

#[panic_handler]
fn panic(_: &PanicInfo) -> !
{
    loop
    {}
}
