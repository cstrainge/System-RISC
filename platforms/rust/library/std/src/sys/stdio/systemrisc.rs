//! No process streams have been admitted yet. Refuse I/O explicitly instead of
//! using the generic sink/EOF fallback. System panic reports use their own path.
use crate::io::{self, Read, Write};

pub struct Stdin;
pub struct Stdout;
pub type Stderr = Stdout;

impl Stdin
{
    pub const fn new() -> Self
    {
        Self
    }
}

impl Read for Stdin
{
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize>
    {
        if buffer.is_empty() { Ok(0) } else { Err(unavailable()) }
    }
}

impl Stdout
{
    pub const fn new() -> Self
    {
        Self
    }
}

impl Write for Stdout
{
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize>
    {
        if buffer.is_empty() { Ok(0) } else { Err(unavailable()) }
    }

    fn flush(&mut self) -> io::Result<()>
    {
        Err(unavailable())
    }
}

fn unavailable() -> io::Error
{
    io::const_error!(io::ErrorKind::Unsupported, "System RISC process stream is unavailable")
}

// A zero-sized BufReader would fabricate EOF without calling the service.
pub const STDIN_BUF_SIZE: usize = 1024;

pub fn is_ebadf(_error: &io::Error) -> bool
{
    // stdio must not turn this refusal into a successful null-device operation.
    false
}

pub fn panic_output() -> Option<Vec<u8>>
{
    None
}
