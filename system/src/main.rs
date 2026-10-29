//! Native System executable entry; the resident implementation is in its library.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
use mac_abi::SystemServices;
/// # Safety
/// Firmware supplies the exact payload/length and explicit BOOT or HART role.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(payload: *const (), bytes: usize, role: u64)
                                -> *const SystemServices
{
    unsafe { mac_system::kernel::start(payload, bytes, role) }
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
