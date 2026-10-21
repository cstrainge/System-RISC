//! Logical creation metadata. Persistent IDs, source volume identities and
//! on-disk container fields remain the filesystem provider's responsibility.
use super::*;
pub const PROPERTIES_BYTES: usize = 72;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileProperties<'a>
{
    pub name:        &'a str,
    pub comment:     &'a str,
    pub created:     u64,
    pub modified:    u64,
    pub backup:      Option<u64>,
    pub attributes:  u16,
    pub finder_info: [u8; 32]
}
impl<'a> FileProperties<'a>
{
    pub fn fresh(name: &'a str, when: u64) -> Self
    {
        Self { name,
               comment: "",
               created: when,
               modified: when,
               backup: None,
               attributes: 0,
               finder_info: [0; 32] }
    }

    pub fn validate(&self) -> Result<(), Error>
    {
        name_input(self.name.as_bytes())?;
        if self.comment.len() > 255
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    pub fn encode(&self, output: &mut [u8]) -> Result<usize, Error>
    {
        self.validate()?;
        let count = PROPERTIES_BYTES + self.name.len() + self.comment.len();
        let out = output.get_mut(..count).ok_or(Error::Length)?;
        out[..PROPERTIES_BYTES].fill(0);
        put16(out, 0, SCHEMA);
        put16(out, 2, PROPERTIES_BYTES as u16);
        put16(out, 4, self.attributes);
        put16(out, 6, self.name.len() as u16);
        put16(out, 8, self.comment.len() as u16);
        put32(out, 12, u32::from(self.backup.is_some()));
        put64(out, 16, self.created);
        put64(out, 24, self.modified);
        put64(out, 32, self.backup.unwrap_or(0));
        out[40..72].copy_from_slice(&self.finder_info);
        let end = PROPERTIES_BYTES + self.name.len();
        out[PROPERTIES_BYTES..end].copy_from_slice(self.name.as_bytes());
        out[end..].copy_from_slice(self.comment.as_bytes());
        Ok(count)
    }

    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error>
    {
        if bytes.len() < PROPERTIES_BYTES || bytes.len() > PROPERTIES_BYTES + MAX_NAME_INPUT + 255
        {
            return Err(Error::Length);
        }
        if get16(bytes, 0) != SCHEMA
        {
            return Err(Error::Version);
        }
        if get16(bytes, 2) as usize != PROPERTIES_BYTES
           || get16(bytes, 10) != 0
           || get32(bytes, 12) > 1
           || get32(bytes, 12) == 0 && get64(bytes, 32) != 0
        {
            return Err(Error::Invalid);
        }
        let names = get16(bytes, 6) as usize;
        let comments = get16(bytes, 8) as usize;
        if names > MAX_NAME_INPUT
           || comments > 255
           || bytes.len() != PROPERTIES_BYTES + names + comments
        {
            return Err(Error::Length);
        }
        let value = Self { name: core::str::from_utf8(&bytes[PROPERTIES_BYTES..PROPERTIES_BYTES+names]).map_err(|_| Error::Utf8)?,
            comment: core::str::from_utf8(&bytes[PROPERTIES_BYTES+names..]).map_err(|_| Error::Utf8)?,
            created: get64(bytes,16), modified: get64(bytes,24), backup: (get32(bytes,12)!=0).then(|| get64(bytes,32)), attributes: get16(bytes,4), finder_info: bytes[40..72].try_into().map_err(|_| Error::Length)? };
        value.validate()?;
        Ok(value)
    }
}
