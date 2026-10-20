//! Native date value. Legacy HFS/MacBinary seconds are unsigned, from 1904.
//! Time zones/calendar presentation do not change an absolute timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp
{
    seconds:      i64,
    microseconds: u32
}
impl Timestamp
{
    pub const MAC_TO_UNIX_SECONDS: i64 = 2_082_844_800;

    pub const fn from_unix_seconds(seconds: i64) -> Self
    {
        Self { seconds,
               microseconds: 0 }
    }

    pub const fn unix_seconds(self) -> i64
    {
        self.seconds
    }

    pub const fn from_mac_seconds(seconds: u32) -> Self
    {
        Self { seconds:      seconds as i64 - Self::MAC_TO_UNIX_SECONDS,
               microseconds: 0 }
    }

    pub const fn from_unix_parts(seconds: i64, microseconds: u32) -> Option<Self>
    {
        if microseconds < 1_000_000
        {
            Some(Self { seconds,
                        microseconds })
        }
        else
        {
            None
        }
    }

    pub const fn subsec_microseconds(self) -> u32
    {
        self.microseconds
    }

    pub const fn from_mac_microseconds(value: u64) -> Self
    {
        Self { seconds:      (value / 1_000_000) as i64 - Self::MAC_TO_UNIX_SECONDS,
               microseconds: (value % 1_000_000) as u32 }
    }

    pub fn mac_microseconds(self) -> Option<u64>
    {
        u64::try_from(self.seconds.checked_add(Self::MAC_TO_UNIX_SECONDS)?).ok()?
            .checked_mul(1_000_000)?.checked_add(u64::from(self.microseconds))
    }

    /// Classic's last second is 2040-02-06 06:28:15 UTC. Out-of-range
    /// dates fail explicitly; native timestamps continue beyond that boundary.
    pub fn mac_seconds(self) -> Option<u32>
    {
        u32::try_from(self.seconds.checked_add(Self::MAC_TO_UNIX_SECONDS)?).ok()
    }
}

/// Precise wall-clock transport; native seconds remain signed for std conversion.
#[derive(Default, Clone, Copy)]
#[repr(C)]
pub struct ClockCall
{
    pub unix_seconds: i64,
    pub microseconds: u32,
    pub reserved:     u32
}
