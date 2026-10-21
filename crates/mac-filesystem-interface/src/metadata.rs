use super::*;
pub const METADATA_BYTES: usize = 112;
pub const MOUNT_BYTES: usize = 48;
/// Common catalog projection; no host container or on-disk filesystem layout.
/// Dates are checked unsigned microseconds from the Macintosh 1904 epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata<'a>
{
    pub id:              u32,
    pub parent:          u32,
    pub directory:       bool,
    pub data_length:     u64,
    pub resource_length: u64,
    pub created:         u64,
    pub modified:        u64,
    pub backup:          Option<u64>,
    pub child_count:     u32,
    pub attributes:      u16,
    pub finder_info:     [u8; 32],
    pub revision:        u64,
    pub name:            &'a str,
    pub comment:         &'a str
}
impl<'a> Metadata<'a>
{
    fn validate(&self) -> Result<(), Error>
    {
        name(self.name.as_bytes())?;
        if self.id < 2
           || self.id == 2 && (!self.directory || self.parent != 0)
           || self.id != 2 && self.parent < 2
           || self.id == self.parent
           || self.revision == 0
           || !self.directory && self.child_count != 0
           || self.directory && (self.data_length != 0 || self.resource_length != 0)
           || self.comment.len() > 255
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    pub fn encode(&self, output: &mut [u8]) -> Result<usize, Error>
    {
        self.validate()?;
        let count = METADATA_BYTES + self.name.len() + self.comment.len();
        let out = output.get_mut(..count).ok_or(Error::Length)?;
        out[..METADATA_BYTES].fill(0);
        put16(out, 0, SCHEMA);
        put16(out, 2, METADATA_BYTES as u16);
        put32(out, 4, self.id);
        put32(out, 8, self.parent);
        put32(out, 12, u32::from(self.directory));
        for (at, value) in [(16, self.data_length),
                            (24, self.resource_length),
                            (32, self.created),
                            (40, self.modified),
                            (48, self.backup.unwrap_or(0)),
                            (104, self.revision)]
        {
            put64(out, at, value)
        }
        put32(out, 56, u32::from(self.backup.is_some()));
        put32(out, 60, self.child_count);
        put16(out, 64, self.attributes);
        put16(out, 66, self.name.len() as u16);
        put16(out, 68, self.comment.len() as u16);
        out[72..104].copy_from_slice(&self.finder_info);
        out[METADATA_BYTES..METADATA_BYTES + self.name.len()].copy_from_slice(self.name.as_bytes());
        out[METADATA_BYTES + self.name.len()..].copy_from_slice(self.comment.as_bytes());
        Ok(count)
    }

    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error>
    {
        if bytes.len() < METADATA_BYTES || bytes.len() > METADATA_BYTES + 510
        {
            return Err(Error::Length);
        }
        if get16(bytes, 0) != SCHEMA
        {
            return Err(Error::Version);
        }
        if get16(bytes, 2) as usize != METADATA_BYTES
           || get16(bytes, 70) != 0
           || get32(bytes, 12) > 1
           || get32(bytes, 56) > 1
           || get32(bytes, 56) == 0 && get64(bytes, 48) != 0
        {
            return Err(Error::Invalid);
        }
        let n = get16(bytes, 66) as usize;
        let c = get16(bytes, 68) as usize;
        if n > 255 || c > 255 || bytes.len() != METADATA_BYTES + n + c
        {
            return Err(Error::Length);
        }
        let value = Self {
            id: get32(bytes, 4),
            parent: get32(bytes, 8),
            directory: get32(bytes, 12) != 0,
            data_length: get64(bytes, 16),
            resource_length: get64(bytes, 24),
            created: get64(bytes, 32),
            modified: get64(bytes, 40),
            backup: (get32(bytes, 56) != 0).then(|| get64(bytes, 48)),
            child_count: get32(bytes, 60),
            attributes: get16(bytes, 64),
            finder_info: bytes[72..104].try_into().unwrap(),
            revision: get64(bytes, 104),
            name: core::str::from_utf8(&bytes[METADATA_BYTES..METADATA_BYTES + n])
                .map_err(|_| Error::Utf8)?,
            comment: core::str::from_utf8(&bytes[METADATA_BYTES + n..])
                .map_err(|_| Error::Utf8)?
        };
        value.validate()?;
        Ok(value)
    }
}
/// Mount descriptor followed by its root catalog metadata. Capacity values are
/// optional: an unavailable host free-space query is not a fabricated zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mount<'a>
{
    pub capabilities:       u32,
    pub filesystem:         [u8; 4],
    pub filesystem_version: u32,
    pub capacity:           Option<(u64, u64)>,
    pub root:               Metadata<'a>
}
impl<'a> Mount<'a>
{
    fn validate(&self) -> Result<(), Error>
    {
        if self.capabilities & !ALL_CAPABILITIES != 0
           || self.capabilities & READ == 0
           || !self.filesystem.iter().all(|b| b.is_ascii_graphic() || *b == b' ')
           || self.filesystem == *b"    "
           || self.filesystem_version == 0
           || self.root.id != 2
           || !self.root.directory
           || self.root.parent != 0
           || self.capacity.is_some_and(|(total, free)| free > total)
        {
            return Err(Error::Invalid);
        }
        self.root.validate()
    }

    pub fn encode(&self, output: &mut [u8]) -> Result<usize, Error>
    {
        self.validate()?;
        let count = MOUNT_BYTES + METADATA_BYTES + self.root.name.len() + self.root.comment.len();
        let out = output.get_mut(..count).ok_or(Error::Length)?;
        // Validation and capacity checks precede all output mutations.
        self.root.encode(&mut out[MOUNT_BYTES..])?;
        out[..MOUNT_BYTES].fill(0);
        put16(out, 0, SCHEMA);
        put16(out, 2, MOUNT_BYTES as u16);
        put32(out, 4, self.capabilities);
        out[8..12].copy_from_slice(&self.filesystem);
        put32(out, 12, self.filesystem_version);
        put32(out, 16, u32::from(self.capacity.is_some()));
        let (total, free) = self.capacity.unwrap_or((0, 0));
        put64(out, 24, total);
        put64(out, 32, free);
        Ok(count)
    }

    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error>
    {
        if bytes.len() < MOUNT_BYTES
        {
            return Err(Error::Length);
        }
        if get16(bytes, 0) != SCHEMA
        {
            return Err(Error::Version);
        }
        if get16(bytes, 2) as usize != MOUNT_BYTES
           || get32(bytes, 16) > 1
           || get32(bytes, 20) != 0
           || get64(bytes, 40) != 0
           || get32(bytes, 16) == 0 && (get64(bytes, 24) != 0 || get64(bytes, 32) != 0)
        {
            return Err(Error::Invalid);
        }
        let value = Self { capabilities:       get32(bytes, 4),
                           filesystem:         bytes[8..12].try_into().unwrap(),
                           filesystem_version: get32(bytes, 12),
                           capacity:           (get32(bytes, 16) != 0).then(|| {
                                                                          (get64(bytes, 24),
                                                                           get64(bytes, 32))
                                                                      }),
                           root:               Metadata::decode(&bytes[MOUNT_BYTES..])? };
        value.validate()?;
        Ok(value)
    }
}
