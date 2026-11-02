//! Existing native fixture transport: one audited LOG/clock/negative-ABI edge.
//! Worker bodies use safe SDK interfaces; no synthetic thread/account is issued.
use core::{fmt::{Arguments, Write},
           ptr,
           sync::atomic::{AtomicPtr, Ordering}};

use mac_driver::abi;
#[cfg(not(feature = "pressure"))]
use mac_driver::{Error, Result};
static HOST: AtomicPtr<abi::Host> = AtomicPtr::new(ptr::null_mut());
pub unsafe fn install(host: *const abi::Host)
{
    // The actual loader owns this immutable descriptor for this module's life.
    HOST.store(host.cast_mut(), Ordering::Release);
}
pub fn now() -> u64
{
    let ticks;
    // SAFETY: side-effect-free native QEMU monotonic 10 MHz counter.
    unsafe {
        core::arch::asm!("rdtime {}", out(reg) ticks, options(nomem, nostack));
    }
    ticks
}
/// Malformed descriptors still use valid borrowed storage and inert ZST state.
/// No captured allocation or callback may transfer on these ordinary refusals.
#[cfg(all(feature = "workers", not(feature = "pressure")))]
pub fn worker_descriptor_refusals()
{
    use mac_abi::{BAD_ABI, BAD_ARGUMENT};

    unsafe extern "C" fn unexpected_entry(_: *mut core::ffi::c_void) -> i32
    {
        panic!("refused worker descriptor entered its body")
    }
    unsafe extern "C" fn unexpected_destroy(_: *mut core::ffi::c_void)
    {
        panic!("refused worker descriptor transferred capture ownership")
    }
    // SAFETY: real module entry installed this immutable resident descriptor.
    let host = unsafe { &*HOST.load(Ordering::Acquire) };
    let callback = host.workers.expect("validated mandatory worker callback invariant");
    let name = b"descriptor refusal";
    for (label, size, affinity, reserved, expected) in
        [("old descriptor size", 64, abi::worker::AFFINITY_AUTOMATIC, 0, BAD_ABI),
         ("unknown affinity", 72, u32::MAX, 0, BAD_ARGUMENT),
         ("nonzero reserved", 72, abi::worker::AFFINITY_AUTOMATIC, 1, BAD_ARGUMENT)]
    {
        let specification = abi::WorkerSpecification {
            header: abi::Header { size, ..abi::Header::of::<abi::WorkerSpecification>() },
            entry: Some(unexpected_entry),
            destroy: Some(unexpected_destroy),
            state: ptr::NonNull::<u8>::dangling().as_ptr().cast(),
            state_size: 0,
            state_alignment: 1,
            name: name.as_ptr(),
            name_length: name.len(),
            affinity,
            reserved
        };
        let mut call = abi::WorkerCall { header:        abi::Header::of::<abi::WorkerCall>(),
                                         operation:     abi::worker::CREATE,
                                         flags:         0,
                                         worker:        0,
                                         specification: &specification,
                                         state:         0,
                                         exit_status:   0,
                                         microseconds:  0 };
        // SAFETY: both wire records and the name remain borrowed through return.
        // The non-null ZST capture has no allocation; neither thunk may run.
        let status = unsafe { callback(host.context, &mut call) };
        assert_eq!(status, expected, "worker descriptor refusal lost its cause: {label}");
        assert_eq!(call.worker, 0, "refused descriptor published a worker: {label}");
    }
    log(format_args!("MODULE_WORKER_DESCRIPTOR_REFUSALS_PASS size=72 old=bad-abi \
                      affinity=bad-argument reserved=bad-argument capture=retained\n"));
}
/// A direct worker has no resident definition scope. Refusal must precede
/// request borrowing; these null/zero-size envelopes are deliberately invalid.
#[cfg(all(feature = "workers", not(feature = "pressure")))]
pub fn worker_toolbox_refusals()
{
    for number in [mac_abi::traps::THEME,
                   mac_abi::traps::FONT,
                   mac_abi::traps::SURFACE,
                   mac_abi::traps::WAIT_EVENT,
                   mac_abi::traps::APPLICATION_HEAP]
    {
        let mut result = 0usize;
        // SAFETY: the ordinary worker gate must reject before borrowing any
        // request. No pointee is supplied or authorized by this negative probe.
        unsafe {
            core::arch::asm!("ecall", in("a7") number,
                inlateout("a0") result, in("a1") 0usize, clobber_abi("C"));
        }
        assert_eq!(result as isize as i32,
                   mac_abi::WRONG_THREAD,
                   "direct worker Toolbox admission changed for trap {number:#x}");
    }
    log(format_args!("MODULE_WORKER_TOOLBOX_REFUSALS_PASS requests=5 status=wrong-thread \
                      before-borrow=yes\n"));
}
#[cfg(not(feature = "pressure"))]
pub fn current_token() -> Result<u64>
{
    // SAFETY: real entry installed the immutable resident descriptor.
    let host = unsafe { &*HOST.load(Ordering::Acquire) };
    let callback = host.workers.ok_or(Error(mac_abi::NOT_SUPPORTED))?;
    let mut call = abi::WorkerCall { header:        abi::Header::of::<abi::WorkerCall>(),
                                     operation:     abi::worker::CURRENT,
                                     flags:         0,
                                     worker:        0,
                                     specification: ptr::null(),
                                     state:         0,
                                     exit_status:   0,
                                     microseconds:  0 };
    let status = unsafe { callback(host.context, &mut call) };
    if status != 0
    {
        return Err(Error(status));
    }
    Ok(call.worker)
}
pub fn log(arguments: Arguments<'_>)
{
    struct Text
    {
        bytes:  [u8; 512],
        length: usize
    }
    impl Write for Text
    {
        fn write_str(&mut self, value: &str) -> core::fmt::Result
        {
            if value.len() > self.bytes.len() - self.length
            {
                return Err(core::fmt::Error);
            }
            self.bytes[self.length..self.length + value.len()].copy_from_slice(value.as_bytes());
            self.length += value.len();
            Ok(())
        }
    }
    let mut text = Text { bytes:  [0; 512],
                          length: 0 };
    assert!(text.write_fmt(arguments).is_ok(), "bounded worker outcome diagnostic overflow");
    let mut call = mac_abi::traps::LogCall { bytes:  text.bytes.as_ptr(),
                                             length: text.length };
    let mut result = ptr::from_mut(&mut call).addr();
    // SAFETY: one synchronous native LOG borrows exact live stack/text storage.
    unsafe {
        core::arch::asm!("ecall", in("a7") mac_abi::traps::LOG,
        inlateout("a0") result, in("a1") core::mem::size_of_val(&call), clobber_abi("C"));
    }
    let _ = result;
}
/// Observe a token issued by CURRENT in this same real module; no token is
/// synthesized and the resident still enforces ownership/generation.
#[cfg(not(feature = "pressure"))]
pub fn state(token: u64) -> Result<u32>
{
    let host = unsafe { &*HOST.load(Ordering::Acquire) };
    let callback = host.workers.ok_or(Error(mac_abi::NOT_SUPPORTED))?;
    let mut call = abi::WorkerCall { header:        abi::Header::of::<abi::WorkerCall>(),
                                     operation:     abi::worker::STATUS,
                                     flags:         0,
                                     worker:        token,
                                     specification: ptr::null(),
                                     state:         0,
                                     exit_status:   0,
                                     microseconds:  0 };
    let status = unsafe { callback(host.context, &mut call) };
    if status != 0
    {
        return Err(Error(status));
    }
    Ok(call.state)
}
