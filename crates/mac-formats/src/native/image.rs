//! Compact RV64 segments. Runtime metadata contains no ELF structures.
#![forbid(unsafe_code)]
mod destination;
use alloc::vec::Vec;

pub use destination::{Destination, WriteError};
pub use mac_code_resource::image::{EXECUTE, HEADER_SIZE, MAX_SEGMENTS, READ, RECORD_SIZE, Segment,
                                   WRITE};

use super::{Descriptor, FORMAT_VERSION};
use crate::{Error, Result};

/// Adapt the allocation-free wire inspector without discarding a typed error
/// returned by its reader. Wire failures themselves are always malformed data.
pub fn inspect(length: usize, descriptor: &Descriptor,
               mut read: impl FnMut(usize, &mut [u8]) -> Result<()>, emit: impl FnMut(Segment))
               -> Result<usize>
{
    let mut read_error = None;
    let result = mac_code_resource::image::inspect(length,
                                                   descriptor,
                                                   |at, output| {
                                                       read(at, output).map_err(|error| {
                                                           read_error = Some(error);
                                                           mac_code_resource::Error("native image \
                                                                                     reader failed")
                                                       })
                                                   },
                                                   emit);
    result.map_err(|error| read_error.unwrap_or_else(|| error.into()))
}
#[derive(Debug)]
pub struct Image
{
    pub segments: Vec<Segment>
}

pub fn decode(bytes: &[u8], descriptor: &Descriptor) -> Result<Image>
{
    let mut count = 0;
    let end = inspect(bytes.len(),
                      descriptor,
                      |at, out| {
                          out.copy_from_slice(crate::slice(bytes, at, out.len())?);
                          Ok(())
                      },
                      |_| count += 1)?;
    if end != bytes.len()
    {
        return Err(Error::Malformed("trailing data in RV64 segment resource"));
    }
    // Validate all wire geometry before allocating from its segment count.
    // Inspection reads metadata only; it does not copy or scan segment bodies.
    let mut segments = Vec::new();
    segments.try_reserve_exact(count)
            .map_err(|_| Error::AllocationFailed("native segment metadata allocation failed"))?;
    inspect(bytes.len(),
            descriptor,
            |at, out| {
                out.copy_from_slice(crate::slice(bytes, at, out.len())?);
                Ok(())
            },
            |segment| segments.push(segment))?;
    Ok(Image { segments })
}
/// BSS and all other uninitialized reservation bytes are implicit zeros.
pub fn encode<'a>(descriptor: &Descriptor,
                  segments: impl IntoIterator<Item = (u64, u64, u32, &'a [u8])>)
                  -> Result<Vec<u8>>
{
    let mut result = b"RV64".to_vec();
    result.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
    result.extend_from_slice(&0u16.to_be_bytes());
    let mut count = 0u16;
    for (offset, memory_length, flags, bytes) in segments
    {
        count = count.checked_add(1).ok_or(Error::Malformed("too many RV64 segments"))?;
        result.extend_from_slice(&offset.to_be_bytes());
        result.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        result.extend_from_slice(&memory_length.to_be_bytes());
        result.extend_from_slice(&flags.to_be_bytes());
        result.extend_from_slice(bytes);
    }
    result[6..8].copy_from_slice(&count.to_be_bytes());
    decode(&result, descriptor)?;
    Ok(result)
}
impl Image
{
    pub fn source(&self, address: u64, width: usize) -> Result<usize>
    {
        let end = address.checked_add(width as u64)
                         .ok_or(Error::Malformed("relocation address overflow"))?;
        let segment = self.segments
                          .iter()
                          .find(|s| address >= s.offset && end <= s.offset + s.length as u64)
                          .ok_or(Error::Malformed("relocation outside initialized image"))?;
        if segment.flags & EXECUTE != 0
        {
            return Err(Error::Malformed("native PIC contract forbids relocation slots in \
                                         executable segments; rebuild the package"));
        }
        Ok(segment.data_offset + (address - segment.offset) as usize)
    }
}

pub struct LoadPlan<'a>
{
    bytes:       &'a [u8],
    image:       Image,
    entry:       u64,
    length:      usize,
    patches:     Vec<crate::relocation::Patch>,
    destination: u64
}
impl<'a> LoadPlan<'a>
{
    pub fn entry(&self) -> u64
    {
        self.entry
    }

    /// Validated memory extents used by real segmented native destinations.
    /// Metadata grants neither mapping ownership nor permission to execute.
    pub fn segments(&self) -> &[Segment]
    {
        &self.image.segments
    }

    pub fn length(&self) -> usize
    {
        self.length
    }

    /// Validate the entire image and RLOC table before any destination writes.
    pub fn new(bytes: &'a [u8], descriptor: &Descriptor, relocations: Option<&[u8]>,
               destination: u64)
               -> Result<Self>
    {
        let image = decode(bytes, descriptor)?;
        let extent = descriptor.load_limit - descriptor.load_base;
        destination.checked_add(extent).ok_or(Error::Malformed("native destination overflow"))?;
        let length =
            usize::try_from(extent).map_err(|_| Error::Malformed("native reservation overflow"))?;
        let patches = if let Some(table) = relocations
        {
            crate::relocation::patches(table,
                                       descriptor.load_base,
                                       extent,
                                       destination,
                                       |offset, width| {
                                           if descriptor.role == super::Role::CodeResource
                                              && width == 4
                                           {
                                               return Err(Error::Malformed("RV64 module/\
                                                                            definition \
                                                                            pointers require \
                                                                            64-bit relocation \
                                                                            slots; rebuild the \
                                                                            package for \
                                                                            arbitrary placement"));
                                           }
                                           let at = image.source(offset, usize::from(width))?;
                                           let data = crate::slice(bytes, at, usize::from(width))?;
                                           Ok(if width == 4
                                              {
                                                  u64::from(u32::from_le_bytes(data.try_into()
                                                                                   .unwrap()))
                                              }
                                              else
                                              {
                                                  u64::from_le_bytes(data.try_into().unwrap())
                                              })
                                       })?
        }
        else
        {
            if descriptor.role != super::Role::System
            {
                return Err(Error::Missing("native relocation metadata is missing"));
            }
            if destination != descriptor.load_base
            {
                return Err(Error::Malformed("System image must use its linked reservation"));
            }
            Vec::new()
        };
        let entry = destination + (descriptor.entry - descriptor.load_base);
        if entry & 1 != 0
        {
            return Err(Error::Malformed("relocated entry is not instruction aligned"));
        }
        Ok(Self { bytes,
                  image,
                  entry,
                  length,
                  patches,
                  destination })
    }

    pub fn write(&self, output: &mut [u8]) -> Result<()>
    {
        self.write_to(&mut destination::Slice(output)).map_err(|error| {
                                                          match error
                                                          {
                                                              WriteError::Format(error)
                                                              | WriteError::Destination(error) =>
                                                              {
                                                                  error
                                                              }
                                                          }
                                                      })
    }

    /// Initialize owned storage independently of its CPU alias or physical
    /// contiguity. Relocation values use the plan's execution destination;
    /// output offsets never reinterpret an execution VA as a host pointer.
    pub fn write_to<T: Destination>(&self, output: &mut T)
                                    -> core::result::Result<(), WriteError<T::Error>>
    {
        if output.length() != self.length
        {
            return Err(WriteError::Format(Error::Malformed("native destination has wrong \
                                                            length")));
        }
        // Initialize BSS, holes and owned padding. Storage that knows the plan
        // can leave file prefixes for the complete writes below, avoiding a
        // redundant clear before copying them. No image is publishable until
        // every write and relocation succeeds; refusal retains its owner.
        output.prepare(&self.image.segments).map_err(WriteError::Destination)?;
        for segment in &self.image.segments
        {
            if segment.length == 0
            {
                continue; // BSS-only memory is already zeroed in its owned backing.
            }
            output.write(segment.offset as usize,
                         &self.bytes[segment.data_offset..segment.data_offset + segment.length])
                  .map_err(WriteError::Destination)?;
        }
        for patch in &self.patches
        {
            let at = (patch.address - self.destination) as usize;
            let value = patch.value.to_le_bytes();
            output.write(at, &value[..usize::from(patch.width)])
                  .map_err(WriteError::Destination)?;
        }
        Ok(())
    }
}

