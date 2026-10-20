//! Bounded display decoders. Export always uses untouched bytes instead.
use core::fmt::Write;

use mac_toolbox::Error;

use super::document::Text;

fn word(bytes: &[u8], offset: usize) -> Result<u16, Error>
{
    Ok(u16::from_be_bytes(bytes.get(offset..offset + 2)
                               .ok_or(Error::BadArgument)?
                               .try_into()
                               .map_err(|_| Error::BadArgument)?))
}
fn pstring<'a>(bytes: &'a [u8], at: &mut usize) -> Result<&'a [u8], Error>
{
    let size = usize::from(*bytes.get(*at).ok_or(Error::BadArgument)?);
    let result = bytes.get(*at + 1..*at + 1 + size).ok_or(Error::BadArgument)?;
    *at += 1 + size;
    Ok(result)
}
pub fn structured(kind: [u8; 4], data: &[u8], mut line: impl FnMut(&str)) -> Result<bool, Error>
{
    let mut at = 0;
    match &kind
    {
        b"NABI" =>
        {
            mac_code_resource::Descriptor::decode(data).map_err(|_| Error::BadArgument)?;
            let role = match word(data, 6)?
            {
                1 => "System",
                2 => "Application",
                3 => "Code resource",
                _ => return Err(Error::BadArgument)
            };
            let mut text = Text::<128>::new();
            let _ = write!(text,
                           "Native ABI: {} ({role})",
                           u32::from_be_bytes(data[8..12].try_into()
                                                         .map_err(|_| Error::BadArgument)?));
            line(text.as_str());
            for (label, at) in [("Load base", 16), ("Load limit", 24), ("Entry", 32)]
            {
                let mut text = Text::<128>::new();
                let _ = write!(text,
                               "{label}: 0x{:08x}",
                               u64::from_be_bytes(data[at..at + 8].try_into().map_err(|_| {
                                                                                  Error::BadArgument
                                                                              })?));
                line(text.as_str());
            }
        }
        b"STR " =>
        {
            let mut text = Text::<1024>::new();
            text.roman(pstring(data, &mut at)?);
            line(text.as_str());
        }
        b"STR#" =>
        {
            let count = word(data, 0)?;
            at = 2;
            for index in 0..count
            {
                let mut text = Text::<1024>::new();
                let _ = write!(text, "{}: ", index + 1);
                text.roman(pstring(data, &mut at)?);
                line(text.as_str());
            }
        }
        b"vers" =>
        {
            let header = data.get(..6).ok_or(Error::BadArgument)?;
            let mut text = Text::<128>::new();
            let _ = write!(text,
                           "Version {}.{}.{}",
                           (header[0] >> 4) * 10 + (header[0] & 15),
                           header[1] >> 4,
                           header[1] & 15);
            line(text.as_str());
            let mut text = Text::<128>::new();
            let _ = write!(text,
                           "Stage 0x{:02x}, build {}, country {}",
                           header[2],
                           header[3],
                           word(data, 4)?);
            line(text.as_str());
            at = 6;
            for _ in 0..2
            {
                let mut text = Text::<1024>::new();
                text.roman(pstring(data, &mut at)?);
                line(text.as_str());
            }
        }
        b"MENU" =>
        {
            let header = data.get(..12).ok_or(Error::BadArgument)?;
            let enabled =
                u32::from_be_bytes(header[8..12].try_into().map_err(|_| Error::BadArgument)?);
            let mut text = Text::<128>::new();
            let _ = write!(text, "Menu {}   MDEF {}", word(data, 0)? as i16, word(data, 6)? as i16);
            line(text.as_str());
            let mut text = Text::<128>::new();
            let _ = write!(text,
                           "Size {} x {}   {}",
                           word(data, 2)?,
                           word(data, 4)?,
                           if enabled & 1 != 0 { "enabled" } else { "disabled" });
            line(text.as_str());
            at = 12;
            let mut text = Text::<1024>::new();
            text.roman(pstring(data, &mut at)?);
            line(text.as_str());
            let mut index = 1u32;
            loop
            {
                let label = pstring(data, &mut at)?;
                if label.is_empty()
                {
                    break;
                }
                let metadata = data.get(at..at + 4).ok_or(Error::BadArgument)?;
                at += 4;
                let mut text = Text::<1024>::new();
                let _ = write!(text,
                               "{} ",
                               if enabled & 1 != 0 && index < 32 && enabled & (1 << index) != 0
                               {
                                   "+"
                               }
                               else
                               {
                                   "-"
                               });
                if metadata[2] != 0
                {
                    let _ = text.write_str("[marked] ");
                }
                text.roman(label);
                line(text.as_str());
                let mut text = Text::<128>::new();
                let _ = write!(text,
                               "  icon {}  key {}  mark {}  style {:#x}",
                               metadata[0], metadata[1], metadata[2], metadata[3]);
                line(text.as_str());
                index += 1;
            }
        }
        _ => return Ok(false)
    }
    Ok(true)
}
pub fn hex(data: &[u8], offset: usize, mut line: impl FnMut(&str))
{
    for (index, chunk) in data.chunks(8).enumerate()
    {
        let mut text = Text::<128>::new();
        let _ = write!(text, "{:08x} ", offset + index * 8);
        for byte in chunk
        {
            let _ = write!(text, "{:02x} ", byte);
        }
        for _ in chunk.len()..8
        {
            let _ = text.write_str("   ");
        }
        let _ = text.write_str(" ");
        for &byte in chunk
        {
            let _ = text.write_char(if (32..127).contains(&byte) { byte as char } else { '.' });
        }
        line(text.as_str());
    }
}
