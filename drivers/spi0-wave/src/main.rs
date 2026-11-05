#![no_std]
#![no_main]
mac_driver::export_driver!(spi0_wave::DRIVER);
mac_driver::driver_panic_handler!();
