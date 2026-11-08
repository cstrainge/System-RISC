#![no_std]
#![no_main]
mac_driver::export_driver!(psci_power::DRIVER);
mac_driver::driver_panic_handler!();
