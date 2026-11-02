#![no_std]
#![no_main]
mac_driver::export_driver!(virtio_rng::DRIVER);
mac_driver::driver_panic_handler!();
