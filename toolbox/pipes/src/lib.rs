//! Byte-transport pipes. `read` returns zero only at end of stream; a write with
//! no readers fails with `BrokenPipe`; non-blocking mode reports `WouldBlock`
//! instead of parking. A 512-byte write is atomic against other writers, and
//! that bound is never a record size.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::pipes as wire;
use mac_traps::{Client, Error, Result};

#[derive(Clone, Copy)]
pub struct Pipes<'a>
{
    client: Client<'a>
}

pub struct PipeReader<'a>
{
    client: Client<'a>,
    handle: u64,
    active: bool
}
pub struct PipeWriter<'a>
{
    client: Client<'a>,
    handle: u64,
    active: bool
}

impl<'a> Pipes<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    /// Create an anonymous pipe. Both endpoints start in blocking mode.
    pub fn create(self, capacity: usize) -> Result<(PipeReader<'a>, PipeWriter<'a>)>
    {
        self.pair(wire::CREATE, "", capacity, 0)
    }

    /// Create a pipe and register `name` for `connect`. The name is removed when
    /// the last handle closes, including when the owning process exits.
    /// `writers` bounds how many applications may hold a writer obtained from
    /// `connect` at once; zero is unlimited. The creator's own writer is not a
    /// connection, so `1` gives one connecting owner at a time and a second
    /// `connect` is refused with `AccessDenied`.
    pub fn create_named(self, name: &str, capacity: usize, writers: u32)
                        -> Result<(PipeReader<'a>, PipeWriter<'a>)>
    {
        self.pair(wire::CREATE_NAMED, name, capacity, u64::from(writers))
    }

    /// Obtain the writer end of a registered name. An unknown name is `NotFound`;
    /// a name whose last handle closed is gone rather than a dead endpoint. A
    /// name already holding its declared number of connections is
    /// `AccessDenied`, so the existing owner is never joined.
    pub fn connect(self, name: &str) -> Result<PipeWriter<'a>>
    {
        let mut call = wire::Call { operation: wire::CONNECT,
                                    nonblocking: 0,
                                    ..Default::default() };
        name_into(&mut call, name)?;
        self.client.pipes(&mut call)?;
        Ok(PipeWriter { client: self.client,
                        handle: call.writer,
                        active: true })
    }

    /// Launch a command-line executable in an owned Terminal window session.
    /// On success the child owns the ends. On refusal the caller still owns
    /// them and may retry or close them.
    pub fn launch_cli(self, session: mac_abi::WindowId, volume: mac_traps::VolumeId,
                      cnid: mac_traps::Cnid, stdin: &mut PipeReader<'a>,
                      stdout: &mut PipeWriter<'a>, stderr: &mut PipeWriter<'a>)
                      -> Result<mac_traps::ProcessId>
    {
        self.launch_cli_with_path(session, volume, cnid, "", stdin, stdout, stderr)
    }

    /// Launch in an owned session with the Terminal's executable search path.
    pub fn launch_cli_with_path(self, session: mac_abi::WindowId, volume: mac_traps::VolumeId,
                                cnid: mac_traps::Cnid, path: &str, stdin: &mut PipeReader<'a>,
                                stdout: &mut PipeWriter<'a>, stderr: &mut PipeWriter<'a>)
                                -> Result<mac_traps::ProcessId>
    {
        use mac_abi::processes::{LAUNCH, ProcessCall};
        if !stdin.active || !stdout.active || !stderr.active
        {
            return Err(Error::BadArgument);
        }
        let mut call = ProcessCall { operation: LAUNCH,
                                     id: session.0,
                                     volume: volume.0,
                                     cnid: cnid.0,
                                     streams: [stdin.handle, stdout.handle, stderr.handle],
                                     ..Default::default() };
        if path.is_empty()
        {
            self.client.process(&mut call)?;
        }
        else
        {
            self.client.launch_argv(&mut call, &[], 0, path)?;
        }
        stdin.active = false;
        stdout.active = false;
        stderr.active = false;
        Ok(mac_traps::ProcessId(call.info.id))
    }

    fn pair(self, operation: u32, name: &str, capacity: usize, writers: u64)
            -> Result<(PipeReader<'a>, PipeWriter<'a>)>
    {
        let capacity = u32::try_from(capacity).map_err(|_| Error::BadArgument)?;
        let mut call = wire::Call { operation,
                                    nonblocking: 0,
                                    capacity,
                                    writers,
                                    ..Default::default() };
        name_into(&mut call, name)?;
        self.client.pipes(&mut call)?;
        Ok((PipeReader { client: self.client,
                         handle: call.reader,
                         active: true },
            PipeWriter { client: self.client,
                         handle: call.writer,
                         active: true }))
    }
}

fn name_into(call: &mut wire::Call, name: &str) -> Result<()>
{
    if name.len() > wire::NAME_BYTES
    {
        return Err(Error::InvalidName);
    }
    call.name_length = name.len() as u32;
    call.name[..name.len()].copy_from_slice(name.as_bytes());
    Ok(())
}

impl PipeReader<'_>
{
    /// Remove up to `output.len()` bytes. Zero is returned only when no writer
    /// remains and no buffered bytes remain.
    pub fn read(&mut self, output: &mut [u8]) -> Result<usize>
    {
        if output.is_empty()
        {
            return Err(Error::BadArgument);
        }
        let mut call = wire::Call { operation: wire::READ,
                                    handle: self.handle,
                                    buffer: output.as_mut_ptr(),
                                    length: output.len(),
                                    ..Default::default() };
        let result = self.client.pipes(&mut call);
        call.buffer = core::ptr::null_mut();
        result.map(|()| call.transferred)
    }

    /// Wait for data or real EOF, without changing the endpoint's blocking mode.
    /// `None` waits indefinitely; expiry returns `Error::WouldBlock`, preserving
    /// the output buffer. Zero duration makes one immediate attempt.
    pub fn read_wait(&mut self, output: &mut [u8], timeout: Option<core::time::Duration>)
                     -> Result<usize>
    {
        if output.is_empty()
        {
            return Err(Error::BadArgument);
        }
        let wait_nanoseconds = match timeout
        {
            Some(value) =>
            {
                u64::try_from(value.as_nanos()).ok()
                                               .filter(|value| *value != u64::MAX)
                                               .ok_or(Error::BadArgument)?
            }
            None => u64::MAX
        };
        let mut call = wire::Call { operation: wire::READ_WAIT,
                                    handle: self.handle,
                                    buffer: output.as_mut_ptr(),
                                    length: output.len(),
                                    wait_nanoseconds,
                                    ..Default::default() };
        let result = self.client.pipes(&mut call);
        call.buffer = core::ptr::null_mut();
        result.map(|()| call.transferred)
    }

    pub fn set_blocking(&mut self, blocking: bool) -> Result<()>
    {
        let mut call = wire::Call { operation: wire::SET_BLOCKING,
                                    handle: self.handle,
                                    nonblocking: u32::from(!blocking),
                                    ..Default::default() };
        self.client.pipes(&mut call)
    }

    /// Close explicitly so the caller can observe a refusal. Dropping also
    /// closes; the name disappears with the last handle.
    pub fn close(mut self) -> Result<()>
    {
        self.active = false;
        let mut call = wire::Call { operation: wire::CLOSE,
                                    handle: self.handle,
                                    ..Default::default() };
        self.client.pipes(&mut call)
    }
}
impl Drop for PipeReader<'_>
{
    fn drop(&mut self)
    {
        if self.active
        {
            self.active = false;
            let _ = self.client.pipes(&mut wire::Call { operation: wire::CLOSE,
                                                        handle: self.handle,
                                                        ..Default::default() });
        }
    }
}

impl PipeWriter<'_>
{
    /// Add bytes. A write of at most 512 bytes transfers all of them or none;
    /// a larger write transfers at least one and may transfer fewer than asked.
    pub fn write(&mut self, input: &[u8]) -> Result<usize>
    {
        if input.is_empty()
        {
            return Err(Error::BadArgument);
        }
        let mut call = wire::Call { operation: wire::WRITE,
                                    handle: self.handle,
                                    buffer: input.as_ptr() as *mut u8,
                                    length: input.len(),
                                    ..Default::default() };
        let result = self.client.pipes(&mut call);
        call.buffer = core::ptr::null_mut();
        result.map(|()| call.transferred)
    }

    pub fn set_blocking(&mut self, blocking: bool) -> Result<()>
    {
        let mut call = wire::Call { operation: wire::SET_BLOCKING,
                                    handle: self.handle,
                                    nonblocking: u32::from(!blocking),
                                    ..Default::default() };
        self.client.pipes(&mut call)
    }

    pub fn close(mut self) -> Result<()>
    {
        self.active = false;
        let mut call = wire::Call { operation: wire::CLOSE,
                                    handle: self.handle,
                                    ..Default::default() };
        self.client.pipes(&mut call)
    }
}
impl Drop for PipeWriter<'_>
{
    fn drop(&mut self)
    {
        if self.active
        {
            self.active = false;
            let _ = self.client.pipes(&mut wire::Call { operation: wire::CLOSE,
                                                        handle: self.handle,
                                                        ..Default::default() });
        }
    }
}
