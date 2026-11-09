//! System7 Mac Roman conversion (before the Mac OS8.5 currency-to-euro change).
use alloc::{string::String, vec::Vec};

use mac_text::HIGH;
pub use mac_text::character;

use crate::{Error, Result};

pub fn encode(text: &str) -> Result<Vec<u8>>
{
    text.chars()
        .map(|c| {
            if c.is_ascii()
            {
                Ok(c as u8)
            }
            else
            {
                HIGH.iter()
                    .position(|&v| v == c)
                    .map(|i| i as u8 + 128)
                    .ok_or(Error::Malformed("text is not representable in Mac Roman"))
            }
        })
        .collect()
}

pub fn decode(bytes: &[u8]) -> String
{
    bytes.iter().map(|&b| character(b)).collect()
}

pub fn pstring(text: &str) -> Result<Vec<u8>>
{
    let bytes = encode(text)?;
    if bytes.len() > 255
    {
        return Err(Error::Malformed("Pascal string exceeds 255 bytes"));
    }
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.push(bytes.len() as u8);
    out.extend(bytes);
    Ok(out)
}

