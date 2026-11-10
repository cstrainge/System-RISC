//! Owned format values around the allocation-free `mac-text` codec.
//! Character mapping, representability and Pascal capacity rules live there.
use alloc::{string::String, vec::Vec};

pub use mac_text::{MacRomanText, byte, character, decode_into, utf8_length};

use crate::{Error, Result};

fn output(length: usize) -> Result<Vec<u8>>
{
    let mut result = Vec::new();
    result.try_reserve_exact(length)
          .map_err(|_| Error::AllocationFailed("text conversion output allocation refused"))?;
    result.resize(length, 0);
    Ok(result)
}

pub fn encode(text: &str) -> Result<Vec<u8>>
{
    let value = MacRomanText::new(text)?;
    let mut result = output(value.len())?;
    value.write_into(&mut result)?;
    Ok(result)
}

pub fn decode(bytes: &[u8]) -> String
{
    mac_text::characters(bytes).collect()
}

pub fn pstring(text: &str) -> Result<Vec<u8>>
{
    let value = MacRomanText::new(text)?;
    let mut result = output(value.pascal_length(255)?)?;
    value.write_pascal(255, &mut result)?;
    Ok(result)
}
