//! Native stdio uses the child attachments installed by the process launcher.
use mac_abi::pipes as wire;

use crate::{io::{self, Read, Write},
            sys::pal::runtime};

pub struct Stdin;
pub struct Stdout;
pub struct Stderr;

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
        transfer(0, wire::READ, buffer.as_mut_ptr(), buffer.len())
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
        transfer(1, wire::WRITE, buffer.as_ptr().cast_mut(), buffer.len())
    }

    fn flush(&mut self) -> io::Result<()>
    {
        Ok(())
    }
}

impl Stderr
{
    pub const fn new() -> Self
    {
        Self
    }
}

impl Write for Stderr
{
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize>
    {
        transfer(2, wire::WRITE, buffer.as_ptr().cast_mut(), buffer.len())
    }

    fn flush(&mut self) -> io::Result<()>
    {
        Ok(())
    }
}

fn transfer(index: usize, operation: u32, buffer: *mut u8, length: usize) -> io::Result<usize>
{
    let handle = runtime::stream(index);
    if length == 0 || handle == 0
    {
        return Ok(if operation == wire::READ { 0 } else { length });
    }
    let mut call = wire::Call { operation,
                                handle,
                                buffer,
                                length,
                                ..Default::default() };
    runtime::client().pipes(&mut call)
                     .map_err(|error| io::Error::from_raw_os_error(error.status()))?;
    Ok(call.transferred)
}

pub const STDIN_BUF_SIZE: usize = crate::sys::io::DEFAULT_BUF_SIZE;
pub fn is_ebadf(_error: &io::Error) -> bool
{
    false
}

pub fn panic_output() -> Option<Vec<u8>>
{
    None
}
