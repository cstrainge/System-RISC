
use std::{
    fs::File,
    io::{self, IsTerminal, Read, Write},
    process::Stdio,
    sync::Mutex,
};

#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};

// Crossterm's cursor-position query is hardwired to descriptor 1. During a native read
// descriptor 1 is the terminal control endpoint; Shelly's data writers/spawns retain the
// original destination here. The mode lease serializes routing and terminal restoration.
static ORIGINAL: Mutex<Option<File>> = Mutex::new(None);

pub(crate) struct OutputGuard;
impl OutputGuard
{
    pub(crate) fn enter() -> io::Result<Self>
    {
        io::stdout().flush()?;
        #[cfg(unix)]
        unsafe {
            let mut original = ORIGINAL.lock().unwrap();
            let descriptor = libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 3);
            if descriptor < 0
            {
                return Err(io::Error::last_os_error());
            }
            let file = File::from_raw_fd(descriptor);
            if libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) < 0
            {
                return Err(io::Error::last_os_error());
            }
            *original = Some(file);
        }
        Ok(Self)
    }
    pub(crate) fn restore(&self) -> io::Result<()>
    {
        let mut original = ORIGINAL.lock().unwrap();
        #[cfg(unix)]
        if let Some(file) = original.as_ref()
        {
            io::stdout().flush()?;
            if unsafe { libc::dup2(file.as_raw_fd(), libc::STDOUT_FILENO) } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        *original = None;
        Ok(())
    }
}
impl Drop for OutputGuard
{
    fn drop(&mut self)
    {
        let _ = self.restore();
    }
}

pub(crate) fn data_stdio() -> io::Result<Option<Stdio>>
{
    ORIGINAL
        .lock()
        .unwrap()
        .as_ref()
        .map(|file| file.try_clone().map(Stdio::from))
        .transpose()
}

pub(crate) fn write_data(bytes: &[u8]) -> io::Result<()>
{
    let original = ORIGINAL.lock().unwrap();
    if let Some(file) = original.as_ref()
    {
        if file.is_terminal()
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Terminal is busy editing input",
            ));
        }
        (&*file).write_all(bytes)
    }
    else if super::editing() && io::stdout().is_terminal()
    {
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "Terminal is busy editing input",
        ))
    }
    else
    {
        let mut output = io::stdout().lock();
        output.write_all(bytes)?;
        // Forward partial pipeline chunks now, before a child writes to the same descriptor.
        output.flush()
    }
}
pub(crate) fn copy_data(input: &mut impl Read) -> io::Result<u64>
{
    let mut total = 0;
    let mut buffer = [0u8; 65536];
    loop
    {
        let length = input.read(&mut buffer)?;
        if length == 0
        {
            return Ok(total);
        }
        write_data(&buffer[..length])?;
        total += length as u64;
    }
}
