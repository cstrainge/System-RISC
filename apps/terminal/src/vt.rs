//! Decode terminal output into operations on the retained view.
use std::collections::VecDeque;

use vte::{Params, Parser, Perform};

use crate::{PIPE_BYTES,
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
    {
        let mut target = Target { screen,
                                  replies,
                                  style: &mut self.style,
                                  default: self.default,
                                  image: &mut self.image };
        self.parser.advance(&mut target, bytes);
    }
}

struct Target<'a>
{
    screen:  &'a mut Screen,
    replies: &'a mut VecDeque<u8>,
    style:   &'a mut Style,
    default: Style,
    image:   &'a mut Option<Vec<u8>>
}
impl Target<'_>
{
    fn reply(&mut self, bytes: &[u8])
    {
        if bytes.len() <= PIPE_BYTES - self.replies.len()
        {
            self.replies.extend(bytes);
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
                7 | 27 => self.style.reverse = code == 7,
                30..=37 | 90..=97 =>
                {
                    self.style.fg = palette(if code < 40 { code - 30 } else { code - 82 })
                }
                40..=47 | 100..=107 =>
                {
                    self.style.bg = palette(if code < 50 { code - 40 } else { code - 92 })
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
                        Some(5) => next().filter(|v| *v <= 255).map(palette),
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
                                    Some(rgb(r as u32, g as u32, b as u32))
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

    fn osc_dispatch(&mut self, params: &[&[u8]], _: bool)
    {
        match params
        {
            [b"777", b"systemrisc", b"raw", b"1"] => self.screen.raw_input = true,
            [b"777", b"systemrisc", b"raw", b"0"] => self.screen.raw_input = false,
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
           && let Some(image) = sixel::decode(&payload, self.style.bg)
        {
            self.screen.image(image);
        }
    }
}

fn rgb(r: u32, g: u32, b: u32) -> u32
{
    (r << 16) | (g << 8) | b
}

pub fn palette(index: u16) -> u32
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
        232..=255 =>
        {
            let level = 8 + 10 * u32::from(index - 232);
            rgb(level, level, level)
        }
        _ => 0
    }
}
