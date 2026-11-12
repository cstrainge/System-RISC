#![no_std]
#![no_main]
mac_driver::export_driver!(dwc2_usb::DRIVER);
mac_driver::driver_panic_handler!();
