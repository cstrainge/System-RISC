//! Optional, small metadata overrides for host objects. Ordinary host bytes are
//! never wrapped merely to save Finder state. Identity is supplied by the host
//! contract; names and source modification dates are not identity substitutes.
use mac_filesystem_interface::{MAX_PATCH_BYTES, MetadataPatch};
use sha2::{Digest, Sha256};

use crate::{Error, Result};
pub const DIRECTORY: &str = ".system-risc-metadata";
pub const MAGIC: &[u8; 8] = b"RSMETA01";
pub const VERSION: u32 = 1;
pub const BYTES: usize = 512;
const PATCH: usize = 96;
const HASH: usize = 480;
const _: () = assert!(PATCH + MAX_PATCH_BYTES <= HASH);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Annotation<'a>
{
    pub volume:     [u8; 16],
    pub object:     [u8; 32],
    /// Persistent catalog identity, not a live manager token.
    pub catalog_id: u32,
    /// Metadata edits since the physical base. Zero is an identity-only
    /// binding and must carry an empty patch, so adoption changes no revision.
    pub revision:   u64,
    pub patch:      MetadataPatch<'a>
}
impl<'a> Annotation<'a>
{
    /// Apply selected overrides without changing the stored name or either
    /// fork. Allocation and arithmetic finish before the metadata is mutated.
    pub fn apply(&self, metadata: &mut crate::Metadata) -> Result<()>
    {
        use alloc::string::String;

        use mac_filesystem_interface::FileProperties;
        self.validate()?;
        if self.volume != metadata.volume
        {
            return Err(Error::Corrupt);
        }
        let value = self.patch
                        .apply(FileProperties { name:        &metadata.name,
                                                comment:     &metadata.comment,
                                                created:     metadata.created,
                                                modified:    metadata.modified,
                                                backup:      metadata.backup,
                                                attributes:  metadata.attributes,
                                                finder_info: metadata.finder_info })
                        .map_err(|_| Error::Corrupt)?;
        let mut comment = String::new();
        comment.try_reserve_exact(value.comment.len()).map_err(|_| Error::NoMemory)?;
        comment.push_str(value.comment);
        let revision = metadata.revision.checked_add(self.revision).ok_or(Error::Range)?;
        let identity = if metadata.kind == crate::Kind::Root
        {
            if self.catalog_id != 2
            {
                return Err(Error::Corrupt);
            }
            metadata.volume
        }
        else
        {
            crate::catalog_identity(metadata.volume, self.catalog_id)?
        };
        metadata.created = value.created;
        metadata.modified = value.modified;
        metadata.backup = value.backup;
        metadata.attributes = value.attributes;
        metadata.finder_info = value.finder_info;
        metadata.comment = comment;
        metadata.catalog_id = self.catalog_id;
        metadata.identity = identity;
        metadata.revision = revision;
        Ok(())
    }

    pub fn validate(&self) -> Result<()>
    {
        if self.volume == [0; 16]
           || self.object == [0; 32]
           || self.catalog_id != 2 && self.catalog_id < 16
           || self.catalog_id >= 0x8000_0000
           || self.revision == 0 && self.patch != MetadataPatch::default()
        {
            return Err(Error::Invalid);
        }
        self.patch.validate().map_err(|_| Error::Invalid)
    }

    pub fn encode(&self, output: &mut [u8; BYTES]) -> Result<()>
    {
        self.validate()?;
        // Fixed stack scratch makes rejected requests leave output untouched.
        let mut bytes = [0; BYTES];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..12].copy_from_slice(&VERSION.to_le_bytes());
        bytes[12..16].copy_from_slice(&(BYTES as u32).to_le_bytes());
        bytes[16..32].copy_from_slice(&self.volume);
        bytes[32..64].copy_from_slice(&self.object);
        bytes[64..68].copy_from_slice(&self.catalog_id.to_le_bytes());
        let count = self.patch.encode(&mut bytes[PATCH..HASH]).map_err(|_| Error::Invalid)?;
        bytes[68..70].copy_from_slice(&(count as u16).to_le_bytes());
        bytes[72..80].copy_from_slice(&self.revision.to_le_bytes());
        let digest = Sha256::digest(&bytes[..HASH]);
        bytes[HASH..].copy_from_slice(&digest);
        *output = bytes;
        Ok(())
    }

    pub fn decode(bytes: &'a [u8; BYTES]) -> Result<Self>
    {
        if &bytes[..8] != MAGIC
        {
            return Err(Error::Corrupt);
        }
        if u32::from_le_bytes(bytes[8..12].try_into().unwrap()) != VERSION
        {
            return Err(Error::Version);
        }
        if u32::from_le_bytes(bytes[12..16].try_into().unwrap()) != BYTES as u32
           || bytes[70..72] != [0; 2]
           || bytes[80..PATCH].iter().any(|b| *b != 0)
           || Sha256::digest(&bytes[..HASH]).as_slice() != &bytes[HASH..]
        {
            return Err(Error::Corrupt);
        }
        let length = u16::from_le_bytes(bytes[68..70].try_into().unwrap()) as usize;
        if length > MAX_PATCH_BYTES || bytes[PATCH + length..HASH].iter().any(|b| *b != 0)
        {
            return Err(Error::Corrupt);
        }
        let value =
            Self { volume:     bytes[16..32].try_into().unwrap(),
                   object:     bytes[32..64].try_into().unwrap(),
                   catalog_id: u32::from_le_bytes(bytes[64..68].try_into().unwrap()),
                   revision:   u64::from_le_bytes(bytes[72..80].try_into().unwrap()),
                   patch:
                       MetadataPatch::decode(&bytes[PATCH..PATCH + length]).map_err(|_| {
                                                                               Error::Corrupt
                                                                           })? };
        value.validate().map_err(|_| Error::Corrupt)?;
        Ok(value)
    }
}
/// Portable key component; the metadata directory is provider bookkeeping.
pub fn key(object: &[u8; 32]) -> [u8; 64]
{
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0; 64];
    for (i, byte) in object.iter().enumerate()
    {
        bytes[2 * i] = HEX[(byte >> 4) as usize];
        bytes[2 * i + 1] = HEX[(byte & 15) as usize];
    }
    bytes
}
