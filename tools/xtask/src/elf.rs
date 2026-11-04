//! Turn a linked executable into the flat image a boot medium carries.
//!
//! This replaces a dependency on a host `llvm-objcopy`: the conversion is a
//! program-header walk over our own linker output, so it works the same on
//! every host without an extra installed tool.
//!
//! Nothing here is architecture-specific. The loader contract is "place these
//! bytes at these physical addresses", which is what `PT_LOAD` already says.
use mac_build::{Result, error};

const PT_LOAD: u32 = 1;

fn u16le(bytes: &[u8], offset: usize) -> Result<u16>
{
    bytes.get(offset..offset + 2)
         .map(|slice| u16::from_le_bytes([slice[0], slice[1]]))
         .ok_or_else(|| error("truncated ELF header"))
}

fn u32le(bytes: &[u8], offset: usize) -> Result<u32>
{
    bytes.get(offset..offset + 4)
         .map(|slice| u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
         .ok_or_else(|| error("truncated ELF header"))
}

fn u64le(bytes: &[u8], offset: usize) -> Result<u64>
{
    bytes.get(offset..offset + 8)
         .map(|slice| {
             u64::from_le_bytes([slice[0], slice[1], slice[2], slice[3], slice[4], slice[5],
                                 slice[6], slice[7]])
         })
         .ok_or_else(|| error("truncated ELF header"))
}

/// The flat image: its load address and its bytes.
pub struct Payload
{
    pub base:  u64,
    pub entry: u64,
    pub bytes: Vec<u8>
}

/// Flatten every loadable segment into one image.
///
/// The load address is the segment's physical address, because a section with
/// a link-time address in RAM can still be loaded from the boot medium. The
/// extent is the file size, so uninitialised data stays out of the payload,
/// exactly as the linker placed it on the medium.
pub fn flatten(image: &[u8], limit: u64) -> Result<Payload>
{
    if image.len() < 64 || &image[..4] != b"\x7fELF"
    {
        return Err(error("bootloader output is not an ELF image"));
    }
    if image[4] != 2 || image[5] != 1
    {
        return Err(error("bootloader ELF must be 64-bit little-endian"));
    }
    let entry = u64le(image, 24)?;
    let program_offset = usize::try_from(u64le(image, 32)?).map_err(|_| {
                                                               error("ELF program header offset \
                                                                      overflows this host")
                                                           })?;
    let entry_size = u16le(image, 54)? as usize;
    let count = u16le(image, 56)? as usize;
    if count == 0
    {
        return Err(error("bootloader ELF has no program headers"));
    }
    let mut segments = Vec::new();
    for index in 0..count
    {
        let at = program_offset
                 + index.checked_mul(entry_size)
                        .ok_or_else(|| error("ELF program header table overflows"))?;
        if u32le(image, at)? != PT_LOAD
        {
            continue;
        }
        let file_size = u64le(image, at + 32)?;
        let memory_size = u64le(image, at + 40)?;
        let offset = u64le(image, at + 8)?;
        let address = u64le(image, at + 24)?;
        if memory_size < file_size
        {
            return Err(error(format!("ELF segment {index} declares less memory than file bytes")));
        }
        if file_size == 0
        {
            continue;
        }
        segments.push((address, offset, file_size));
    }
    let Some(base) = segments.iter().map(|segment| segment.0).min()
    else
    {
        return Err(error("bootloader ELF has no loadable file contents"));
    };
    let end = segments.iter().map(|segment| segment.0 + segment.2).max().unwrap_or(base);
    if end <= base || end - base > limit
    {
        return Err(error(format!("bootloader payload spans {} bytes, which does not fit the \
                                  platform's {limit}-byte boot medium",
                                 end.saturating_sub(base))));
    }
    let mut bytes = vec![0; usize::try_from(end - base).map_err(|_| error("payload too large"))?];
    for (address, offset, file_size) in segments
    {
        let start = usize::try_from(address - base).map_err(|_| error("payload too large"))?;
        let length = usize::try_from(file_size).map_err(|_| error("segment too large"))?;
        let source = usize::try_from(offset).map_err(|_| error("segment offset too large"))?;
        let chunk = image.get(source..source + length)
                         .ok_or_else(|| error("ELF segment extends past the end of the file"))?;
        bytes.get_mut(start..start + length)
             .ok_or_else(|| error("ELF segment overlaps the end of the payload"))?
             .copy_from_slice(chunk);
    }
    Ok(Payload { base,
                 entry,
                 bytes })
}
