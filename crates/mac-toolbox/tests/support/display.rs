//! Native capture geometry comes from the same checked platform as the guest.
fn size() -> (usize, usize)
{
    static SIZE: std::sync::OnceLock<(usize, usize)> = std::sync::OnceLock::new();
    *SIZE.get_or_init(|| mac_build::platform::current().memory().unwrap().display_dimensions())
}
pub fn width() -> usize
{
    size().0
}
pub fn height() -> usize
{
    size().1
}
pub fn header() -> Vec<u8>
{
    format!("P6\n{} {}\n255\n", width(), height()).into_bytes()
}
