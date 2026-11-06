//! Byte-transport pipes. `read` returns zero only at end of stream; a write with
//! no readers fails with `BrokenPipe`; non-blocking mode reports `WouldBlock`
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
    /// Hand this reader's handle to another thread of the same process without
    /// closing it. Consumes the reader, so the handed-over handle never has two
    /// owners; the receiving side adopts it with `Pipes::adopt_reader`.
    pub fn release(mut self) -> Endpoint<'a>
    {
        self.active = false;
        Endpoint { client: self.client,
                   handle: Some(self.handle) }
    }

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
