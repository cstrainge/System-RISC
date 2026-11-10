//! Decode terminal output into operations on the retained view.
use std::collections::VecDeque;

use mac_toolbox::Error;
use vte::{Params, Parser, Perform};

use crate::{color::{self, Color, rgb},
            input,
            screen::{Screen, Style},
            sixel};

pub struct Stream
{
    parser:  Parser,
    style:   Style,
    default: Style,
    image:   Option<Vec<u8>>
}
impl Stream
{
    pub fn new(default: Style) -> Self
    {
        Self { parser: Parser::new(),
               style: default,
               default,
               image: None }
    }

    pub fn advance(&mut self, bytes: &[u8], screen: &mut Screen, replies: &mut VecDeque<u8>)
                   -> Result<(), Error>
    {
        let mut failure = None;
        let mut target = Target { screen,
                                  replies,
                                  style: &mut self.style,
                                  default: self.default,
                                  image: &mut self.image,
                                  failure: &mut failure };
        self.parser.advance(&mut target, bytes);
        failure.map_or(Ok(()), Err)
    }
}

struct Target<'a>
{
    screen:  &'a mut Screen,
    replies: &'a mut VecDeque<u8>,
    style:   &'a mut Style,
    default: Style,
    image:   &'a mut Option<Vec<u8>>,
    failure: &'a mut Option<Error>
}
impl Target<'_>
{
    fn reply(&mut self, bytes: &[u8])
    {
        if self.failure.is_none()
           && let Err(cause) = input::queue(self.replies, bytes)
        {
            *self.failure = Some(cause);
        }
    }

    fn sgr(&mut self, params: &Params)
    {
        let mut values = params.iter().peekable();
        if values.peek().is_none()
        {
            *self.style = self.default;
        }
        while let Some(group) = values.next()
        {
            let code = group[0];
            match code
            {
                0 => *self.style = self.default,
                1 => self.style.attrs |= Style::BOLD,
                2 => self.style.attrs |= Style::FAINT,
                3 => self.style.attrs |= Style::ITALIC,
                4 =>
                {
                    match group.get(1).copied().unwrap_or(1)
                    {
                        0 => self.style.attrs &= !(Style::UNDERLINE | Style::DOUBLE_UNDERLINE),
                        1 =>
                        {
                            self.style.attrs &= !Style::DOUBLE_UNDERLINE;
                            self.style.attrs |= Style::UNDERLINE;
                        }
                        2 =>
                        {
                            self.style.attrs &= !Style::UNDERLINE;
                            self.style.attrs |= Style::DOUBLE_UNDERLINE;
                        }
                        _ => ()
                    }
                }
                8 => self.style.attrs |= Style::HIDDEN,
                9 => self.style.attrs |= Style::STRIKE,
                21 =>
                {
                    self.style.attrs &= !Style::UNDERLINE;
                    self.style.attrs |= Style::DOUBLE_UNDERLINE;
                }
                22 => self.style.attrs &= !(Style::BOLD | Style::FAINT),
                23 => self.style.attrs &= !Style::ITALIC,
                24 => self.style.attrs &= !(Style::UNDERLINE | Style::DOUBLE_UNDERLINE),
                28 => self.style.attrs &= !Style::HIDDEN,
                29 => self.style.attrs &= !Style::STRIKE,
                7 | 27 => self.style.reverse = code == 7,
                30..=37 | 90..=97 =>
                {
                    self.style.fg =
                        Color::indexed((if code < 40 { code - 30 } else { code - 82 }) as u8)
                }
                40..=47 | 100..=107 =>
                {
                    self.style.bg =
                        Color::indexed((if code < 50 { code - 40 } else { code - 92 }) as u8)
                }
                39 => self.style.fg = self.default.fg,
                49 => self.style.bg = self.default.bg,
                38 | 48 =>
                {
                    let mut inline = group[1..].iter().copied();
                    let mode = inline.next().or_else(|| values.next().map(|g| g[0]));
                    let mut next = || inline.next().or_else(|| values.next().map(|g| g[0]));
                    let color = match mode
                    {
                        Some(5) =>
                        {
                            next().filter(|v| *v <= 255).map(|value| Color::indexed(value as u8))
                        }
                        Some(2) =>
                        {
                            if group.len() == 6
                            {
                                let _ = next();
                            }
                            match (next(), next(), next())
                            {
                                (Some(r), Some(g), Some(b)) if r <= 255 && g <= 255 && b <= 255 =>
                                {
                                    Some(Color::rgb(rgb(r as u32, g as u32, b as u32)))
                                }
                                _ => None
                            }
                        }
                        _ => None
                    };
                    if let Some(color) = color
                    {
                        if code == 38
                        {
                            self.style.fg = color;
                        }
                        else
                        {
                            self.style.bg = color;
                        }
                    }
                }
                _ => ()
            }
        }
    }
}

impl Perform for Target<'_>
{
    fn print(&mut self, ch: char)
    {
        self.screen.put(ch, *self.style);
    }

    fn execute(&mut self, byte: u8)
    {
        let (x, y) = self.screen.position();
        match byte
        {
            b'\r' => self.screen.place(0, y),
            b'\n' | 11 | 12 =>
            {
                // The pipe transport has no termios ONLCR processing.
                self.screen.place(0, y);
                self.screen.index(false, *self.style);
            }
            8 => self.screen.shift(-1, 0),
            b'\t' => self.screen.place((x / 8 + 1) * 8, y),
            _ => ()
        }
    }

    fn esc_dispatch(&mut self, intermediate: &[u8], ignore: bool, action: u8)
    {
        if ignore || !intermediate.is_empty()
        {
            return;
        }
        match action
        {
            b'7' => self.screen.save(),
            b'8' => self.screen.restore(),
            b'D' => self.screen.index(false, *self.style),
            b'M' => self.screen.index(true, *self.style),
            b'E' =>
            {
                self.execute(b'\n');
            }
            _ => ()
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediate: &[u8], ignore: bool, action: char)
    {
        if ignore
        {
            return;
        }
        if intermediate == b"?" && matches!(action, 'h' | 'l')
        {
            let enabled = action == 'h';
            for group in params
            {
                match group[0]
                {
                    1 => self.screen.application_keys = enabled,
                    7 => self.screen.autowrap = enabled,
                    25 => self.screen.set_cursor_visible(enabled),
                    47 | 1047 | 1049 => self.screen.switch(enabled),
                    1048 if enabled => self.screen.save(),
                    1048 => self.screen.restore(),
                    _ => ()
                }
            }
            return;
        }
        if !intermediate.is_empty()
        {
            return;
        }
        let mut p = params.iter().map(|g| g[0]);
        let first = p.next().unwrap_or(0);
        let n = usize::from(first.max(1));
        let d = n as isize;
        let (x, y) = self.screen.position();
        let (cols, rows) = self.screen.size();
        let style = *self.style;
        match action
        {
            'A' => self.screen.shift(0, -d),
            'B' | 'e' => self.screen.shift(0, d),
            'C' | 'a' => self.screen.shift(d, 0),
            'D' => self.screen.shift(-d, 0),
            'E' => self.screen.place(0, y.saturating_add(n)),
            'F' => self.screen.place(0, y.saturating_sub(n)),
            'G' | '`' => self.screen.place(n - 1, y),
            'd' => self.screen.place(x, n - 1),
            'H' | 'f' => self.screen.place(usize::from(p.next().unwrap_or(1).max(1)) - 1, n - 1),
            'J' | 'K' => self.screen.erase(action == 'J', first, style),
            '@' | 'P' | 'X' => self.screen.edit_chars(action, n, style),
            'L' | 'M' | 'S' | 'T' =>
            {
                self.screen
                    .scroll(matches!(action, 'M' | 'S'), n, matches!(action, 'L' | 'M'), style)
            }
            'r' =>
            {
                self.screen
                    .scroll_region(n - 1,
                                   p.next().filter(|v| *v != 0).map_or(rows, usize::from) - 1)
            }
            's' => self.screen.save(),
            'u' => self.screen.restore(),
            'm' => self.sgr(params),
            'n' if first == 5 => self.reply(b"\x1b[0n"),
            'n' if first == 6 => self.reply(format!("\x1b[{};{}R", y + 1, x + 1).as_bytes()),
            'c' if first == 0 => self.reply(b"\x1b[?62;4c"),
            't' if first == 18 => self.reply(format!("\x1b[8;{rows};{cols}t").as_bytes()),
            't' if first == 16 =>
            {
                let (w, h) = self.screen.cell_size();
                self.reply(format!("\x1b[6;{h};{w}t").as_bytes());
            }
            _ => ()
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool)
    {
        fn number(bytes: &[u8]) -> Option<u8>
        {
            if bytes.is_empty()
            {
                return None;
            }
            let mut value = 0u8;
            for &byte in bytes
            {
                if !byte.is_ascii_digit()
                {
                    return None;
                }
                value = value.checked_mul(10)?.checked_add(byte - b'0')?;
            }
            Some(value)
        }
        let Some(&operation) = params.first()
        else
        {
            return;
        };
        let terminator = if bell_terminated { "\x07" } else { "\x1b\\" };
        match operation
        {
            b"4" =>
            {
                for pair in params[1..].chunks_exact(2)
                {
                    let Some(index) = number(pair[0])
                    else
                    {
                        continue;
                    };
                    if pair[1] == b"?"
                    {
                        let value = self.screen.palette_color(index);
                        self.reply(format!("\x1b]4;{index};rgb:{:04x}/{:04x}/{:04x}{terminator}",
                                           ((value >> 16) & 255) * 257,
                                           ((value >> 8) & 255) * 257,
                                           (value & 255) * 257).as_bytes());
                    }
                    else if let Some(value) = color::parse(pair[1])
                    {
                        self.screen.set_palette_color(index, value);
                    }
                }
            }
            b"104" if params.len() == 1 => self.screen.reset_palette(),
            b"104" =>
            {
                for parameter in &params[1..]
                {
                    if let Some(index) = number(parameter)
                    {
                        self.screen.reset_palette_color(index);
                    }
                }
            }
            b"10" | b"11" | b"12" =>
            {
                let start = if operation == b"10"
                {
                    10
                }
                else if operation == b"11"
                {
                    11
                }
                else
                {
                    12
                };
                for (offset, spec) in params[1..].iter().enumerate().take(13 - start)
                {
                    let code = start + offset;
                    let foreground = code == 10;
                    if *spec == b"?"
                    {
                        let value = if code == 12
                        {
                            self.screen.cursor_color()
                        }
                        else
                        {
                            self.screen.dynamic_color(foreground)
                        };
                        self.reply(format!("\x1b]{code};rgb:{:04x}/{:04x}/{:04x}{terminator}",
                                           ((value >> 16) & 255) * 257,
                                           ((value >> 8) & 255) * 257,
                                           (value & 255) * 257).as_bytes());
                    }
                    else if let Some(value) = color::parse(spec)
                    {
                        if code == 12
                        {
                            self.screen.set_cursor_color(value);
                        }
                        else
                        {
                            self.screen.set_dynamic_color(foreground, value);
                        }
                    }
                }
            }
            b"110" => self.screen.reset_dynamic_color(true),
            b"111" => self.screen.reset_dynamic_color(false),
            b"112" => self.screen.reset_cursor_color(),
            _ => ()
        }
    }

    fn hook(&mut self, _: &Params, intermediate: &[u8], ignore: bool, action: char)
    {
        *self.image = (!ignore && intermediate.is_empty() && action == 'q').then(Vec::new);
    }

    fn put(&mut self, byte: u8)
    {
        if let Some(image) = self.image
        {
            if image.len() < sixel::INPUT_LIMIT
            {
                image.push(byte);
            }
            else
            {
                *self.image = None;
            }
        }
    }

    fn unhook(&mut self)
    {
        if let Some(payload) = self.image.take()
           && let Some(image) =
               sixel::decode(&payload, self.screen.resolve_color(self.style.bg, false))
        {
            self.screen.image(image, *self.style);
        }
    }
}
