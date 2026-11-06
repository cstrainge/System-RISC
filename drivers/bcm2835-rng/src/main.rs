#![no_std]
#![no_main]
mac_driver::export_driver!(bcm2835_rng::DRIVER);
mac_driver::driver_panic_handler!();
