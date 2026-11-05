#![no_std]
#![no_main]
mac_driver::export_driver!(videocore_framebuffer::DRIVER);
mac_driver::driver_panic_handler!();
