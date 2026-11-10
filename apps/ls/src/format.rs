//! Display formatting: permission strings, sizes, dates.
//!
//! Pure functions over std types; no I/O here.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::native_fs::Metadata;

/// Native catalog flags: directory/file, locked, APPL/CLAP application.
/// System RISC does not expose Unix owner/group/mode permissions.
pub fn attributes(md: &Metadata) -> String
{
    format!("{}{}{}",
            if md.is_dir() { 'd' } else { 'f' },
            if md.is_locked() { 'L' } else { '-' },
            if md.is_application() { 'A' } else { '-' })
}

/// `1536` as `1.5K`, `1048576` as `1.0M`; sizes under 1K print bare.
pub fn human_size(size: u64) -> String
{
    const UNITS: [&str; 5] = ["", "K", "M", "G", "T"];
    if size < 1024
    {
        return size.to_string();
    }
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1
    {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0
    {
        format!("{value:.1}{}", UNITS[unit])
    }
    else
    {
        format!("{value:.0}{}", UNITS[unit])
    }
}

const MONTHS: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// `Jan  2 15:04` for recent times, `Jan  2  2026` for older ones:
/// times within the last six months (and not in the future) show the
/// time of day, everything else shows the year.
pub fn mtime(md: &Metadata) -> String
{
    let time = md.modified().unwrap_or(UNIX_EPOCH);
    let recent = match SystemTime::now().duration_since(time)
    {
        Ok(age) => age < Duration::from_secs(6 * 30 * 24 * 60 * 60),
        Err(_) => false
    };
    let (year, month, day, hour, minute) = civil(time);
    if recent
    {
        format!("{} {:2} {:02}:{:02}", MONTHS[month - 1], day, hour, minute)
    }
    else
    {
        format!("{} {:2}  {}", MONTHS[month - 1], day, year)
    }
}

/// Split a `SystemTime` into (year, month, day, hour, minute) in UTC.
/// Howard Hinnant's civil-from-days algorithm; std has no date formatting.
fn civil(time: SystemTime) -> (i64, usize, u32, u32, u32)
{
    let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let days = secs.div_euclid(86_400);
    let day_secs = secs.rem_euclid(86_400);
    let hour = (day_secs / 3600) as u32;
    let minute = ((day_secs % 3600) / 60) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as usize;
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day, hour, minute)
}

/// `-F` indicator: `/` directory, `*` native APPL/CLAP application.
pub fn indicator(md: &Metadata) -> &'static str
{
    if md.is_dir()
    {
        "/"
    }
    else if md.is_application()
    {
        "*"
    }
    else
    {
        ""
    }
}
