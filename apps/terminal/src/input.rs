//! Encode one native key without allocating an intermediate String.
use std::collections::VecDeque;

use mac_toolbox::{Error,
                  events::{Key, Modifiers}};

pub struct Bytes
{
    data:   [u8; 12],
    length: usize
}
impl Bytes
{
    fn new() -> Self
    {
        Self { data:   [0; 12],
               length: 0 }
    }

    fn push(&mut self, bytes: &[u8]) -> Option<()>
    {
        let end = self.length.checked_add(bytes.len())?;
        self.data.get_mut(self.length..end)?.copy_from_slice(bytes);
        self.length = end;
        Some(())
    }

    pub fn as_slice(&self) -> &[u8]
    {
        &self.data[..self.length]
    }
}

pub fn queue(queue: &mut VecDeque<u8>, bytes: &[u8]) -> Result<(), Error>
{
    queue.try_reserve(bytes.len()).map_err(|_| Error::NoMemory)?;
    queue.extend(bytes);
    Ok(())
}

pub fn encode(key: Key, modifiers: Modifiers, application_keys: bool) -> Option<Bytes>
{
    let mask = 1
               + u8::from(modifiers.contains(Modifiers::SHIFT))
               + 2 * u8::from(modifiers.contains(Modifiers::OPTION))
               + 4 * u8::from(modifiers.contains(Modifiers::CONTROL));
    let mut bytes = Bytes::new();
    let code = key.code();
    match code
    {
        0x7b..=0x7e | 0x73 | 0x77 =>
        {
            let suffix = match code
            {
                0x7b => b'D',
                0x7c => b'C',
                0x7d => b'B',
                0x7e => b'A',
                0x73 => b'H',
                _ => b'F'
            };
            if mask == 1
            {
                bytes.push(if application_keys { b"\x1bO" } else { b"\x1b[" })?;
            }
            else
            {
                bytes.push(b"\x1b[1;")?;
                bytes.push(&[b'0' + mask])?;
            }
            bytes.push(&[suffix])?;
        }
        0x74 | 0x79 | 0x75 =>
        {
            let number = match code
            {
                0x74 => b'5',
                0x79 => b'6',
                _ => b'3'
            };
            bytes.push(b"\x1b[")?;
            bytes.push(&[number])?;
            if mask != 1
            {
                bytes.push(&[b';', b'0' + mask])?;
            }
            bytes.push(b"~")?;
        }
        0x30 if mask == 2 => bytes.push(b"\x1b[Z")?,
        0x24 | 0x4c | 0x30 | 0x33 if mask != 1 =>
        {
            // CSI-u disambiguates these modified keys; full Kitty protocol
            // detection stays false until its complete contract is implemented.
            bytes.push(b"\x1b[")?;
            bytes.push(match code
                       {
                           0x24 | 0x4c => b"13",
                           0x30 => b"9",
                           _ => b"127"
                       })?;
            bytes.push(&[b';', b'0' + mask, b'u'])?;
        }
        0x24 | 0x4c => bytes.push(b"\r")?,
        0x30 => bytes.push(b"\t")?,
        0x33 => bytes.push(b"\x7f")?,
        0x35 => bytes.push(b"\x1b")?,
        0x31 if modifiers.contains(Modifiers::CONTROL) => bytes.push(b"\0")?,
        _ =>
        {
            let character = key.character();
            if modifiers.contains(Modifiers::CONTROL)
               && (character.is_ascii_alphabetic()
                   || matches!(character, '@' | '[' | '\\' | ']' | '^' | '_'))
            {
                bytes.push(&[character as u8 & 31])?;
            }
            else if character != '\0'
            {
                if modifiers.contains(Modifiers::OPTION)
                {
                    bytes.push(b"\x1b")?;
                }
                let mut utf8 = [0; 4];
                bytes.push(character.encode_utf8(&mut utf8).as_bytes())?;
            }
            else
            {
                return None;
            }
        }
    }
    Some(bytes)
}
