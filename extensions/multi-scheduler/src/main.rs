//! Platform-owned native module boundary and allocator/panic scaffold.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#[global_allocator]
static HEAP: mac_driver_alloc::ForwardingAllocator = mac_driver_alloc::ForwardingAllocator;
mac_driver::export_driver!(multi_scheduler_extension::EXTENSION);
#[alloc_error_handler]
fn allocation_failed(layout: core::alloc::Layout) -> !
{
    mac_driver::runtime::allocation_failed(layout)
}
mac_driver::driver_panic_handler!();
