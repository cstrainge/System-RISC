
use reedline::Color;

use crate::{ language::analysis::highlight::HighlightKind,
             runtime::color::{ TtyColorMode, basic_color, nearest_color } };


pub const COLOR_COUNT: usize = HighlightKind::Error as usize + 1;


// Foreground-only defaults drawn from the logo's violet, cyan, blue, and warm highlights.
pub const SYNTAX_COLORS: &[(HighlightKind, &str, &str)] = &[
        (HighlightKind::Comment, "$syntax_color_comment", "\x1b[38;2;125;137;156m"),
        (HighlightKind::Keyword, "$syntax_color_keyword", "\x1b[38;2;177;120;255m"),
        (HighlightKind::Variable, "$syntax_color_variable", "\x1b[38;2;255;205;110m"),
        (HighlightKind::Type, "$syntax_color_type", "\x1b[38;2;88;235;200m"),
        (HighlightKind::Function, "$syntax_color_function", "\x1b[38;2;110;175;255m"),
        (HighlightKind::Method, "$syntax_color_method", "\x1b[38;2;118;206;255m"),
        (HighlightKind::Builtin, "$syntax_color_builtin", "\x1b[38;2;156;157;255m"),
        (HighlightKind::Alias, "$syntax_color_alias", "\x1b[38;2;202;160;255m"),
        (HighlightKind::Command, "$syntax_color_command", "\x1b[38;2;49;216;224m"),
        (HighlightKind::Argument, "$syntax_color_argument", "\x1b[38;2;215;222;233m"),
        (HighlightKind::String, "$syntax_color_string", "\x1b[38;2;108;226;191m"),
        (HighlightKind::Number, "$syntax_color_number", "\x1b[38;2;255;205;110m"),
        (HighlightKind::Literal, "$syntax_color_literal", "\x1b[38;2;205;153;255m"),
        (HighlightKind::Operator, "$syntax_color_operator", "\x1b[38;2;86;205;225m"),
        (HighlightKind::Delimiter, "$syntax_color_delimiter", "\x1b[38;2;174;184;200m"),
        (HighlightKind::Field, "$syntax_color_field", "\x1b[38;2;151;199;255m"),
        (HighlightKind::Module, "$syntax_color_module", "\x1b[38;2;131;160;255m"),
        (HighlightKind::Format, "$syntax_color_format", "\x1b[38;2;49;216;224m"),
        (HighlightKind::Escape, "$syntax_color_escape", "\x1b[38;2;255;169;130m"),
        (HighlightKind::Error, "$syntax_color_error", "\x1b[38;2;255;112;142m"),
    ];


#[derive(Clone)]
pub struct SyntaxSettings
{
    pub enabled: bool,
    pub colors: [Option<Color>; COLOR_COUNT],
    pub mode: TtyColorMode,
}

impl SyntaxSettings
{
    pub fn new(mode: TtyColorMode) -> Self
    {
        let mut settings = Self { enabled: true, colors: [None; COLOR_COUNT], mode };
        for &(kind, _, color) in SYNTAX_COLORS
        { settings.colors[kind as usize] = foreground(color, mode).unwrap(); }
        settings
    }

    pub fn set_color(&mut self, kind: HighlightKind, value: &str)
    {
        if let Some(color) = foreground(value, self.mode)
        { self.colors[kind as usize] = color; }
    }
}


/**
 * Configuration is never copied into terminal output. Accept one foreground SGR only;
 * empty text and the default foreground intentionally disable a category's coloring.
 */
fn foreground(value: &str, mode: TtyColorMode) -> Option<Option<Color>>
{
    if value.is_empty() { return Some(None); }
    let sequence = value.strip_prefix("\x1b[")?.strip_suffix('m')?;
    let numbers: Vec<u8> = sequence.split(';').map(str::parse).collect::<Result<_, _>>().ok()?;
    let color = match numbers.as_slice()
        {
            [0] | [39] => return Some(None),
            [code @ 30..=37] => basic_color(code - 30),
            [code @ 90..=97] => basic_color(code - 90 + 8),
            [38, 5, index] => Color::Fixed(*index),
            [38, 2, red, green, blue] => Color::Rgb(*red, *green, *blue),
            _ => return None,
        };
    Some(match mode
        {
            TtyColorMode::TtyMonochrome => None,
            TtyColorMode::TtyBasic => Some(nearest_color(color, 16)),
            TtyColorMode::Tty256 => Some(match color
                {
                    Color::Rgb(..) => nearest_color(color, 256),
                    color => color,
                }),
            TtyColorMode::TtyTrueColor => Some(color),
        })
}
