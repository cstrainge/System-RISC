//! User-runtime entry offsets. Addresses are validated against the actual
//! owning executable after relocation; they never name resident functions.
use crate::{Error, Result};

pub const RESOURCE_KIND: [u8; 4] = *b"URTM";
pub const RESOURCE_ID: i16 = 0;
pub const BYTES: usize = 32;
/// Build-time RV64 little-endian table of two linked native function pointers.
pub const LINKED_BYTES: usize = 16;
pub const LINKED_SECTION: &[u8] = b".system_risc_user_runtime";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor
{
    pub callback_offset:      u64,
    pub thread_return_offset: u64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entries
{
    pub callback_loop: u64,
    pub thread_return: u64
}

impl Descriptor
{
    fn validate_offsets(self) -> Result<()>
    {
        if self.callback_offset & 1 != 0 || self.thread_return_offset & 1 != 0
        {
            return Err(Error("user runtime entries must be instruction aligned"));
        }
        Ok(())
    }

    /// Consume actual linked RV64 table bytes before section metadata is stripped.
    /// The packager separately proves this span is initialized read-only payload.
    pub fn from_linked(bytes: &[u8], base: u64) -> Result<Self>
    {
        if base & 1 != 0
        {
            return Err(Error("user runtime linked image base must be instruction aligned"));
        }
        if bytes.len() != LINKED_BYTES
        {
            return Err(Error("user runtime table must contain exactly two native addresses"));
        }
        let callback =
            u64::from_le_bytes(bytes[..8].try_into()
                                         .map_err(|_| Error("truncated user callback entry"))?);
        let returned = u64::from_le_bytes(bytes[8..].try_into()
                                                    .map_err(|_| {
                                                        Error("truncated user thread-return entry")
                                                    })?);
        let descriptor =
            Self { callback_offset:
                       callback.checked_sub(base)
                               .ok_or(Error("user callback entry precedes its executable"))?,
                   thread_return_offset:
                       returned.checked_sub(base)
                               .ok_or(Error("user thread-return entry precedes its executable"))? };
        descriptor.validate_offsets()?;
        Ok(descriptor)
    }

    pub fn encode(self) -> Result<[u8; BYTES]>
    {
        self.validate_offsets()?;
        let mut bytes = [0; BYTES];
        bytes[..4].copy_from_slice(b"URT1");
        bytes[4..6].copy_from_slice(&1u16.to_be_bytes());
        bytes[6..8].copy_from_slice(&(BYTES as u16).to_be_bytes());
        bytes[8..16].copy_from_slice(&self.callback_offset.to_be_bytes());
        bytes[16..24].copy_from_slice(&self.thread_return_offset.to_be_bytes());
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self>
    {
        if bytes.len() != BYTES
           || &bytes[..4] != b"URT1"
           || bytes[4..6] != 1u16.to_be_bytes()
           || bytes[6..8] != (BYTES as u16).to_be_bytes()
           || bytes[24..].iter().any(|byte| *byte != 0)
        {
            return Err(Error("invalid user runtime metadata; rebuild the application"));
        }
        let callback_offset =
            u64::from_be_bytes(bytes[8..16].try_into()
                                           .map_err(|_| Error("invalid user callback offset"))?);
        let thread_return_offset =
            u64::from_be_bytes(bytes[16..24].try_into()
                                            .map_err(|_| {
                                                Error("invalid user thread-return offset")
                                            })?);
        let result = Self { callback_offset,
                            thread_return_offset };
        result.validate_offsets()?;
        Ok(result)
    }

    pub fn resolve(self, base: u64, limit: u64, mut executable: impl FnMut(u64) -> bool)
                   -> Result<Entries>
    {
        self.validate_offsets()?;
        if base >= limit || base & 1 != 0
        {
            return Err(Error("invalid user runtime image range"));
        }
        let callback_loop = base.checked_add(self.callback_offset)
                                .ok_or(Error("user callback address overflows"))?;
        let thread_return = base.checked_add(self.thread_return_offset)
                                .ok_or(Error("user thread-return address overflows"))?;
        let callback_last =
            callback_loop.checked_add(1).ok_or(Error("user callback instruction overflows"))?;
        let return_last = thread_return.checked_add(1)
                                       .ok_or(Error("user thread-return instruction overflows"))?;
        if callback_last >= limit
           || return_last >= limit
           || callback_loop & 1 != 0
           || thread_return & 1 != 0
           || !executable(callback_loop)
           || !executable(callback_last)
           || !executable(thread_return)
           || !executable(return_last)
        {
            return Err(Error("user runtime entry lies outside initialized bytes of its owning \
                              executable"));
        }
        Ok(Entries { callback_loop,
                     thread_return })
    }
}
