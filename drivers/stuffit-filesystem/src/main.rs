//! Narrow native module entry and allocator boundary. Filesystem logic, byte
//! codecs and callback state live in the safe library.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
use core::sync::atomic::{AtomicBool, Ordering};

use linked_list_allocator::LockedHeap;
use mac_driver::{Client, Completion, Driver, Error, Request, Result, file_sources::FileSources};
use stuffit_filesystem::service::Service;

const HEAP_BYTES: usize = 4 * 1024 * 1024;
#[repr(align(4096))]
struct HeapStorage([u8; HEAP_BYTES]);
static mut STORAGE: HeapStorage = HeapStorage([0; HEAP_BYTES]);
#[global_allocator]
static HEAP: LockedHeap = LockedHeap::empty();
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
        // SAFETY: The admitted module is pinned and its BSS was zeroed before
        // entry. This aligned private storage is initialized once and borrowed
        // exclusively by the locked allocator for the module's entire lifetime.
        unsafe {
            HEAP.lock().init(core::ptr::addr_of_mut!(STORAGE.0).cast::<u8>(), HEAP_BYTES);
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

// Kernel modules are trusted code. General fault isolation is a separate kernel
// milestone; ordinary allocation/I/O failures use the fallible service paths.
#[alloc_error_handler]
fn allocation_failed(_: core::alloc::Layout) -> !
{
    fatal()
}
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    fatal()
}
fn fatal() -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}
