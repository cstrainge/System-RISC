#![no_std]
#![no_main]
mac_driver::export_driver!(sifive_test_power::DRIVER);
mac_driver::driver_panic_handler!();
