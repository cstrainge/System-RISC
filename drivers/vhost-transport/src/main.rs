#![no_std]
#![no_main]
mac_driver::export_driver!(vhost_transport::DRIVER);
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}
