//! Existing negative native ABI boundary. Normal requests use the public SDK;
//! scheduler-malformed alone mutates the borrowed envelope after SDK dispatch.
#[cfg(feature = "workers")]
use core::{ptr,
           sync::atomic::{AtomicPtr, Ordering}};

use mac_driver::abi;
#[cfg(feature = "workers")]
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
    #[cfg(feature = "workers")]
    HOST.store(host.cast_mut(), Ordering::Release);
    unsafe { mac_driver::entry(&super::DEVICE, host, output, dispatch) }
}
unsafe extern "C" fn dispatch(state: u64, call: *mut abi::Call) -> i32
{
    let status = unsafe { mac_driver::dispatch(&super::DEVICE, state, call) };
    #[cfg(feature = "scheduler-malformed")]
    {
        // SAFETY: the real resident supplies exclusive live Call storage for
        // this invocation. SDK dispatch has returned, ending its borrow. No
        // input/output pointer or allocation is fabricated by this negative case.
        if status == mac_abi::OK
        {
            let call = unsafe { &mut *call };
            if call.phase == mac_scheduler_interface::control::POLICY_PHASE
               && call.class == mac_scheduler_interface::POLICY_CLASS.0
            {
                call.ticket = call.ticket.wrapping_add(1);
                MALFORMED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    status
}
#[cfg(feature = "scheduler-malformed")]
static MALFORMED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "scheduler-malformed")]
pub fn malformed_count() -> u64
{
    MALFORMED.load(core::sync::atomic::Ordering::Relaxed)
}
#[cfg(feature = "workers")]
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
                                     microseconds:  0 };
    let callback = host.workers.expect("validated mandatory worker callback invariant");
    // Only the token belongs to another real module; all storage is valid.
    unsafe { callback(host.context, &mut call) }
}
