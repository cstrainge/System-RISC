//! Read-only document and streaming, lossless resources.json archive export.
//! No guest allocator or executable loading is needed, including for large CODE.
use core::fmt::{self, Write};

use mac_toolbox::{files::{CatalogInfo, Fork},
                  resources::ResourceFile,
                  *};

pub struct Text<const N: usize>
{
    bytes:  [u8; N],
    length: usize
}
impl<const N: usize> Text<N>
{
    pub const fn new() -> Self
    {
        Self { bytes:  [0; N],
               length: 0 }
    }

    pub fn as_str(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.length]).unwrap_or("")
    }

    pub fn roman(&mut self, bytes: &[u8])
    {
        for &byte in bytes
        {
            let _ = self.write_char(mac_text::character(byte));
        }
    }
}
impl<const N: usize> Write for Text<N>
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        let end = self.length.checked_add(text.len()).filter(|&n| n <= N).ok_or(fmt::Error)?;
        self.bytes[self.length..end].copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
    }
}
fn json_string(out: &mut impl Write, text: &str) -> fmt::Result
{
    out.write_char('"')?;
    for ch in text.chars()
    {
        match ch
        {
            '"' => out.write_str("\\\"")?,
            '\\' => out.write_str("\\\\")?,
            '\u{0}'..='\u{1f}' => write!(out, "\\u{:04x}", ch as u32)?,
            _ => out.write_char(ch)?
        }
    }
    out.write_char('"')
}
fn fourcc(out: &mut impl Write, bytes: &[u8; 4]) -> fmt::Result
{
    let mut text = Text::<16>::new();
    text.roman(bytes);
    json_string(out, text.as_str())
}
fn hex(out: &mut impl Write, bytes: &[u8]) -> fmt::Result
{
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut buffer = [0; 1024];
    for chunk in bytes.chunks(512)
    {
        for (at, &byte) in chunk.iter().enumerate()
        {
            buffer[at * 2] = DIGITS[(byte >> 4) as usize];
            buffer[at * 2 + 1] = DIGITS[(byte & 15) as usize];
        }
        out.write_str(core::str::from_utf8(&buffer[..chunk.len() * 2]).map_err(|_| fmt::Error)?)?;
    }
    Ok(())
}
#[derive(Debug)]
pub enum ExportError
{
    Service(Error),
    Output
}
impl From<Error> for ExportError
{
    fn from(e: Error) -> Self
    {
        Self::Service(e)
    }
}
impl From<fmt::Error> for ExportError
{
    fn from(_: fmt::Error) -> Self
    {
        Self::Output
    }
}

/// Export raw bytes, not reconstructed structured previews. In archive mode the
/// compiler preserves NABI/RV64/RLOC/vers and does not synthesize replacements.
pub fn export(system: &System<'_>, file: &ResourceFile<'_>, catalog: &CatalogInfo,
              out: &mut impl Write)
              -> Result<(), ExportError>
{
    out.write_str("{\n\"archive\":true,\"name\":")?;
    json_string(out, catalog.name())?;
    out.write_str(",\"resource_format\":")?;
    json_string(out, match file.format()?
    {
        mac_toolbox::resources::ResourceForkFormat::Classic => "classic",
        mac_toolbox::resources::ResourceForkFormat::SystemRisc => "system_risc"
    })?;
    out.write_str(",\"file_type\":")?;
    fourcc(out, catalog.file_type.as_bytes())?;
    out.write_str(",\"creator\":")?;
    fourcc(out, catalog.creator.as_bytes())?;
    write!(out,
           ",\"file_flags\":{},\"created\":{},\"modified\":{},\"role\":\"application\",\"\
            abi_version\":{},\"load_base\":\"0x83000000\",\"load_limit\":\"0x84000000\",\"\
            description\":\"Lossless ResEdit \
            archive\",\"data_fork\":{{\"format\":\"hex\",\"hex\":\"",
           catalog.finder_flags().bits(),
           catalog.created.mac_microseconds().ok_or(mac_toolbox::Error::BadAbi)?,
           catalog.modified.mac_microseconds().ok_or(mac_toolbox::Error::BadAbi)?,
           mac_toolbox::abi::ABI_VERSION)?;
    let mut data = system.files().open(catalog.volume, catalog.id, Fork::Data)?;
    let mut buffer = [0; 512];
    let mut remaining = data.len()?;
    while remaining != 0
    {
        let size = remaining.min(buffer.len() as u64) as usize;
        let count = data.read(&mut buffer[..size])?;
        if count != size
        {
            return Err(ExportError::Service(Error::Io));
        }
        hex(out, &buffer[..size])?;
        remaining -= size as u64;
    }
    data.close()?;
    out.write_str("\"},\"resources\":[\n")?;
    let mut ordinal = 0u32;
    for type_index in 0..file.type_count()?
    {
        let kind = file.type_at(type_index)?;
        for index in 0..file.count(kind)?
        {
            let item = file.at(kind, index)?;
            if ordinal != 0
            {
                out.write_str(",\n")?;
            }
            out.write_str("{\"type\":")?;
            fourcc(out, kind.as_bytes())?;
            write!(out,
                   ",\"id\":{},\"symbol\":\"RESOURCE_{}\",\"attributes\":{},\"name\":",
                   item.info.key.id,
                   ordinal,
                   item.info.attributes.bits())?;
            if item.has_name
            {
                json_string(out, item.info.name())?;
            }
            else
            {
                out.write_str("null")?;
            }
            out.write_str(",\"payload\":{\"format\":\"hex\",\"hex\":\"")?;
            let mut offset = 0;
            while offset < item.info.length
            {
                let size = (item.info.length - offset).min(buffer.len());
                if file.read(item.info.key, offset, &mut buffer[..size])? != size
                {
                    return Err(ExportError::Service(Error::Io));
                }
                hex(out, &buffer[..size])?;
                offset += size;
            }
            out.write_str("\"}}")?;
            ordinal += 1;
        }
    }
    out.write_str("\n]}\n")?;
    Ok(())
}
