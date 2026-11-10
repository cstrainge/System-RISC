//! Retained colours keep their meaning until paint, including scrollback.
// Keep each retained colour at four bytes: the high byte is its kind and
// RGB occupies only the low three bytes. Constructors keep the tag private.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Color(u32);
impl Color
{
    pub const DEFAULT: Self = Self(0x0100_0000);

    pub const fn indexed(index: u8) -> Self
    {
        Self(0x0200_0000 | index as u32)
    }

    pub const fn rgb(value: u32) -> Self
    {
        Self(value & 0x00ff_ffff)
    }
}
const _: () = assert!(core::mem::size_of::<Color>() == 4);

pub(super) const FOREGROUND: u32 = 0xc0c0c0;
pub(super) const BACKGROUND: u32 = 0;

pub(super) struct Palette
{
    pub indexed:    [u32; 256],
    pub foreground: u32,
    pub background: u32
}
impl Palette
{
    pub fn new() -> Self
    {
        Self { indexed:    std::array::from_fn(|index| indexed(index as u8)),
               foreground: FOREGROUND,
               background: BACKGROUND }
    }

    pub fn resolve(&self, color: Color, foreground: bool) -> u32
    {
        match color.0 >> 24
        {
            1 =>
            {
                if foreground
                {
                    self.foreground
                }
                else
                {
                    self.background
                }
            }
            2 => self.indexed[(color.0 & 255) as usize],
            _ => color.0
        }
    }
}
pub(super) fn rgb(r: u32, g: u32, b: u32) -> u32
{
    (r << 16) | (g << 8) | b
}
pub(super) fn indexed(index: u8) -> u32
{
    const BASIC: [u32; 16] = [0, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd,
                              0xe5e5e5, 0x7f7f7f, 0xff0000, 0x00ff00, 0xffff00, 0x5c5cff,
                              0xff00ff, 0x00ffff, 0xffffff];
    match index
    {
        0..=15 => BASIC[index as usize],
        16..=231 =>
        {
            let n = u32::from(index - 16);
            let level = |v| if v == 0 { 0 } else { 55 + 40 * v };
            rgb(level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        _ =>
        {
            let level = 8 + 10 * u32::from(index - 232);
            rgb(level, level, level)
        }
    }
}
/// Numeric X-style colour specifications: #RGB through #RRRRGGGGBBBB,
/// or rgb:r/g/b with one to four hexadecimal digits per component.
/// Unsupported names and malformed specifications leave the palette unchanged.
pub(super) fn parse(bytes: &[u8]) -> Option<u32>
{
    fn component(bytes: &[u8]) -> Option<u32>
    {
        if bytes.is_empty() || bytes.len() > 4
        {
            return None;
        }
        let mut value = 0u32;
        for &byte in bytes
        {
            let digit = match byte
            {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return None
            };
            value = value * 16 + u32::from(digit);
        }
        Some(value * 255 / ((1u32 << (4 * bytes.len())) - 1))
    }
    let (r, g, b) = if let Some(hex) = bytes.strip_prefix(b"#")
    {
        if !matches!(hex.len(), 3 | 6 | 9 | 12)
        {
            return None;
        }
        let n = hex.len() / 3;
        (component(&hex[..n])?, component(&hex[n..2 * n])?, component(&hex[2 * n..])?)
    }
    else if let Some(spec) = bytes.strip_prefix(b"rgb:")
    {
        let mut parts = spec.split(|byte| *byte == b'/');
        let channels =
            (component(parts.next()?)?, component(parts.next()?)?, component(parts.next()?)?);
        if parts.next().is_some()
        {
            return None;
        }
        channels
    }
    else
    {
        return None;
    };
    Some(rgb(r, g, b))
}
