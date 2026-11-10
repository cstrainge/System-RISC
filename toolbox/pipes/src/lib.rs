//! Byte-transport pipes. A nonempty `read` on an ordinary pipe returns zero at
//! end of stream. Attached cooked terminal input also returns zero once for
//! Ctrl+D on an empty line; that indication does not close the pipe, and later
//! reads can receive more input. A write with no readers fails with `BrokenPipe`;
//! non-blocking mode reports `WouldBlock`
//! instead of parking. A 512-byte write is atomic against other writers, and
//! that bound is never a record size.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::pipes as wire;
use mac_traps::{Client, Cnid, Error, Result, VolumeId};

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

/// The right to one endpoint handle, and the only way to move one between
/// threads of the same process. It is produced by consuming a `PipeReader` or
/// `PipeWriter`, so one handle yields at most one token and a handle cannot be
/// adopted twice; adopting consumes the token, so a handle cannot be left with
/// two owners. A token dropped without being adopted closes its handle rather
/// than leaking it.
pub struct Endpoint<'a>
{
    client: Client<'a>,
    handle: Option<u64>
}
impl Drop for Endpoint<'_>
{
    fn drop(&mut self)
    {
        if let Some(handle) = self.handle.take()
        {
            let _ = self.client.pipes(&mut wire::Call { operation: wire::CLOSE,
                                                        handle,
                                                        ..Default::default() });
        }
    }
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

    /// Take ownership of a reader handle this process already holds, by
    /// consuming the endpoint token that authorises it. Endpoint ownership is
    /// per process, so another thread of the same application may pass a reader
    /// here. Because the token is consumed and cannot be copied, two readers
    /// cannot be adopted from one handle.
    pub fn adopt_reader(self, endpoint: Endpoint<'a>) -> Result<PipeReader<'a>>
    {
        let handle = endpoint.take()?;
        Ok(PipeReader { client: self.client,
                        handle,
                        active: true })
    }

    /// Take ownership of a writer handle this process already holds, by
    /// consuming the endpoint token that authorises it. A zero handle is refused.
    pub fn adopt_writer(self, endpoint: Endpoint<'a>) -> Result<PipeWriter<'a>>
    {
        let handle = endpoint.take()?;
        Ok(PipeWriter { client: self.client,
                        handle,
                        active: true })
    }

    /// Open a named pipe item: the File Manager performs `connect` for this
    /// process. The returned writer is the caller's only owner of that handle,
    /// and dropping it closes the endpoint as it would for `connect`.
    ///
    /// This is how a Finder double-click and any other item-open reaches a pipe;
    /// `open` on a pipe item never launches an application. A consumed writer
    /// limit is `AccessDenied`; a destroyed identity is `NotFound`.
    pub fn open_item(self, volume: VolumeId, item: Cnid) -> Result<PipeWriter<'a>>
    {
        let mut call = mac_abi::files::PipeOpenCall { operation: mac_abi::files::OPEN_PIPE,
                                                      volume: volume.0,
                                                      cnid: item.0,
                                                      ..Default::default() };
        self.client.file_pipe_open(&mut call)?;
        Ok(PipeWriter { client: self.client,
                        handle: call.writer,
                        active: true })
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

impl Endpoint<'_>
{
    /// Move the handle out, leaving the token inert so its `Drop` cannot close a
    /// handle that a new owner now holds.
    fn take(mut self) -> Result<u64>
    {
        self.handle.take().filter(|handle| *handle != 0).ok_or(Error::BadArgument)
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

impl<'a> PipeReader<'a>
{
    pub fn set_terminal_mode(&self, flags: u32) -> Result<()>
    {
        if !self.active
        {
            return Err(Error::BadArgument);
        }
        let mut call = wire::Call { operation: wire::TERMINAL_MODE,
                                    handle: self.handle,
                                    terminal: wire::TerminalInfo { flags,
                                                                   ..Default::default() },
                                    ..Default::default() };
        self.client.pipes(&mut call)
    }

    /// Query the terminal associated with this endpoint. Ordinary redirected
    /// pipes return NotSupported; a stale or foreign endpoint is an error.
    pub fn terminal(&self) -> Result<wire::TerminalInfo>
    {
        if !self.active
        {
            return Err(Error::BadArgument);
        }
        let mut call = wire::Call { operation: wire::TERMINAL_QUERY,
                                    handle: self.handle,
                                    ..Default::default() };
        self.client.pipes(&mut call)?;
        Ok(call.terminal)
    }

    /// Obtain an independent blocking endpoint alias. It keeps the pipe alive
    /// until dropped and can be transferred to a child without consuming this one.
    pub fn try_clone(&self) -> Result<Self>
    {
        if !self.active
        {
            return Err(Error::BadArgument);
        }
        let mut call = wire::Call { operation: wire::DUPLICATE,
                                    handle: self.handle,
                                    ..Default::default() };
        self.client.pipes(&mut call)?;
        Ok(Self { client: self.client,
                  handle: call.reader,
                  active: true })
    }

    /// Non-owning readiness source. It grants no endpoint access and becomes
    /// invalid when this endpoint closes or is transferred to another owner.
    pub fn readiness(&self) -> mac_abi::readiness::Source
    {
        mac_abi::readiness::Source { kind:    mac_abi::readiness::PIPE_READ,
                                     id:      self.handle,
                                     minimum: 0,
                                     ready:   0 }
    }

    /// Hand this reader's handle to another thread of the same process without
    /// closing it. Consumes the reader, so the handed-over handle never has two
    /// owners; the receiving side adopts it with `Pipes::adopt_reader`.
    pub fn release(mut self) -> Endpoint<'a>
    {
        self.active = false;
        Endpoint { client: self.client,
                   handle: Some(self.handle) }
    }

    /// Remove up to `output.len()` bytes. Zero indicates drained writer closure
    /// or, for cooked terminal input, one empty-line Ctrl+D indication. The
    /// latter leaves the endpoint open and later input can be read normally.
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

    /// Wait for data or an EOF indication without changing the blocking mode.
    /// On cooked terminal input, an empty-line Ctrl+D is a single EOF indication
    /// and leaves the endpoint open for subsequent reads.
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

impl<'a> PipeWriter<'a>
{
    /// Interrupt the associated cooked terminal even when its input is full.
    /// NotSupported means interrupt processing is disabled (normally raw mode).
    pub fn interrupt_terminal(&self) -> Result<()>
    {
        if !self.active
        {
            return Err(Error::BadArgument);
        }
        let mut call = wire::Call { operation: wire::TERMINAL_INTERRUPT,
                                    handle: self.handle,
                                    ..Default::default() };
        self.client.pipes(&mut call)
    }

    /// Query the terminal associated with this endpoint. Ordinary redirected
    /// pipes return NotSupported; a stale or foreign endpoint is an error.
    pub fn terminal(&self) -> Result<wire::TerminalInfo>
    {
        if !self.active
        {
            return Err(Error::BadArgument);
        }
        let mut call = wire::Call { operation: wire::TERMINAL_QUERY,
                                    handle: self.handle,
                                    ..Default::default() };
        self.client.pipes(&mut call)?;
        Ok(call.terminal)
    }

    /// Turn caller-owned anonymous streams into a terminal. The frontend keeps
    /// this input writer and both output readers; child ends may then transfer.
    pub fn attach_terminal(&self, stdout: &PipeReader<'a>, stderr: &PipeReader<'a>,
                           size: wire::TerminalInfo)
                           -> Result<()>
    {
        if !self.active || !stdout.active || !stderr.active
        {
            return Err(Error::BadArgument);
        }
        let mut call = wire::Call { operation: wire::TERMINAL_ATTACH,
                                    handle: self.handle,
                                    terminal_outputs: [stdout.handle, stderr.handle],
                                    terminal: size,
                                    ..Default::default() };
        self.client.pipes(&mut call)
    }

    /// Feed keyboard or paste input through resident terminal discipline.
    /// `write` is still the transparent path for terminal protocol replies.
    pub fn terminal_input(&mut self, bytes: &[u8]) -> Result<usize>
    {
        if !self.active || bytes.is_empty()
        {
            return Err(Error::BadArgument);
        }
        let mut call = wire::Call { operation: wire::TERMINAL_INPUT,
                                    handle: self.handle,
                                    buffer: bytes.as_ptr().cast_mut(),
                                    length: bytes.len(),
                                    ..Default::default() };
        self.client.pipes(&mut call)?;
        Ok(call.transferred)
    }

    pub fn resize_terminal(&self, size: wire::TerminalInfo) -> Result<()>
    {
        if !self.active
        {
            return Err(Error::BadArgument);
        }
        let mut call = wire::Call { operation: wire::TERMINAL_RESIZE,
                                    handle: self.handle,
                                    terminal: size,
                                    ..Default::default() };
        self.client.pipes(&mut call)
    }

    /// Obtain an independent blocking endpoint alias. It keeps the pipe alive
    /// until dropped and can be transferred to a child without consuming this one.
    pub fn try_clone(&self) -> Result<Self>
    {
        if !self.active
        {
            return Err(Error::BadArgument);
        }
        let mut call = wire::Call { operation: wire::DUPLICATE,
                                    handle: self.handle,
                                    ..Default::default() };
        self.client.pipes(&mut call)?;
        Ok(Self { client: self.client,
                  handle: call.writer,
                  active: true })
    }

    /// Non-owning readiness source. It grants no endpoint access and becomes
    /// invalid when this endpoint closes or is transferred to another owner.
    pub fn readiness(&self, minimum: usize) -> mac_abi::readiness::Source
    {
        mac_abi::readiness::Source { kind:    mac_abi::readiness::PIPE_WRITE,
                                     id:      self.handle,
                                     minimum: minimum as u64,
                                     ready:   0 }
    }

    /// Hand this writer's handle to another thread of the same process without
    /// closing it. Consumes the writer, so the handed-over handle never has two
    /// owners; the receiving side adopts it with `Pipes::adopt_writer`.
    pub fn release(mut self) -> Endpoint<'a>
    {
        self.active = false;
        Endpoint { client: self.client,
                   handle: Some(self.handle) }
    }

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
