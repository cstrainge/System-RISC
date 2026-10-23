//! MacBinary II transport. An HFS file has real forks, not a .bin extension.
use alloc::{vec, vec::Vec};

use crate::{Error, Result, be16, be32, slice};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacFile
{
    pub name:      Vec<u8>,
    pub file_type: [u8; 4],
    pub creator:   [u8; 4],
    pub flags:     u16,
    /// Seconds since 1904-01-01.
    pub created:   u32,
    pub modified:  u32,
    pub data:      Vec<u8>,
    pub resources: Vec<u8>
}

pub fn crc16(bytes: &[u8]) -> u16
{
    let mut crc = 0u16;
    for &byte in bytes
    {
        crc ^= (byte as u16) << 8;
        for _ in 0..8
        {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

fn padded(length: usize) -> Result<usize>
{
    Ok(length.checked_add(127).ok_or(Error::Malformed("MacBinary size overflow"))? & !127)
}

pub fn encode(file: &MacFile) -> Result<Vec<u8>>
{
    if file.name.is_empty()
       || file.name.len() > 63
       || file.name.contains(&b':')
       || file.name.contains(&0)
    {
        return Err(Error::Malformed("invalid Macintosh filename"));
    }
    let mut output = vec![0; 128];
    output[1] = file.name.len() as u8;
    output[2..2 + file.name.len()].copy_from_slice(&file.name);
    output[65..69].copy_from_slice(&file.file_type);
    output[69..73].copy_from_slice(&file.creator);
    output[73] = (file.flags >> 8) as u8;
    output[101] = file.flags as u8;
    for (offset, length) in [(83, file.data.len()), (87, file.resources.len())]
    {
        output[offset..offset + 4].copy_from_slice(&u32::try_from(length).map_err(|_| {
                                                        Error::Malformed("fork exceeds 32 bits")
                                                    })?
                                                    .to_be_bytes());
    }
    output[91..95].copy_from_slice(&file.created.to_be_bytes());
    output[95..99].copy_from_slice(&file.modified.to_be_bytes());
    output[122] = 129;
    output[123] = 129;
    let crc = crc16(&output[..124]);
    output[124..126].copy_from_slice(&crc.to_be_bytes());
    output.extend_from_slice(&file.data);
    output.resize(128 + padded(file.data.len())?, 0);
    output.extend_from_slice(&file.resources);
    output.resize(128 + padded(file.data.len())? + padded(file.resources.len())?, 0);
    Ok(output)
}

/// Accept II plus original MacBinary headers emitted by some HFS tools.
pub fn decode(bytes: &[u8]) -> Result<MacFile>
{
    let h = slice(bytes, 0, 128)?;
    if h[0] != 0 || h[74] != 0 || h[82] != 0 || !(1..=63).contains(&h[1])
    {
        return Err(Error::Malformed("invalid MacBinary header"));
    }
    if h[122] >= 129
    {
        if h[123] > 129 || crc16(&h[..124]) != be16(h, 124)?
        {
            return Err(Error::Malformed("MacBinary version or CRC mismatch"));
        }
    }
    else if h[101..126].iter().any(|&b| b != 0)
    {
        return Err(Error::Malformed("invalid original MacBinary header"));
    }
    let data_length = be32(h, 83)? as usize;
    let rsrc_length = be32(h, 87)? as usize;
    let data_start = 128 + padded(be16(h, 120)? as usize)?;
    let rsrc_start = data_start.checked_add(padded(data_length)?)
                               .ok_or(Error::Malformed("MacBinary size overflow"))?;
    let expected_end = rsrc_start.checked_add(padded(rsrc_length)?)
                                 .ok_or(Error::Malformed("MacBinary size overflow"))?;
    slice(bytes, 0, expected_end)?;
    let name = slice(h, 2, h[1] as usize)?.to_vec();
    if name.contains(&b':') || name.contains(&0)
    {
        return Err(Error::Malformed("invalid Macintosh filename"));
    }
    Ok(MacFile { name,
                 file_type: h[65..69].try_into().unwrap(),
                 creator: h[69..73].try_into().unwrap(),
                 flags: ((h[73] as u16) << 8) | h[101] as u16,
                 created: be32(h, 91)?,
                 modified: be32(h, 95)?,
                 data: slice(bytes, data_start, data_length)?.to_vec(),
                 resources: slice(bytes, rsrc_start, rsrc_length)?.to_vec() })
}

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn crc_and_two_forks_roundtrip()
    {
        assert_eq!(crc16(b"123456789"), 0x31c3);
        let file = MacFile { name:      b"Finder".to_vec(),
                             file_type: *b"FNDR",
                             creator:   *b"MACS",
                             flags:     0x2040,
                             created:   2082844800,
                             modified:  2082844800,
                             data:      vec![42; 129],
                             resources: vec![7; 257] };
        let bytes = encode(&file).unwrap();
        assert_eq!(bytes.len(), 128 + 256 + 384);
        assert_eq!(decode(&bytes).unwrap(), file);
        for len in 0..bytes.len()
        {
            assert!(decode(&bytes[..len]).is_err());
        }
        let mut broken = bytes;
        broken[65] ^= 1;
        assert!(decode(&broken).is_err());
    }
}
