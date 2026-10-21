//! Safe owned handle clients. Allocations and state live in the resident System.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::memory::*;
pub use mac_traps::types::MemoryHandleId;
mac_traps::native_flags!(HandleState, u8; LOCKED = mac_abi::memory::LOCKED,
    PURGEABLE = mac_abi::memory::PURGEABLE_BIT, RESOURCE = mac_abi::memory::RESOURCE);
pub use mac_traps::{Client, Error, Result};

#[derive(Clone, Copy)]
pub struct Memory<'a>
{
    client: Client<'a>
}
impl<'a> Memory<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    pub fn new_handle_clear(self, size: usize) -> Result<Handle<'a>>
    {
        let mut call = MemoryCall { operation: NEW_CLEAR,
                                    size: length(size)?,
                                    ..Default::default() };
        self.client.memory(&mut call, &[], &mut [])?;
        Ok(Handle { client: self.client,
                    id:     MemoryHandleId(call.handle) })
    }

    pub fn handle_from_bytes(self, bytes: &[u8]) -> Result<Handle<'a>>
    {
        let mut call = MemoryCall { operation: FROM_BYTES,
                                    ..Default::default() };
        self.client.memory(&mut call, bytes, &mut [])?;
        Ok(Handle { client: self.client,
                    id:     MemoryHandleId(call.handle) })
    }

    pub fn last_error(self) -> Result<Option<Error>>
    {
        let mut call = MemoryCall { operation: ERROR,
                                    ..Default::default() };
        self.client.memory(&mut call, &[], &mut [])?;
        Ok((call.error != 0).then(|| Error::from_status(call.error)))
    }

    pub fn new_empty_handle(self) -> Result<Handle<'a>>
    {
        let mut call = MemoryCall { operation: NEW_EMPTY,
                                    ..Default::default() };
        self.client.memory(&mut call, &[], &mut [])?;
        Ok(Handle { client: self.client,
                    id:     MemoryHandleId(call.handle) })
    }
}

/// Stable opaque identity. No raw data pointer escapes System. Byte access
/// borrows caller buffers; locking retains the original handle-state bits.
#[must_use = "dropping the handle owner disposes its allocation"]
pub struct Handle<'a>
{
    client: Client<'a>,
    id:     MemoryHandleId
}
fn length(n: usize) -> Result<u32>
{
    u32::try_from(n).map_err(|_| Error::BadArgument)
}
impl<'a> Handle<'a>
{
    fn perform(&self, operation: u32) -> Result<MemoryCall>
    {
        let mut call = MemoryCall { operation,
                                    handle: self.id.0,
                                    ..Default::default() };
        self.client.memory(&mut call, &[], &mut [])?;
        Ok(call)
    }

    pub fn len(&self) -> Result<usize>
    {
        Ok(self.perform(SIZE)?.size as usize)
    }

    /// Whether allocated data has zero length. An emptied/purged handle instead
    /// reports nilHandleErr, just like its original size query.
    pub fn is_empty(&self) -> Result<bool>
    {
        self.len().map(|size| size == 0)
    }

    pub fn state(&self) -> Result<HandleState>
    {
        Ok(HandleState::from_bits_retain(self.perform(STATE)?.state as u8))
    }

    pub fn set_state(&self, state: HandleState) -> Result<()>
    {
        let mut call = MemoryCall { operation: SET_STATE,
                                    handle: self.id.0,
                                    state: u32::from(state.bits()),
                                    ..Default::default() };
        self.client.memory(&mut call, &[], &mut [])
    }

    pub fn lock(&self) -> Result<()>
    {
        self.perform(LOCK).map(|_| ())
    }

    pub fn unlock(&self) -> Result<()>
    {
        self.perform(UNLOCK).map(|_| ())
    }

    pub fn set_purgeable(&self, purgeable: bool) -> Result<()>
    {
        self.perform(if purgeable { PURGEABLE } else { NONPURGEABLE }).map(|_| ())
    }

    pub fn resize(&self, size: usize) -> Result<()>
    {
        self.resize_with(RESIZE, size)
    }

    /// Reallocate an emptied handle; existing contents are not preserved.
    pub fn reallocate(&self, size: usize) -> Result<()>
    {
        self.resize_with(REALLOCATE, size)
    }

    fn resize_with(&self, operation: u32, size: usize) -> Result<()>
    {
        let mut call = MemoryCall { operation,
                                    handle: self.id.0,
                                    size: length(size)?,
                                    ..Default::default() };
        self.client.memory(&mut call, &[], &mut [])
    }

    pub fn empty(&self) -> Result<()>
    {
        self.perform(EMPTY).map(|_| ())
    }

    pub fn duplicate(&self) -> Result<Handle<'a>>
    {
        let call = self.perform(DUPLICATE)?;
        Ok(Handle { client: self.client,
                    id:     MemoryHandleId(call.other) })
    }

    pub fn read(&self, offset: usize, output: &mut [u8]) -> Result<usize>
    {
        let mut call = MemoryCall { operation: READ,
                                    handle: self.id.0,
                                    offset: length(offset)?,
                                    ..Default::default() };
        self.client.memory(&mut call, &[], output)?;
        Ok(call.length)
    }

    pub fn write(&self, offset: usize, input: &[u8]) -> Result<()>
    {
        let mut call = MemoryCall { operation: WRITE,
                                    handle: self.id.0,
                                    offset: length(offset)?,
                                    ..Default::default() };
        self.client.memory(&mut call, input, &mut [])
    }

    pub fn append(&self, input: &[u8]) -> Result<()>
    {
        let mut call = MemoryCall { operation: APPEND_BYTES,
                                    handle: self.id.0,
                                    ..Default::default() };
        self.client.memory(&mut call, input, &mut [])
    }

    pub fn replace(&self, input: &[u8]) -> Result<()>
    {
        let mut call = MemoryCall { operation: REPLACE_BYTES,
                                    handle: self.id.0,
                                    ..Default::default() };
        self.client.memory(&mut call, input, &mut [])
    }

    pub fn append_handle(&self, source: &Handle<'_>) -> Result<()>
    {
        let mut call = MemoryCall { operation: APPEND_HANDLE,
                                    handle: self.id.0,
                                    other: source.id.0,
                                    ..Default::default() };
        self.client.memory(&mut call, &[], &mut [])
    }

    /// Dispose without losing ownership on temporary contention. A future
    /// scheduler-aware gate can block instead; callers must not spin in kernel.
    pub fn try_dispose(&mut self) -> Result<()>
    {
        if self.id.0 != 0
        {
            self.perform(DISPOSE)?;
            self.id = MemoryHandleId(0);
        }
        Ok(())
    }

    pub fn dispose(&mut self) -> Result<()>
    {
        self.try_dispose()
    }
}
impl Drop for Handle<'_>
{
    fn drop(&mut self)
    {
        if self.id.0 != 0
        {
            let _ = self.perform(DISPOSE);
        }
    }
}
