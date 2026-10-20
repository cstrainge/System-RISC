//! Pinned Unicode 17.0.0 NFC storage and canonical caseless matching (UAX #15).
//! No host locale or Rust/compiler Unicode tables participate in disk ordering.
use alloc::{string::String, vec::Vec};

use crate::{Error, Result};
mod tables;
pub const UNICODE_VERSION: u32 = 0x0011_0000;
pub const MAX_NAME: usize = 255;
pub const MAX_INPUT: usize = 16 * 1024;
const MAX_SCALARS: usize = MAX_INPUT * 4;

fn push(out: &mut Vec<u32>, c: u32) -> Result<()>
{
    if out.len() == MAX_SCALARS
    {
        return Err(Error::Budget);
    }
    out.try_reserve(1).map_err(|_| Error::NoMemory)?;
    out.push(c);
    Ok(())
}
fn class(c: u32) -> u8
{
    tables::COMBINING.binary_search_by_key(&c, |r| r.0).map_or(0, |i| tables::COMBINING[i].1)
}
fn decompose(c: u32, out: &mut Vec<u32>, depth: u8) -> Result<()>
{
    if depth > 8
    {
        return Err(Error::Corrupt);
    }
    if (0xac00..0xd7a4).contains(&c)
    {
        let s = c - 0xac00;
        push(out, 0x1100 + s / 588)?;
        push(out, 0x1161 + (s % 588) / 28)?;
        if !s.is_multiple_of(28)
        {
            push(out, 0x11a7 + s % 28)?;
        }
    }
    else if let Ok(i) = tables::DECOMPOSE.binary_search_by_key(&c, |r| r.0)
    {
        for &n in tables::DECOMPOSE[i].1
        {
            decompose(n, out, depth + 1)?;
        }
    }
    else
    {
        push(out, c)?;
    }
    Ok(())
}
fn nfd(codes: impl Iterator<Item = u32>) -> Result<Vec<u32>>
{
    let mut out = Vec::new();
    for c in codes
    {
        decompose(c, &mut out, 0)?;
    }
    // Stable canonical ordering, bounded by the independently checked work cap.
    // In-place heapsort is unstable; use insertion only inside short mark runs,
    // and reject pathological quadratic input before doing that work.
    let mut work = 0usize;
    for i in 1..out.len()
    {
        let cc = class(out[i]);
        if cc == 0
        {
            continue;
        }
        let mut j = i;
        while j > 0 && class(out[j - 1]) > cc
        {
            work += 1;
            if work > MAX_SCALARS * 8
            {
                return Err(Error::Budget);
            }
            out.swap(j, j - 1);
            j -= 1;
        }
    }
    Ok(out)
}
fn pair(a: u32, b: u32) -> Option<u32>
{
    if (0x1100..0x1113).contains(&a) && (0x1161..0x1176).contains(&b)
    {
        return Some(0xac00 + (a - 0x1100) * 588 + (b - 0x1161) * 28);
    }
    if (0xac00..0xd7a4).contains(&a)
       && (a - 0xac00).is_multiple_of(28)
       && (0x11a8..0x11c3).contains(&b)
    {
        return Some(a + b - 0x11a7);
    }
    tables::COMPOSE.binary_search_by_key(&(a, b), |r| (r.0, r.1)).ok().map(|i| tables::COMPOSE[i].2)
}
fn string(codes: &[u32]) -> Result<String>
{
    let mut out = String::new();
    out.try_reserve(codes.len().checked_mul(4).ok_or(Error::Budget)?)
       .map_err(|_| Error::NoMemory)?;
    for &c in codes
    {
        out.push(char::from_u32(c).ok_or(Error::Corrupt)?);
    }
    Ok(out)
}
pub fn nfc(input: &str) -> Result<String>
{
    if input.len() > MAX_INPUT
    {
        return Err(Error::Budget);
    }
    let codes = nfd(input.chars().map(u32::from))?;
    let mut out = Vec::new();
    let mut starter = None;
    let mut previous_class = 0;
    for c in codes
    {
        let cc = class(c);
        if let Some(i) = starter
           && (previous_class == 0 || previous_class < cc)
           && let Some(composed) = pair(out[i], c)
        {
            out[i] = composed;
            continue;
        }
        if cc == 0
        {
            starter = Some(out.len());
        }
        push(&mut out, c)?;
        previous_class = cc;
    }
    string(&out)
}
pub fn normalize(input: &str) -> Result<String>
{
    let name = nfc(input)?;
    if name.is_empty()
       || name.len() > MAX_NAME
       || name == "."
       || name == ".."
       || name.contains(['\0', '/', ':'])
    {
        return Err(Error::Invalid);
    }
    Ok(name)
}
pub fn comparison(input: &str) -> Result<String>
{
    if input.len() > MAX_INPUT
    {
        return Err(Error::Budget);
    }
    let canonical = nfd(input.chars().map(u32::from))?;
    let mut folded = Vec::new();
    for c in canonical
    {
        if let Ok(i) = tables::FOLD.binary_search_by_key(&c, |r| r.0)
        {
            for &f in tables::FOLD[i].1
            {
                push(&mut folded, f)?;
            }
        }
        else
        {
            push(&mut folded, c)?;
        }
    }
    string(&nfd(folded.into_iter())?)
}
