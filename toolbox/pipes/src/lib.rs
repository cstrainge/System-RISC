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

    /// Take ownership of a reader handle this process already holds, such as one
    /// a parent thread created. Endpoint ownership is per process, so another
    /// thread of the same application may read through it. A zero handle is
    /// refused.
    pub fn adopt_reader(self, handle: u64) -> Result<PipeReader<'a>>
    {
        if handle == 0
        {
            return Err(Error::BadArgument);
        }
        Ok(PipeReader { client: self.client,
                        handle,
                        active: true })
    }

    /// Take ownership of a writer handle this process already holds. Endpoint
    /// ownership is per process, so another thread of the same application may
    /// write through it. A zero handle is refused.
    pub fn adopt_writer(self, handle: u64) -> Result<PipeWriter<'a>>
    {
        if handle == 0
        {
            return Err(Error::BadArgument);
        }
        Ok(PipeWriter { client: self.client,
                        handle,
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
    /// The handle this reader owns. Passing it to `Pipes::adopt_reader` lets
    /// another thread of the same process read through it; the handle stays
    /// owned by the process either way.
    pub const fn raw(&self) -> u64
    {
        self.handle
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
