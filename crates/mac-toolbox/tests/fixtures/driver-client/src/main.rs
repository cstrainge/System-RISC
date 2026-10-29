#![no_std]
#![no_main]
#![cfg_attr(feature = "workers", feature(alloc_error_handler))]
#[cfg(feature = "workers")]
mod machine;
#[cfg(all(feature = "workers", not(feature = "pressure")))]
mod workers;
#[cfg(feature = "pressure")]
mod pressure;
#[cfg(feature = "pressure")]
extern crate alloc;
#[cfg(feature = "workers")]
#[global_allocator]
static ALLOCATOR: mac_driver_alloc::ForwardingAllocator = mac_driver_alloc::ForwardingAllocator;
#[cfg(feature = "workers")]
#[alloc_error_handler]
fn allocation_failed(layout: core::alloc::Layout) -> !
{
    mac_driver::runtime::allocation_failed(layout)
}
#[cfg(not(feature = "pressure"))]
use mac_driver::{ClassId, Client, Completion, Driver, Error, Phase, Request, Result};
struct Device;
static DEVICE: Device = Device;
// Intentionally exercise the raw resident boundary before entering the safe
// SDK. A rejected output pointer must not write or poison later resolution.
#[used]
#[unsafe(link_section = ".system_risc_module")]
static MODULE_CONTRACT: [u8;
                           include_bytes!(concat!(env!("OUT_DIR"), "/module_contract.bin")).len()] =
    *include_bytes!(concat!(env!("OUT_DIR"), "/module_contract.bin"));
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
unsafe extern "C" fn _start(host: *const mac_driver::abi::Host,
                            output: *mut mac_driver::abi::Instance)
                            -> i32
{
    #[cfg(feature = "pressure")]
    {
        unsafe {
            machine::install(host);
        }
        return unsafe { mac_driver::entry(&DEVICE, host, output, module_dispatch) };
    }
    #[cfg(not(feature = "pressure"))]
    {
        // Loader-owned Host is valid and pinned for this invocation. None of the
        // deliberately bad result pointers is dereferenced by this fixture.
        let descriptor = unsafe { *host };
        #[cfg(feature = "workers")]
        unsafe {
            machine::install(host);
        }
        let Some(resolve) = descriptor.resolve
        else
        {
            return -4;
        };
        #[repr(align(8))]
        struct Guard([u8; 16]);
        let mut guard = Guard([0xa5; 16]);
        let unaligned = guard.0.as_mut_ptr().wrapping_add(4).cast::<u64>();
        for pointer in [core::ptr::null_mut(), unaligned]
        {
            if unsafe { resolve(descriptor.context, u32::from_le_bytes(*b"TEST"), 1, pointer) }
               != -2
               || guard.0 != [0xa5; 16]
            {
                return -1;
            }
        }
        unsafe { mac_driver::entry(&DEVICE, host, output, module_dispatch) }
    }
}
unsafe extern "C" fn module_dispatch(state: u64, call: *mut mac_driver::abi::Call) -> i32
{
    // Normal valid broker calls use the same audited SDK decoder as drivers.
    unsafe { mac_driver::dispatch(&DEVICE, state, call) }
}
mac_driver::driver_panic_handler!();
#[cfg(not(feature = "pressure"))]
impl Driver for Device
{
    fn initialize(&self, host: Client) -> Result<()>
    {
        if host.module() <= u64::from(u32::MAX)
        {
            return Err(Error(-1));
        }
        let provider = host.resolve(ClassId(*b"TEST"), 1)?;
        let mut output = [0; 32];
        let mut ticket = 0;
        let result = provider.invoke(&mut ticket, Phase::Start, 7, b"class-neutral", &mut output)?;
        if result != Completion::Complete(13) || &output[..13] != b"lartuen-ssalc" || ticket != 0
        {
            return Err(Error(-1));
        }
        #[cfg(feature = "workers")]
        return workers::initialize(host, provider);
        #[cfg(not(feature = "workers"))]
        Ok(())
    }

    fn request(&self, _: Request<'_>, _: &mut [u8]) -> Result<Completion>
    {
        Err(Error(-4))
    }
}
