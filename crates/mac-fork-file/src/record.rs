//! Pure bounded records shared by synchronous tooling and asynchronous drivers.
//! Transport scheduling never changes the container layout or validation rules.
use sha2::{Digest, Sha256};

use crate::*;

/// One complete metadata area, including canonical zero padding. A staging
/// writer clears the first HEADER_BYTES until all fork chunks are acknowledged,
/// then publishes that header last and waits for its real durable flush.
pub fn encode_metadata(metadata: &Metadata, data: u64, resources: u64)
                       -> Result<([u8; METADATA_BYTES], Layout)>
{
    let (header, layout) = Header::encode(metadata, data, resources)?;
    let mut bytes = [0; METADATA_BYTES];
    bytes[..HEADER_BYTES].copy_from_slice(header.bytes());
    let end = HEADER_BYTES + metadata.name.len();
    bytes[HEADER_BYTES..end].copy_from_slice(metadata.name.as_bytes());
    bytes[end..end + metadata.comment.len()].copy_from_slice(metadata.comment.as_bytes());
    Ok((bytes, layout))
}

/// Validate header, metadata checksum, UTF-8/NFC, lengths and all padding before
/// exposing metadata or a fork layout. actual_length is the leased file length.
pub fn decode_metadata(bytes: &[u8; METADATA_BYTES], actual_length: u64)
                       -> Result<(Metadata, Layout)>
{
    let header = Header::decode(bytes[..HEADER_BYTES].try_into().unwrap(), actual_length)?;
    let end = HEADER_BYTES + header.metadata_bytes();
    let decoded = header.metadata(&bytes[HEADER_BYTES..end])?;
    if bytes[end..].iter().any(|&byte| byte != 0)
    {
        return Err(Error::Corrupt);
    }
    Ok(decoded)
}

/// Encode one chunk followed immediately by its digest. The caller obtains the
/// exact required length from ForkLayout::chunk; an empty fork has no chunks.
pub fn encode_chunk(input: &[u8], expected: usize, output: &mut [u8]) -> Result<usize>
{
    if input.len() != expected
    {
        return Err(Error::Invalid);
    }
    if expected == 0 || expected > CHUNK_BYTES
    {
        return Err(Error::Range);
    }
    let length = expected + HASH_BYTES as usize;
    let target = output.get_mut(..length).ok_or(Error::Range)?;
    target[..expected].copy_from_slice(input);
    let digest: [u8; 32] = Sha256::digest(input).into();
    target[expected..].copy_from_slice(&digest);
    Ok(length)
}

/// Verify a complete physical chunk before any of its bytes reach the caller.
pub fn decode_chunk(input: &[u8], expected: usize) -> Result<&[u8]>
{
    if expected == 0 || expected > CHUNK_BYTES || input.len() != expected + HASH_BYTES as usize
    {
        return Err(Error::Range);
    }
    let digest: [u8; 32] = Sha256::digest(&input[..expected]).into();
    if digest != input[expected..]
    {
        return Err(Error::Corrupt);
    }
    Ok(&input[..expected])
}
