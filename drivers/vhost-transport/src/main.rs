#![no_std]
#![no_main]
mac_driver::export_driver!(vhost_transport::DRIVER);
mac_driver::driver_panic_handler!();
