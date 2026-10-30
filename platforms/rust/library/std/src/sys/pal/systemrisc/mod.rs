//! System RISC uses its native services, not libc or a host service adapter.
#![deny(unsafe_op_in_unsafe_fn)]

use crate::io;

pub(crate) mod runtime;

pub fn unsupported<T>() -> io::Result<T>
{
    Err(unsupported_err())
}

pub fn unsupported_err() -> io::Error
{
    io::const_error!(io::ErrorKind::Unsupported, "System RISC service is not implemented")
}

// Services/current-thread context must already exist before std enters init.
pub unsafe fn init(_argc: isize, _argv: *const *const u8, _sigpipe: u8)
{
    runtime::client();
}

pub unsafe fn cleanup()
{
    // The platform exit adapter owns TLS teardown, after std's cleanup turn.
}

pub fn abort_internal() -> !
{
    unsafe { runtime::__system_risc_abort_v1() }
}
