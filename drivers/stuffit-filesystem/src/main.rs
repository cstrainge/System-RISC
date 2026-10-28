//! Narrow native module entry and allocator boundary. Filesystem logic, byte
//! codecs and callback state live in the safe library.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
use core::sync::atomic::{AtomicBool, Ordering};

use mac_driver::{Client, Completion, Driver, Error, Request, Result, file_sources::FileSources};
use mac_driver_alloc::ForwardingAllocator;
use stuffit_filesystem::service::Service;

#[global_allocator]
static HEAP: ForwardingAllocator = ForwardingAllocator;
static INITIALIZED: AtomicBool = AtomicBool::new(false);
static SERVICE: Service<FileSources> = Service::new();
struct FilesystemDriver;
impl Driver for FilesystemDriver
{
    fn initialize(&self, host: Client) -> Result<()>
    {
        if INITIALIZED.swap(true, Ordering::AcqRel)
        {
            return Err(Error(mac_abi::SERVICE_BUSY));
        }
        SERVICE.initialize(FileSources::new(host)?)
    }

    fn request(&self, request: Request<'_>, output: &mut [u8]) -> Result<Completion>
    {
        SERVICE.request(request, output)
    }

    fn quiesce(&self) -> Result<()>
    {
        SERVICE.quiesce()
    }
}
mac_driver::export_driver!(FilesystemDriver);

// Fallible service paths preserve ordinary allocation failures. An infallible
// allocation or panic uses the platform diagnostic/fault contract.
#[alloc_error_handler]
fn allocation_failed(layout: core::alloc::Layout) -> !
{
    mac_driver::runtime::allocation_failed(layout)
}
mac_driver::driver_panic_handler!();
