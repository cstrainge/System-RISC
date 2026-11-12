//! Virtio evdev packets become hardware-independent input, then classic events.
#![forbid(unsafe_code)]
use mac_abi::{events::*, geometry::Point};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input
{
    Move(Point),
    Button
    {
        point: Point,
        down:  bool
    },
    Key
    {
        mac_code:  u8,
        character: u8,
        down:      bool,
        repeat:    bool,
        modifiers: u16
    }
}

pub struct Decoder
{
    point:   Point,
    pending: Point,
    button:  Option<bool>,
    keys:    [bool; 128],
    caps:    bool
}
impl Default for Decoder
{
    fn default() -> Self
    {
        Self::new()
    }
}
impl Decoder
{
    pub fn new() -> Self
    {
        Self { point:   Point { h: 16,
                                v: 444 },
               pending: Point { h: 16,
                                v: 444 },
               button:  None,
               keys:    [false; 128],
               caps:    false }
    }

    fn modifiers(&self) -> u16
    {
        (if self.keys[42] || self.keys[54] { SHIFT_KEY } else { 0 })
        | (if self.keys[100] { OPTION_KEY } else { 0 })
        | (if self.keys[29] || self.keys[97] { CONTROL_KEY } else { 0 })
        // Left Alt supplies Command even when the host intercepts Super.
        | (if self.keys[56] || self.keys[125] || self.keys[126] { CMD_KEY } else { 0 })
        | (if self.caps { ALPHA_LOCK } else { 0 })
    }

    /// Physical keys translated to the guest modifier layout.
    /// Combining decoder snapshots, rather than clearing individual bits on a
    /// release, preserves a modifier held on another input device/key.
    pub fn key_map(&self) -> KeyMap
    {
        let mut map = KeyMap::default();
        for (code, &down) in self.keys.iter().enumerate()
        {
            if !down
            {
                continue;
            }
            let mac_code = match code
            {
                29 => 0x3b,
                97 => 0x3e,
                56 | 125 | 126 => 0x37,
                42 => 0x38,
                54 => 0x3c,
                100 => 0x3d,
                58 => 0x39,
                _ => key(code as u16).0
            };
            map.set(mac_code, true);
        }
        map
    }

    pub fn feed(&mut self, kind: u16, code: u16, value: u32, mut emit: impl FnMut(Input))
    {
        match (kind, code)
        {
            (3, 0) =>
            {
                self.pending.h = absolute_pixel(value, crate::display::width());
            }
            (3, 1) =>
            {
                self.pending.v = absolute_pixel(value, crate::display::height());
            }
            // Relative movement: a mouse reports how far it moved, as a signed
            // count, and the pointer stays on the screen.
            (2, 0) =>
            {
                self.pending.h = relative_pixel(self.pending.h, value, crate::display::width());
            }
            (2, 1) =>
            {
                self.pending.v = relative_pixel(self.pending.v, value, crate::display::height());
            }
            (1, 0x110) => self.button = Some(value != 0),
            (0, 0) =>
            {
                if self.pending != self.point
                {
                    self.point = self.pending;
                    emit(Input::Move(self.point));
                }
                if let Some(down) = self.button.take()
                {
                    emit(Input::Button { point: self.point,
                                         down });
                }
            }
            (1, code) if code < 128 =>
            {
                let down = value != 0;
                if code == 58 && value == 1
                {
                    self.caps = !self.caps;
                }
                self.keys[code as usize] = down;
                let modifiers = self.modifiers();
                let (mac_code, mut character) = key(code);
                if character.is_ascii_alphabetic() && ((modifiers & SHIFT_KEY != 0) ^ self.caps)
                {
                    character = character.to_ascii_uppercase();
                }
                else if modifiers & SHIFT_KEY != 0
                {
                    character = match character
                    {
                        b'1' => b'!',
                        b'2' => b'@',
                        b'3' => b'#',
                        b'4' => b'$',
                        b'5' => b'%',
                        b'6' => b'^',
                        b'7' => b'&',
                        b'8' => b'*',
                        b'9' => b'(',
                        b'0' => b')',
                        b'-' => b'_',
                        b'=' => b'+',
                        b'[' => b'{',
                        b']' => b'}',
                        b';' => b':',
                        b'\'' => b'"',
                        b',' => b'<',
                        b'.' => b'>',
                        b'/' => b'?',
                        b'\\' => b'|',
                        b'`' => b'~',
                        other => other
                    };
                }
                // Modifier transitions update state but do not create keyDown events.
                if !matches!(code, 29 | 42 | 54 | 56 | 58 | 97 | 100 | 125 | 126)
                {
                    emit(Input::Key { mac_code,
                                      character,
                                      down,
                                      repeat: value == 2,
                                      modifiers });
                }
                else
                {
                    emit(Input::Key { mac_code: 0xff,
                                      character: 0,
                                      down,
                                      repeat: false,
                                      modifiers });
                }
            }
            _ =>
            {}
        }
    }
}

fn relative_pixel(position: i16, delta: u32, extent: usize) -> i16
{
    let limit = extent.saturating_sub(1).min(i16::MAX as usize) as i32;
    (i32::from(position) + delta as i32).clamp(0, limit) as i16
}

fn absolute_pixel(value: u32, extent: usize) -> i16
{
    const AXIS_MAX: u64 = 32767;
    // QEMU normalizes pixel coordinates by the full extent and truncates.
    // Round to recover the pixel, then clamp the inclusive tablet endpoint.
    let scaled = (u64::from(value).min(AXIS_MAX) * extent as u64 + AXIS_MAX / 2) / AXIS_MAX;
    scaled.min(extent.saturating_sub(1) as u64) as i16
}

fn key(code: u16) -> (u8, u8)
{
    match code
    {
        1 => (0x35, 27),
        2 => (0x12, b'1'),
        3 => (0x13, b'2'),
        4 => (0x14, b'3'),
        5 => (0x15, b'4'),
        6 => (0x17, b'5'),
        7 => (0x16, b'6'),
        8 => (0x1a, b'7'),
        9 => (0x1c, b'8'),
        10 => (0x19, b'9'),
        11 => (0x1d, b'0'),
        12 => (0x1b, b'-'),
        13 => (0x18, b'='),
        14 => (0x33, 8),
        15 => (0x30, 9),
        16 => (0x0c, b'q'),
        17 => (0x0d, b'w'),
        18 => (0x0e, b'e'),
        19 => (0x0f, b'r'),
        20 => (0x11, b't'),
        21 => (0x10, b'y'),
        22 => (0x20, b'u'),
        23 => (0x22, b'i'),
        24 => (0x1f, b'o'),
        25 => (0x23, b'p'),
        26 => (0x21, b'['),
        27 => (0x1e, b']'),
        28 => (0x24, 13),
        30 => (0, b'a'),
        31 => (1, b's'),
        32 => (2, b'd'),
        33 => (3, b'f'),
        34 => (5, b'g'),
        35 => (4, b'h'),
        36 => (0x26, b'j'),
        37 => (0x28, b'k'),
        38 => (0x25, b'l'),
        39 => (0x29, b';'),
        40 => (0x27, b'\''),
        41 => (0x32, b'`'),
        43 => (0x2a, b'\\'),
        44 => (6, b'z'),
        45 => (7, b'x'),
        46 => (8, b'c'),
        47 => (9, b'v'),
        48 => (0x0b, b'b'),
        49 => (0x2d, b'n'),
        50 => (0x2e, b'm'),
        51 => (0x2b, b','),
        52 => (0x2f, b'.'),
        53 => (0x2c, b'/'),
        57 => (0x31, b' '),
        102 => (0x73, 1),
        103 => (0x7e, 30),
        104 => (0x74, 11),
        105 => (0x7b, 28),
        106 => (0x7c, 29),
        107 => (0x77, 4),
        108 => (0x7d, 31),
        109 => (0x79, 12),
        111 => (0x75, 127),
        _ => (0xff, 0)
    }
}

