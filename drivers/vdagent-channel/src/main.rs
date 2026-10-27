#![no_std]
#![no_main]
mac_driver::export_driver!(vdagent_channel::DRIVER);
mac_driver::driver_panic_handler!();
