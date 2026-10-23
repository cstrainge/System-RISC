//! Field-selective metadata changes. Unselected dates, Finder bytes and flags
//! retain their raw values; projected type/creator defaults are never frozen by
//! an unrelated icon-position or comment update.
use super::*;
pub const PATCH_BYTES: usize = 104;
pub const MAX_PATCH_BYTES: usize = PATCH_BYTES + 255;
const COMMENT: u32 = 1;
const CREATED: u32 = 2;
const MODIFIED: u32 = 4;
const BACKUP: u32 = 8;
const BACKUP_PRESENT: u32 = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MetadataPatch<'a>
{
    pub comment:         Option<&'a str>,
    pub created:         Option<u64>,
    pub modified:        Option<u64>,
    /// `None` leaves the field alone; `Some(None)` clears the backup date.
    pub backup:          Option<Option<u64>>,
    pub attributes_mask: u16,
    pub attributes:      u16,
    pub finder_mask:     [u8; 32],
    pub finder_info:     [u8; 32]
}
impl<'a> MetadataPatch<'a>
{
    pub fn validate(&self) -> Result<(), Error>
    {
        if self.comment.is_some_and(|s| s.len() > 255)
           || self.attributes & !self.attributes_mask != 0
           || self.finder_info.iter().zip(&self.finder_mask).any(|(v, m)| v & !m != 0)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    /// Overlay only selected fields onto raw provider properties. Borrowed text
    /// keeps the caller's request/storage lifetime explicit.
    pub fn apply(self, raw: FileProperties<'a>) -> Result<FileProperties<'a>, Error>
    {
        self.validate()?;
        let mut result = raw;
        if let Some(value) = self.comment
        {
            result.comment = value;
        }
        if let Some(value) = self.created
        {
            result.created = value;
        }
        if let Some(value) = self.modified
        {
            result.modified = value;
        }
        if let Some(value) = self.backup
        {
            result.backup = value;
        }
        result.attributes = (raw.attributes & !self.attributes_mask) | self.attributes;
        for i in 0..32
        {
            result.finder_info[i] =
                (raw.finder_info[i] & !self.finder_mask[i]) | self.finder_info[i];
        }
        Ok(result)
    }

    /// Compose two explicit changes, with `new` winning only for its selected
    /// fields. This permits small metadata records independent of fork payloads.
    pub fn merged(self, new: Self) -> Result<Self, Error>
    {
        self.validate()?;
        new.validate()?;
        let mut result = Self { comment:         new.comment.or(self.comment),
                                created:         new.created.or(self.created),
                                modified:        new.modified.or(self.modified),
                                backup:          new.backup.or(self.backup),
                                attributes_mask: self.attributes_mask | new.attributes_mask,
                                attributes:      (self.attributes & !new.attributes_mask)
                                                 | new.attributes,
                                finder_mask:     [0; 32],
                                finder_info:     [0; 32] };
        for i in 0..32
        {
            result.finder_mask[i] = self.finder_mask[i] | new.finder_mask[i];
            result.finder_info[i] =
                (self.finder_info[i] & !new.finder_mask[i]) | new.finder_info[i];
        }
        Ok(result)
    }

    pub fn encode(&self, output: &mut [u8]) -> Result<usize, Error>
    {
        self.validate()?;
        let comment = self.comment.unwrap_or("");
        let length = PATCH_BYTES + comment.len();
        let out = output.get_mut(..length).ok_or(Error::Length)?;
        out.fill(0);
        let fields = u32::from(self.comment.is_some()) * COMMENT
                     | u32::from(self.created.is_some()) * CREATED
                     | u32::from(self.modified.is_some()) * MODIFIED
                     | u32::from(self.backup.is_some()) * BACKUP
                     | u32::from(self.backup.flatten().is_some()) * BACKUP_PRESENT;
        put16(out, 0, SCHEMA);
        put16(out, 2, PATCH_BYTES as u16);
        put32(out, 4, fields);
        put16(out, 8, self.attributes_mask);
        put16(out, 10, self.attributes);
        put16(out, 12, comment.len() as u16);
        put64(out, 16, self.created.unwrap_or(0));
        put64(out, 24, self.modified.unwrap_or(0));
        put64(out, 32, self.backup.flatten().unwrap_or(0));
        out[40..72].copy_from_slice(&self.finder_mask);
        out[72..104].copy_from_slice(&self.finder_info);
        out[104..].copy_from_slice(comment.as_bytes());
        Ok(length)
    }

    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error>
    {
        if !(PATCH_BYTES..=MAX_PATCH_BYTES).contains(&bytes.len())
        {
            return Err(Error::Length);
        }
        if get16(bytes, 0) != SCHEMA
        {
            return Err(Error::Version);
        }
        let fields = get32(bytes, 4);
        let comments = get16(bytes, 12) as usize;
        if get16(bytes, 2) as usize != PATCH_BYTES
           || get16(bytes, 14) != 0
           || fields & !31 != 0
           || fields & BACKUP_PRESENT != 0 && fields & BACKUP == 0
           || fields & CREATED == 0 && get64(bytes, 16) != 0
           || fields & MODIFIED == 0 && get64(bytes, 24) != 0
           || fields & BACKUP_PRESENT == 0 && get64(bytes, 32) != 0
           || fields & COMMENT == 0 && comments != 0
        {
            return Err(Error::Invalid);
        }
        if bytes.len() != PATCH_BYTES + comments
        {
            return Err(Error::Length);
        }
        let result =
            Self { comment:         if fields & COMMENT != 0
                   {
                       Some(core::str::from_utf8(&bytes[PATCH_BYTES..]).map_err(|_| Error::Utf8)?)
                   }
                   else
                   {
                       None
                   },
                   created:         (fields & CREATED != 0).then(|| get64(bytes, 16)),
                   modified:        (fields & MODIFIED != 0).then(|| get64(bytes, 24)),
                   backup:          (fields & BACKUP != 0).then(|| {
                                        (fields & BACKUP_PRESENT != 0).then(|| get64(bytes, 32))
                                    }),
                   attributes_mask: get16(bytes, 8),
                   attributes:      get16(bytes, 10),
                   finder_mask:     bytes[40..72].try_into().unwrap(),
                   finder_info:     bytes[72..104].try_into().unwrap() };
        result.validate()?;
        Ok(result)
    }
}
