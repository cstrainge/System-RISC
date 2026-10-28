//! Negative worker ABI boundary only; ordinary requests retain the public SDK.
use core::{ptr,
           sync::atomic::{AtomicPtr, Ordering}};

use mac_driver::abi;
static HOST: AtomicPtr<abi::Host> = AtomicPtr::new(ptr::null_mut());
#[used]
#[unsafe(link_section = ".system_risc_module")]
static CONTRACT: [u8; include_bytes!(concat!(env!("OUT_DIR"), "/module_contract.bin")).len()] =
    *include_bytes!(concat!(env!("OUT_DIR"), "/module_contract.bin"));
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
unsafe extern "C" fn _start(host: *const abi::Host, output: *mut abi::Instance) -> i32
{
    // SAFETY: the real loader lends this immutable descriptor for module life.
    HOST.store(host.cast_mut(), Ordering::Release);
    unsafe { mac_driver::entry(&super::DEVICE, host, output, dispatch) }
}
unsafe extern "C" fn dispatch(state: u64, call: *mut abi::Call) -> i32
{
    unsafe { mac_driver::dispatch(&super::DEVICE, state, call) }
}
pub fn foreign_status(token: u64) -> i32
{
    // SAFETY: installed only by real entry; descriptor/callback stay pinned.
    let host = unsafe { &*HOST.load(Ordering::Acquire) };
    let mut call = abi::WorkerCall { header:        abi::Header::of::<abi::WorkerCall>(),
                                     operation:     abi::worker::STATUS,
                                     flags:         0,
                                     worker:        token,
                                     specification: ptr::null(),
                                     state:         0,
                                     exit_status:   0,
                                     reserved1:     0 };
    let callback = host.workers.expect("validated mandatory worker callback invariant");
    // Only the token belongs to another real module; all storage is valid.
    unsafe { callback(host.context, &mut call) }
}
