//! Build-time System policy grants bound to the exact loaded resource content.
//! A package name, creator code or self-declared resource grants no authority.
use alloc::vec::Vec;

use sha2::{Digest, Sha256};

use crate::{Error, Result, resource::Resource};

pub const RESOURCE_KIND: [u8; 4] = *b"ACAP";
pub const INSPECT_DOMAINS: u64 = 1;

/// Canonical content identity, independent of fork offsets and input ordering.
/// Length prefixes and the optional-name tag make the encoding unambiguous.
pub fn image_digest(resources: &[Resource]) -> Result<[u8; 32]>
{
    let mut ordered = Vec::new();
    ordered.try_reserve_exact(resources.len()).map_err(|_| {
                                                   Error::AllocationFailed("cannot allocate \
                                                                            application identity \
                                                                            resource index")
                                               })?;
    ordered.extend(resources.iter());
    ordered.sort_unstable_by_key(|resource| (resource.kind, resource.id));
    let mut hash = Sha256::new();
    hash.update(b"System RISC application identity v1\0");
    hash.update((ordered.len() as u64).to_be_bytes());
    let mut previous = None;
    for resource in ordered
    {
        let key = (resource.kind, resource.id);
        if previous == Some(key)
        {
            return Err(Error::Malformed("duplicate resource in application authority identity"));
        }
        previous = Some(key);
        hash.update(resource.kind);
        hash.update(resource.id.to_be_bytes());
        hash.update([resource.attributes, u8::from(resource.name.is_some())]);
        let name = resource.name.as_deref().unwrap_or_default();
        hash.update((name.len() as u64).to_be_bytes());
        hash.update(name);
        hash.update((resource.data.len() as u64).to_be_bytes());
        hash.update(&resource.data);
    }
    Ok(hash.finalize().into())
}

pub fn encode(grants: &[([u8; 32], u64)]) -> Result<Vec<u8>>
{
    let length = grants.len()
                       .checked_mul(40)
                       .and_then(|n| n.checked_add(12))
                       .ok_or(Error::Malformed("application authority policy length overflow"))?;
    let count = u32::try_from(grants.len()).map_err(|_| {
                                               Error::Malformed("too many application authority \
                                                                 grants")
                                           })?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length)
         .map_err(|_| Error::AllocationFailed("cannot allocate application authority policy"))?;
    bytes.extend_from_slice(b"ACAP");
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&count.to_be_bytes());
    for (digest, capabilities) in grants
    {
        if *capabilities == 0 || capabilities & !INSPECT_DOMAINS != 0
        {
            return Err(Error::Malformed("unknown or empty application authority grant"));
        }
        bytes.extend_from_slice(digest);
        bytes.extend_from_slice(&capabilities.to_be_bytes());
    }
    Ok(bytes)
}

/// Validates the entire policy before returning a grant; a matching early entry
/// cannot hide a truncated or unknown later record. Lookup allocates nothing.
pub fn granted(bytes: &[u8], identity: [u8; 32]) -> Result<u64>
{
    if bytes.len() < 12
    {
        return Err(Error::Malformed("truncated application authority policy header"));
    }
    if bytes.get(..4) != Some(b"ACAP") || crate::be32(bytes, 4)? != 1
    {
        return Err(Error::Malformed("incompatible application authority policy; rebuild System"));
    }
    let count = crate::be32(bytes, 8)? as usize;
    let expected = count.checked_mul(40)
                        .and_then(|n| n.checked_add(12))
                        .ok_or(Error::Malformed("application authority policy length overflow"))?;
    if bytes.len() != expected
    {
        return Err(Error::Malformed("truncated or trailing application authority policy data"));
    }
    let mut result = 0;
    for record in bytes[12..].chunks_exact(40)
    {
        let capabilities = u64::from_be_bytes(record[32..].try_into().unwrap());
        if capabilities == 0 || capabilities & !INSPECT_DOMAINS != 0
        {
            return Err(Error::Malformed("unknown or empty application authority grant"));
        }
        if record[..32] == identity
        {
            result |= capabilities;
        }
    }
    Ok(result)
}
