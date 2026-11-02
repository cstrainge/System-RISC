//! Graphical APPL/FNDR profile: intentional null standard streams. GUI stdin is
//! always at EOF; stdout/stderr discard bytes and flush successfully. This is
//! not a Terminal/session endpoint or CLAP/pipe implementation. Panic reports
//! use the independent System crash service.
use crate::io::{self, BorrowedCursor, Read, Write};

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
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize>
    {
        Ok(0)
    }

    fn read_buf(&mut self, _cursor: BorrowedCursor<'_, u8>) -> io::Result<()>
    {
        Ok(())
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
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()>
    {
        Ok(())
    }
}
// An empty input buffer reflects this profile's deliberate permanent EOF and
// avoids allocating a read buffer for a GUI application with no input endpoint.
pub const STDIN_BUF_SIZE: usize = 0;
pub fn is_ebadf(_error: &io::Error) -> bool
{
    false
}
pub fn panic_output() -> Option<Vec<u8>>
{
    None
}
