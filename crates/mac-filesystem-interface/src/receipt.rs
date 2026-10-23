//! Actual published file and volume metadata. Receipt storage is reserved before
//! committing so reporting success never depends on a later heap allocation.
use super::*;
pub const WRITE_RECEIPT_BYTES: usize = 16;
pub const MAX_WRITE_RECEIPT_BYTES: usize =
    WRITE_RECEIPT_BYTES + MAX_METADATA_BYTES + MAX_MOUNT_BYTES;
pub const MAX_ABORT_RECEIPT_BYTES: usize = 4 + MAX_WRITE_RECEIPT_BYTES;
/// Cancellation reports the actual publication, rather than losing its
/// metadata behind a boolean if cleanup followed a successful host commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbortReceipt<'a>(pub Option<WriteReceipt<'a>>);
impl<'a> AbortReceipt<'a>
{
    pub fn encode(&self, output: &mut [u8]) -> Result<usize, Error>
    {
        self.encode_inner(output, false)
    }

    fn encode_inner(&self, output: &mut [u8], allow_root: bool) -> Result<usize, Error>
    {
        let count = if let Some(receipt) = self.0
        {
            receipt.validate(allow_root)?;
            receipt.encoded_len() + 4
        }
        else
        {
            4
        };
        let out = output.get_mut(..count).ok_or(Error::Length)?;
        if let Some(receipt) = self.0
        {
            receipt.encode_inner(&mut out[4..], allow_root)?;
        }
        put32(out, 0, u32::from(self.0.is_some()));
        Ok(count)
    }

    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error>
    {
        Self::decode_inner(bytes, false)
    }

    fn decode_inner(bytes: &'a [u8], allow_root: bool) -> Result<Self, Error>
    {
        if bytes.len() < 4
        {
            return Err(Error::Length);
        }
        match get32(bytes, 0)
        {
            0 if bytes.len() == 4 => Ok(Self(None)),
            1 => Ok(Self(Some(WriteReceipt::decode_inner(&bytes[4..], allow_root)?))),
            _ => Err(Error::Invalid)
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteReceipt<'a>
{
    pub file:   Metadata<'a>,
    pub volume: Mount<'a>
}
impl<'a> WriteReceipt<'a>
{
    fn validate(&self, allow_root: bool) -> Result<(), Error>
    {
        self.file.validate()?;
        self.volume.validate()?;
        // A transaction may publish a file or folder, never the volume root.
        if self.file.id == 2 && (!allow_root || self.file != self.volume.root)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    fn encoded_len(&self) -> usize
    {
        WRITE_RECEIPT_BYTES
        + METADATA_BYTES
        + self.file.name.len()
        + self.file.comment.len()
        + MOUNT_BYTES
        + METADATA_BYTES
        + self.volume.root.name.len()
        + self.volume.root.comment.len()
    }

    pub fn encode(&self, output: &mut [u8]) -> Result<usize, Error>
    {
        self.encode_inner(output, false)
    }

    fn encode_inner(&self, output: &mut [u8], allow_root: bool) -> Result<usize, Error>
    {
        self.validate(allow_root)?;
        let file = METADATA_BYTES + self.file.name.len() + self.file.comment.len();
        let volume = MOUNT_BYTES
                     + METADATA_BYTES
                     + self.volume.root.name.len()
                     + self.volume.root.comment.len();
        let count = self.encoded_len();
        let out = output.get_mut(..count).ok_or(Error::Length)?;
        out[..WRITE_RECEIPT_BYTES].fill(0);
        put16(out, 0, SCHEMA);
        put16(out, 2, WRITE_RECEIPT_BYTES as u16);
        put32(out, 4, file as u32);
        put32(out, 8, volume as u32);
        self.file.encode(&mut out[WRITE_RECEIPT_BYTES..WRITE_RECEIPT_BYTES + file])?;
        self.volume.encode(&mut out[WRITE_RECEIPT_BYTES + file..])?;
        Ok(count)
    }

    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error>
    {
        Self::decode_inner(bytes, false)
    }

    fn decode_inner(bytes: &'a [u8], allow_root: bool) -> Result<Self, Error>
    {
        if bytes.len() < WRITE_RECEIPT_BYTES || bytes.len() > MAX_WRITE_RECEIPT_BYTES
        {
            return Err(Error::Length);
        }
        if get16(bytes, 0) != SCHEMA
        {
            return Err(Error::Version);
        }
        if get16(bytes, 2) as usize != WRITE_RECEIPT_BYTES || get32(bytes, 12) != 0
        {
            return Err(Error::Invalid);
        }
        let file = get32(bytes, 4) as usize;
        let volume = get32(bytes, 8) as usize;
        let split = WRITE_RECEIPT_BYTES.checked_add(file).ok_or(Error::Length)?;
        if split.checked_add(volume) != Some(bytes.len())
        {
            return Err(Error::Length);
        }
        let value = Self { file:   Metadata::decode(bytes.get(WRITE_RECEIPT_BYTES..split)
                                                         .ok_or(Error::Length)?)?,
                           volume: Mount::decode(bytes.get(split..).ok_or(Error::Length)?)? };
        value.validate(allow_root)?;
        Ok(value)
    }
}

/// Metadata edits may target the root, provided its result exactly matches the
/// accompanying mount root. Creation/data-write receipts retain their root ban.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetadataReceipt<'a>(pub WriteReceipt<'a>);
impl<'a> MetadataReceipt<'a>
{
    pub fn encode(&self, output: &mut [u8]) -> Result<usize, Error>
    {
        self.0.encode_inner(output, true)
    }

    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error>
    {
        WriteReceipt::decode_inner(bytes, true).map(Self)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetadataAbortReceipt<'a>(pub Option<WriteReceipt<'a>>);
impl<'a> MetadataAbortReceipt<'a>
{
    pub fn encode(&self, output: &mut [u8]) -> Result<usize, Error>
    {
        AbortReceipt(self.0).encode_inner(output, true)
    }

    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error>
    {
        AbortReceipt::decode_inner(bytes, true).map(|a| Self(a.0))
    }
}

/// A removal receipt cannot refer to the now absent catalog entry. It carries
/// the actual committed flag and the resulting volume metadata instead.
pub const MAX_REMOVAL_RECEIPT_BYTES: usize = 4 + MAX_MOUNT_BYTES;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemovalReceipt<'a>
{
    pub removed: bool,
    pub volume:  Mount<'a>
}
impl<'a> RemovalReceipt<'a>
{
    pub fn encode(&self, output: &mut [u8]) -> Result<usize, Error>
    {
        if output.len() < 4
        {
            return Err(Error::Length);
        }
        let count = self.volume.encode(&mut output[4..])?;
        put32(output, 0, u32::from(self.removed));
        Ok(4 + count)
    }

    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error>
    {
        if bytes.len() < 4 || bytes.len() > MAX_REMOVAL_RECEIPT_BYTES
        {
            return Err(Error::Length);
        }
        let removed = match get32(bytes, 0)
        {
            0 => false,
            1 => true,
            _ => return Err(Error::Invalid)
        };
        Ok(Self { removed,
                  volume: Mount::decode(&bytes[4..])? })
    }
}
