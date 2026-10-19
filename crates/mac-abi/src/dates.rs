//! Native date value. Legacy HFS/MacBinary seconds are unsigned, from 1904.
//! Time zones/calendar presentation do not change an absolute timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Timestamp(i64);
impl Timestamp
{
    pub const MAC_TO_UNIX_SECONDS: i64 = 2_082_844_800;

    pub const fn from_unix_seconds(seconds: i64) -> Self
    {
        Self(seconds)
    }

    pub const fn unix_seconds(self) -> i64
    {
        self.0
    }

    pub const fn from_mac_seconds(seconds: u32) -> Self
    {
        Self(seconds as i64 - Self::MAC_TO_UNIX_SECONDS)
    }

    /// Classic's last second is 2040-02-06 06:28:15 UTC. Out-of-range
    /// dates fail explicitly; native timestamps continue beyond that boundary.
    pub fn mac_seconds(self) -> Option<u32>
    {
        u32::try_from(self.0.checked_add(Self::MAC_TO_UNIX_SECONDS)?).ok()
    }
}
