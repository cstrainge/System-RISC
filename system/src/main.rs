//! Native System executable entry; the resident implementation is in its library.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
use mac_abi::{BootInfo, SystemServices};
/// # Safety
/// Firmware supplies a valid BootInfo and keeps its callbacks alive.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(info: *const BootInfo) -> *const SystemServices
{
    unsafe { mac_system::kernel::start(info) }
}
#[alloc_error_handler]
fn allocation_failed(layout: core::alloc::Layout) -> !
{
    mac_system::kernel::allocation_failed(layout)
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> !
{
    mac_system::kernel::panic(info)
}
