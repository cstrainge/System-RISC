
use reedline::Color;

/**
 * The color mode of the terminal we're running in.
 */
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtyColorMode
{
    /**
     * The terminal is a TTY but only supports monochrome output.
     */
    TtyMonochrome,

    /**
     * The terminal supports the basic set of ANSI colors.
     */
    TtyBasic,

    /**
     * The terminal is a TTY and supports 256 colors.
     */
    Tty256,

    /**
     * The terminal is a TTY and supports true color (24-bit).
     */
    TtyTrueColor
}


impl TtyColorMode
{
    // The native Shelly enum uses the same names and ordering as the runtime mode.
    pub const VARIANT_NAMES: [&'static str; 4] = [
            "TtyMonochrome", "TtyBasic", "Tty256", "TtyTrueColor",
        ];

    pub fn variant_index(self) -> usize
    {
        match self
        {
            Self::TtyMonochrome => 0,
            Self::TtyBasic => 1,
            Self::Tty256 => 2,
            Self::TtyTrueColor => 3,
        }
    }
}


pub(crate) fn basic_color(index: u8) -> Color
{
    match index
    {
        0 => Color::Black, 1 => Color::Red, 2 => Color::Green, 3 => Color::Yellow,
        4 => Color::Blue, 5 => Color::Purple, 6 => Color::Cyan, 7 => Color::White,
        8 => Color::DarkGray, 9 => Color::LightRed, 10 => Color::LightGreen,
        11 => Color::LightYellow, 12 => Color::LightBlue, 13 => Color::LightPurple,
        14 => Color::LightCyan, _ => Color::LightGray,
    }
}


fn palette_rgb(index: u8) -> (u8, u8, u8)
{
    const BASIC: [(u8, u8, u8); 16] = [
            (0, 0, 0), (128, 0, 0), (0, 128, 0), (128, 128, 0),
            (0, 0, 128), (128, 0, 128), (0, 128, 128), (192, 192, 192),
            (128, 128, 128), (255, 0, 0), (0, 255, 0), (255, 255, 0),
            (0, 0, 255), (255, 0, 255), (0, 255, 255), (255, 255, 255),
        ];
    if index < 16 { return BASIC[index as usize]; }
    if index >= 232 { let gray = 8 + (index - 232) * 10; return (gray, gray, gray); }
    let index = index - 16;
    let levels = [0, 95, 135, 175, 215, 255];
    (levels[(index / 36) as usize], levels[(index / 6 % 6) as usize], levels[(index % 6) as usize])
}


// Equal distances choose the lowest palette index. Highlighting can preserve chroma;
// ordinary colored text uses the closest entry even when that entry is a neutral color.
fn nearest_index(wanted: (u8, u8, u8), count: u16, preserve_chroma: bool) -> u8
{
    let chromatic = wanted.0.max(wanted.1).max(wanted.2)
        - wanted.0.min(wanted.1).min(wanted.2) > 32;
    (0..count).filter(|index|
        !preserve_chroma || count != 16 || !chromatic || !matches!(*index, 0 | 7 | 8 | 15))
        .min_by_key(|index|
            {
                let candidate = palette_rgb(*index as u8);
                let delta = |left: u8, right: u8| (left as i32 - right as i32).pow(2);
                delta(wanted.0, candidate.0) + delta(wanted.1, candidate.1)
                    + delta(wanted.2, candidate.2)
            }).unwrap() as u8
}


pub(crate) fn nearest_color(color: Color, count: u16) -> Color
{
    let wanted = match color
        {
            Color::Rgb(red, green, blue) => (red, green, blue),
            Color::Fixed(index) => palette_rgb(index),
            color => return color,
        };
    let index = nearest_index(wanted, count, true);
    if count == 16 { basic_color(index) } else { Color::Fixed(index) }
}


pub(crate) fn adapt_color(color: Color, mode: TtyColorMode) -> Option<Color>
{
    match mode
    {
        TtyColorMode::TtyMonochrome => None,
        TtyColorMode::TtyBasic => Some(nearest_color(color, 16)),
        TtyColorMode::Tty256 => Some(nearest_color(color, 256)),
        TtyColorMode::TtyTrueColor => Some(color),
    }
}


pub(crate) fn color_text(rgb: (u8, u8, u8), text: &str, mode: TtyColorMode,
                        background: bool) -> String
{
    if text.is_empty() || mode == TtyColorMode::TtyMonochrome { return text.to_string(); }
    let channel = if background { 48 } else { 38 };
    let color = match mode
        {
            TtyColorMode::TtyTrueColor => format!("{};2;{};{};{}", channel, rgb.0, rgb.1, rgb.2),
            TtyColorMode::Tty256 => format!("{};5;{}", channel, nearest_index(rgb, 256, false)),
            TtyColorMode::TtyBasic =>
                {
                    let index = nearest_index(rgb, 16, false);
                    let base = if background { 40 } else { 30 };
                    let code = if index < 8 { base + index } else { base + 60 + index - 8 };
                    code.to_string()
                },
            TtyColorMode::TtyMonochrome => unreachable!(),
        };
    // Reset only the selected color channel, preserving unrelated styles.
    let reset = if background { 49 } else { 39 };
    format!("\x1b[{}m{}\x1b[{}m", color, text, reset)
}


pub(crate) fn style_text(text: &str, code: u8, reset: u8, enabled: bool) -> String
{
    if text.is_empty() || !enabled { return text.to_string(); }
    format!("\x1b[{}m{}\x1b[{}m", code, text, reset)
}
