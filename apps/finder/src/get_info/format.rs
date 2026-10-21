//! Checked, allocation-free presentation helpers. English names come from resources.
use mac_toolbox::Timestamp;

pub fn grouped(value: u64) -> Grouped
{
    let mut bytes = [0u8; 26];
    let mut cursor = bytes.len();
    let mut value = value;
    let mut digits = 0usize;
    loop
    {
        if digits != 0 && digits.is_multiple_of(3)
        {
            cursor -= 1;
            bytes[cursor] = b',';
        }
        cursor -= 1;
        bytes[cursor] = b'0' + (value % 10) as u8;
        digits += 1;
        value /= 10;
        if value == 0
        {
            break;
        }
    }
    Grouped { bytes,
              start: cursor }
}

pub struct Grouped
{
    bytes: [u8; 26],
    start: usize
}
impl Grouped
{
    pub fn as_str(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[self.start..]).unwrap_or("")
    }
}

/// Proleptic Gregorian calendar, without a 32-bit signed time_t boundary.
pub fn calendar(timestamp: Timestamp) -> Calendar
{
    let seconds = timestamp.unix_seconds();
    let days = seconds.div_euclid(86400);
    let day_seconds = seconds.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    Calendar { year:    year + i64::from(month <= 2),
               month:   month as u8,
               day:     day as u8,
               weekday: (days + 4).rem_euclid(7) as u8,
               hour:    (day_seconds / 3600) as u8,
               minute:  (day_seconds / 60 % 60) as u8 }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Calendar
{
    pub year:    i64,
    pub month:   u8,
    pub day:     u8,
    pub weekday: u8,
    pub hour:    u8,
    pub minute:  u8
}
