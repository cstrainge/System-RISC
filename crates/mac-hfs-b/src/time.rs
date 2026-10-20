//! Precise unsigned microseconds from 1904; zero is a real epoch timestamp.
use crate::{Error, Result};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(pub u64);
impl Timestamp
{
    pub const UNIX_OFFSET: i64 = 2_082_844_800;

    pub fn from_unix(seconds: i64, microseconds: u32) -> Result<Self>
    {
        if microseconds >= 1_000_000
        {
            return Err(Error::Invalid);
        }
        let seconds = seconds.checked_add(Self::UNIX_OFFSET)
                             .and_then(|n| u64::try_from(n).ok())
                             .ok_or(Error::Invalid)?;
        seconds.checked_mul(1_000_000)
               .and_then(|n| n.checked_add(u64::from(microseconds)))
               .map(Self)
               .ok_or(Error::Invalid)
    }

    pub fn unix(self) -> (i64, u32)
    {
        ((self.0 / 1_000_000) as i64 - Self::UNIX_OFFSET, (self.0 % 1_000_000) as u32)
    }

    pub fn legacy_seconds(self) -> Result<u32>
    {
        u32::try_from(self.0 / 1_000_000).map_err(|_| Error::Invalid)
    }

    pub const fn from_legacy(seconds: u32) -> Self
    {
        Self(seconds as u64 * 1_000_000)
    }
}
