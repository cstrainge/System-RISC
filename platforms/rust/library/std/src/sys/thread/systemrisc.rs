use mac_abi::native_threads as wire;
use mac_traps::native_threads::{CreateTransfer, RequestFailure};

use crate::{ffi::CStr, io, mem::ManuallyDrop, num::NonZero, sys::pal::runtime, thread::ThreadInit,
            time::Duration};

// Matches the current checked native application stack policy. Explicit sizes
// remain subject to the resident service's bounds and resource admission.
pub const DEFAULT_MIN_STACK_SIZE: usize = 4 * 1024 * 1024;

pub struct Thread
{
    right: u64,
    id:    u64
}

impl Thread
{
    pub unsafe fn new(stack: usize, init: Box<ThreadInit>) -> io::Result<Thread>
    {
        let data = Box::into_raw(init);
        let mut call = wire::Call { operation: wire::CREATE,
                                    entry: start as usize,
                                    data: data as usize,
                                    stack_bytes: stack as u64,
                                    ..Default::default() };
        // A child may already run and free data before CREATE returns. Only a
        // typed resident refusal establishes that this caller still owns it.
        if let Err(cause) = unsafe { runtime::client().native_thread(&mut call) }
        {
            if cause.create_transfer() == Some(CreateTransfer::Retained)
            {
                unsafe {
                    drop(Box::from_raw(data));
                }
                return Err(io_error(cause));
            }
            // Do not reconstruct/free possibly transferred data on a malformed
            // reply. System's Abort walk owns quiescence and original resources.
            refused("CREATE ownership indeterminate", cause);
        }
        Ok(Thread { right: call.right,
                    id:    call.thread })
    }

    pub fn join(self)
    {
        let thread = ManuallyDrop::new(self);
        let mut call = wire::Call { operation: wire::JOIN,
                                    right: thread.right,
                                    ..Default::default() };
        if let Err(cause) = unsafe { runtime::client().native_thread(&mut call) }
        {
            refused("JOIN", cause);
        }
        if call.thread != thread.id
           || call.state != wire::COMPLETE
           || call.outcome != wire::NORMAL_OUTCOME
        {
            runtime::failure(mac_abi::BAD_ABI, b"JOIN did not return the completed original child");
        }
    }
}

impl Drop for Thread
{
    fn drop(&mut self)
    {
        let mut call = wire::Call { operation: wire::DETACH,
                                    right: self.right,
                                    ..Default::default() };
        if let Err(cause) = unsafe { runtime::client().native_thread(&mut call) }
        {
            // A failed release is not disposal. Abort retains the canonical
            // right for System's owner walk rather than discarding its failure.
            refused("DETACH", cause);
        }
    }
}

unsafe extern "C" fn start(data: *mut u8)
{
    let init = unsafe { Box::from_raw(data.cast::<ThreadInit>()) };
    let run = init.init();
    run();
    // The admitted user thread-return entry tears TLS down exactly once before
    // issuing the canonical native completion request.
}

pub fn current_os_id() -> Option<u64>
{
    Some(unsafe { runtime::__system_risc_thread_id_v1() })
}
pub fn set_name(name: &CStr)
{
    let bytes = name.to_bytes();
    let mut call = wire::Call { operation: wire::NAME,
                                name: bytes.as_ptr(),
                                name_length: match u32::try_from(bytes.len())
                                {
                                    Ok(length) => length,
                                    Err(_) => return
                                },
                                ..Default::default() };
    if let Err(cause) = unsafe { runtime::client().native_thread(&mut call) }
    {
        // The OS diagnostic name has stricter length/character limits. Rust
        // retains its full name; this optional OS label is best effort.
        if cause.cause() != mac_traps::Error::BadArgument
        {
            refused("NAME", cause);
        }
    }
}
pub fn yield_now()
{
    let mut call = wire::Call { operation: wire::YIELD,
                                ..Default::default() };
    if let Err(cause) = unsafe { runtime::client().native_thread(&mut call) }
    {
        refused("YIELD", cause);
    }
}
pub fn sleep(duration: Duration)
{
    let mut call = wire::Call { operation: wire::SLEEP,
                                seconds: duration.as_secs(),
                                nanoseconds: duration.subsec_nanos(),
                                ..Default::default() };
    if let Err(cause) = unsafe { runtime::client().native_thread(&mut call) }
    {
        refused("SLEEP", cause);
    }
}
pub fn available_parallelism() -> io::Result<NonZero<usize>>
{
    let mut call = wire::Call { operation: wire::PARALLELISM,
                                ..Default::default() };
    unsafe { runtime::client().native_thread(&mut call) }.map_err(io_error)?;
    NonZero::new(call.parallelism as usize).ok_or_else(|| {
                                               io::Error::from_raw_os_error(mac_abi::BAD_ABI)
                                           })
}

fn io_error(cause: RequestFailure) -> io::Error
{
    let kind = io::Error::from_raw_os_error(cause.cause().status()).kind();
    io::Error::new(kind, cause)
}

fn refused(operation: &str, cause: RequestFailure) -> !
{
    crate::sys::pal::failure::report(format_args!("Native std thread {operation} refused: {cause}"))
}
