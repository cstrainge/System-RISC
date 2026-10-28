//! Inline snapshots lend bounded text across disjoint mutable manager calls.
//! No paint-time copy needs a String allocation merely to satisfy borrowing.
#![forbid(unsafe_code)]
use mac_abi::{BAD_ABI, BAD_ARGUMENT, INLINE_STRING_BYTES, Status};

#[derive(Clone, Copy)]
pub(in crate::toolbox) struct Text
{
    bytes:  [u8; INLINE_STRING_BYTES],
    length: usize
}
impl Default for Text
{
    fn default() -> Self
    {
        Self { bytes:  [0; INLINE_STRING_BYTES],
               length: 0 }
    }
}
impl Text
{
    pub fn new(text: &str) -> Result<Self, Status>
    {
        let mut value = Self::default();
        value.push(text)?;
        Ok(value)
    }

    pub fn clear(&mut self)
    {
        self.length = 0;
    }

    pub fn push(&mut self, text: &str) -> Result<(), Status>
    {
        let end = self.length
                      .checked_add(text.len())
                      .filter(|&n| n <= self.bytes.len())
                      .ok_or(BAD_ARGUMENT)?;
        self.bytes[self.length..end].copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
    }

    pub fn text(&self) -> Result<&str, Status>
    {
        core::str::from_utf8(&self.bytes[..self.length]).map_err(|_| BAD_ABI)
    }
}
