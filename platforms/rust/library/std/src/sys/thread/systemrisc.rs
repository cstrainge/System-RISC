use crate::{ffi::CStr, io, mem::ManuallyDrop, num::NonZero, sys::pal::runtime, thread::ThreadInit,
            time::Duration};

// Matches the current checked native application stack policy. Explicit sizes
// remain subject to the resident service's bounds and resource admission.
pub const DEFAULT_MIN_STACK_SIZE: usize = 4 * 1024 * 1024;

pub struct Thread
{
    handle: u64
}

impl Thread
{
    pub unsafe fn new(stack: usize, init: Box<ThreadInit>) -> io::Result<Thread>
    {
        let data = Box::into_raw(init);
        let mut handle = 0;
        let status = unsafe {
            runtime::__system_risc_thread_create_v1(stack, start, data.cast(), &mut handle)
        };
        if status != mac_abi::OK
        {
            // The creation contract did not transfer or execute data on refusal.
            unsafe {
                drop(Box::from_raw(data));
            }
            return Err(io::Error::from_raw_os_error(status));
        }
        if handle == 0
        {
            runtime::failure(mac_abi::BAD_ABI, b"thread creation returned an invalid join right");
        }
        Ok(Thread { handle })
    }

    pub fn join(self)
    {
        let thread = ManuallyDrop::new(self);
        unsafe {
            runtime::__system_risc_thread_join_v1(thread.handle);
        }
    }
}

impl Drop for Thread
{
    fn drop(&mut self)
    {
        unsafe {
            runtime::__system_risc_thread_detach_v1(self.handle);
        }
    }
}

unsafe extern "C" fn start(data: *mut u8)
{
    let init = unsafe { Box::from_raw(data.cast::<ThreadInit>()) };
    let run = init.init();
    run();
    // This must run on the actual child before its native completion receipt.
    unsafe {
        runtime::__system_risc_tls_teardown_v1();
    }
}

pub fn current_os_id() -> Option<u64>
{
    Some(unsafe { runtime::__system_risc_thread_id_v1() })
}
pub fn set_name(name: &CStr)
{
    let bytes = name.to_bytes();
    unsafe {
        runtime::__system_risc_thread_name_v1(bytes.as_ptr(), bytes.len());
    }
}
pub fn yield_now()
{
    unsafe {
        runtime::__system_risc_thread_yield_v1();
    }
}
pub fn sleep(duration: Duration)
{
    unsafe {
        runtime::__system_risc_thread_sleep_v1(duration.as_secs(), duration.subsec_nanos());
    }
}
pub fn available_parallelism() -> io::Result<NonZero<usize>>
{
    let mut count = 0;
    let status = unsafe { runtime::__system_risc_parallelism_v1(&mut count) };
    if status != mac_abi::OK
    {
        return Err(io::Error::from_raw_os_error(status));
    }
    NonZero::new(count).ok_or_else(|| io::Error::from_raw_os_error(mac_abi::BAD_ABI))
}
