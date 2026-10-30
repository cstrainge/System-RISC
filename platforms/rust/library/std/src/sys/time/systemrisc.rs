//! Native clocks retain signed Unix wall time and monotonic nanoseconds.
use crate::{sys::pal::runtime, time::Duration};

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Instant(Duration);

impl Instant
{
    pub fn now() -> Self
    {
        match runtime::client().monotonic_nanoseconds()
        {
            Ok(nanoseconds) => Self(Duration::from_nanos(nanoseconds)),
            Err(cause) => runtime::failure(cause.status(), b"monotonic clock refused")
        }
    }

    pub fn checked_sub_instant(&self, other: &Self) -> Option<Duration>
    {
        self.0.checked_sub(other.0)
    }

    pub fn checked_add_duration(&self, duration: &Duration) -> Option<Self>
    {
        self.0.checked_add(*duration).map(Self)
    }

    pub fn checked_sub_duration(&self, duration: &Duration) -> Option<Self>
    {
        self.0.checked_sub(*duration).map(Self)
    }
}

// Internal nanoseconds avoid sign errors around the Unix epoch. Bound results
// to the native signed-seconds contract; i128 is calculation storage, not a
// wider public clock or a wire type. Duration's u64 seconds always fit in i128.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct SystemTime(i128);

const NANOS_PER_SECOND: i128 = 1_000_000_000;
pub const UNIX_EPOCH: SystemTime = SystemTime(0);

impl SystemTime
{
    pub const MAX: Self = Self(i64::MAX as i128 * NANOS_PER_SECOND + NANOS_PER_SECOND - 1);
    pub const MIN: Self = Self(i64::MIN as i128 * NANOS_PER_SECOND);

    pub fn now() -> Self
    {
        match runtime::client().now()
        {
            Ok(timestamp) =>
            {
                Self(timestamp.unix_seconds() as i128 * NANOS_PER_SECOND
                     + timestamp.subsec_microseconds() as i128 * 1000)
            }
            Err(cause) => runtime::failure(cause.status(), b"wall clock refused")
        }
    }

    pub fn sub_time(&self, other: &Self) -> Result<Duration, Duration>
    {
        let difference = self.0 - other.0;
        let magnitude = difference.unsigned_abs();
        // The entire native range spans at most u64::MAX seconds + a subsecond.
        let duration = Duration::new((magnitude / NANOS_PER_SECOND as u128) as u64,
                                     (magnitude % NANOS_PER_SECOND as u128) as u32);
        if difference >= 0 { Ok(duration) } else { Err(duration) }
    }

    pub fn checked_add_duration(&self, duration: &Duration) -> Option<Self>
    {
        Self::bounded(self.0 + duration.as_nanos() as i128)
    }

    pub fn checked_sub_duration(&self, duration: &Duration) -> Option<Self>
    {
        Self::bounded(self.0 - duration.as_nanos() as i128)
    }

    fn bounded(value: i128) -> Option<Self>
    {
        (Self::MIN.0..=Self::MAX.0).contains(&value).then_some(Self(value))
    }
}
