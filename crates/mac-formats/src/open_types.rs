//! Classic Translation Manager `open` declarations, shared by tools and System.
//! Apple develop18 (June1994), printed p87: signature, zero word, count word,
//! then an array of big-endian FourCCs. A declaration describes opening, not
//! merely the document types for which an application supplies icons.
#![forbid(unsafe_code)]
use crate::{Error, Result};

#[derive(Clone, Copy, Debug)]
pub struct OpenTypes<'a>
{
    signature: [u8; 4],
    types:     &'a [[u8; 4]]
}
impl<'a> OpenTypes<'a>
{
    pub fn decode(bytes: &'a [u8]) -> Result<Self>
    {
        let header = bytes.get(..8).ok_or(Error::Malformed("truncated open-types header"))?;
        if header[4..6] != [0; 2]
        {
            return Err(Error::Malformed("open-types reserved word must be zero"));
        }
        let count = usize::from(u16::from_be_bytes(header[6..8].try_into().unwrap()));
        if bytes.len() != 8 + count * 4
        {
            return Err(Error::Malformed("open-types count does not match resource length"));
        }
        Ok(Self { signature: header[..4].try_into().unwrap(),
                  types:     bytes[8..].as_chunks::<4>().0 })
    }

    pub const fn signature(self) -> [u8; 4]
    {
        self.signature
    }

    pub const fn types(self) -> &'a [[u8; 4]]
    {
        self.types
    }

    pub fn accepts(self, kind: [u8; 4]) -> bool
    {
        self.types.contains(&kind)
    }
}

/// Encode into caller-owned storage; rejection leaves the output unchanged.
pub fn encode(signature: [u8; 4], types: &[[u8; 4]], output: &mut [u8]) -> Result<usize>
{
    let count = u16::try_from(types.len()).map_err(|_| {
                                              Error::Malformed("open-types array exceeds the \
                                                                classic u16 count")
                                          })?;
    let length = 8 + types.len() * 4;
    let output = output.get_mut(..length)
                       .ok_or(Error::Malformed("open-types output buffer is too small"))?;
    output[..4].copy_from_slice(&signature);
    output[4..6].fill(0);
    output[6..8].copy_from_slice(&count.to_be_bytes());
    for (out, kind) in output[8..].chunks_exact_mut(4).zip(types)
    {
        out.copy_from_slice(kind);
    }
    Ok(length)
}
