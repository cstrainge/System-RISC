#![no_std]
#![no_main]
mac_driver::export_driver!(qemu_power_device::DRIVER);
mac_driver::driver_panic_handler!();
