//! System7 Mac Roman conversion (before the Mac OS8.5 currency-to-euro change).
use alloc::{string::String, vec::Vec};

use crate::{Error, Result};

const HIGH: [char; 128] =
    ['\u{c4}', '\u{c5}', '\u{c7}', '\u{c9}', '\u{d1}', '\u{d6}', '\u{dc}', '\u{e1}', '\u{e0}',
     '\u{e2}', '\u{e4}', '\u{e3}', '\u{e5}', '\u{e7}', '\u{e9}', '\u{e8}', '\u{ea}', '\u{eb}',
     '\u{ed}', '\u{ec}', '\u{ee}', '\u{ef}', '\u{f1}', '\u{f3}', '\u{f2}', '\u{f4}', '\u{f6}',
     '\u{f5}', '\u{fa}', '\u{f9}', '\u{fb}', '\u{fc}', '\u{2020}', '\u{b0}', '\u{a2}', '\u{a3}',
     '\u{a7}', '\u{2022}', '\u{b6}', '\u{df}', '\u{ae}', '\u{a9}', '\u{2122}', '\u{b4}', '\u{a8}',
     '\u{2260}', '\u{c6}', '\u{d8}', '\u{221e}', '\u{b1}', '\u{2264}', '\u{2265}', '\u{a5}',
     '\u{b5}', '\u{2202}', '\u{2211}', '\u{220f}', '\u{3c0}', '\u{222b}', '\u{aa}', '\u{ba}',
     '\u{3a9}', '\u{e6}', '\u{f8}', '\u{bf}', '\u{a1}', '\u{ac}', '\u{221a}', '\u{192}',
     '\u{2248}', '\u{2206}', '\u{ab}', '\u{bb}', '\u{2026}', '\u{a0}', '\u{c0}', '\u{c3}',
     '\u{d5}', '\u{152}', '\u{153}', '\u{2013}', '\u{2014}', '\u{201c}', '\u{201d}', '\u{2018}',
     '\u{2019}', '\u{f7}', '\u{25ca}', '\u{ff}', '\u{178}', '\u{2044}', '\u{a4}', '\u{2039}',
     '\u{203a}', '\u{fb01}', '\u{fb02}', '\u{2021}', '\u{b7}', '\u{201a}', '\u{201e}', '\u{2030}',
     '\u{c2}', '\u{ca}', '\u{c1}', '\u{cb}', '\u{c8}', '\u{cd}', '\u{ce}', '\u{cf}', '\u{cc}',
     '\u{d3}', '\u{d4}', '\u{f8ff}', '\u{d2}', '\u{da}', '\u{db}', '\u{d9}', '\u{131}', '\u{2c6}',
     '\u{2dc}', '\u{af}', '\u{2d8}', '\u{2d9}', '\u{2da}', '\u{b8}', '\u{2dd}', '\u{2db}',
     '\u{2c7}'];

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
                    .ok_or(Error("text is not representable in Mac Roman"))
            }
        })
        .collect()
}

pub fn decode(bytes: &[u8]) -> String
{
    bytes.iter().map(|&b| if b < 128 { b as char } else { HIGH[b as usize - 128] }).collect()
}

pub fn pstring(text: &str) -> Result<Vec<u8>>
{
    let bytes = encode(text)?;
    if bytes.len() > 255
    {
        return Err(Error("Pascal string exceeds 255 bytes"));
    }
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.push(bytes.len() as u8);
    out.extend(bytes);
    Ok(out)
}

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn all_mac_roman_bytes_roundtrip()
    {
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(encode(&decode(&bytes)).unwrap(), bytes);
        assert_eq!(encode("Café").unwrap(), b"Caf\x8e");
        assert!(encode("🦀").is_err());
        assert!(pstring(&"a".repeat(256)).is_err());
    }
}
