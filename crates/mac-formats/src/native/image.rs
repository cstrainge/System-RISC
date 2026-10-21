//! Compact RV64 segments. Runtime metadata contains no ELF structures.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

pub use mac_code_resource::image::*;

use super::{Descriptor, FORMAT_VERSION};
use crate::{Error, Result};
#[derive(Debug)]
pub struct Image
{
    pub segments: Vec<Segment>
}

pub fn decode(bytes: &[u8], descriptor: &Descriptor) -> Result<Image>
{
    let mut segments = Vec::new();
    let end = inspect(bytes.len(),
                      descriptor,
                      |at, out| {
                          out.copy_from_slice(crate::slice(bytes, at, out.len())?);
                          Ok(())
                      },
                      |segment| segments.push(segment))?;
    if end != bytes.len()
    {
        return Err(Error("trailing data in RV64 segment resource"));
    }
    Ok(Image { segments })
}
/// BSS and all other uninitialized reservation bytes are implicit zeros.
pub fn encode<'a>(descriptor: &Descriptor,
                  segments: impl IntoIterator<Item = (u64, u32, &'a [u8])>)
                  -> Result<Vec<u8>>
{
    let mut result = b"RV64".to_vec();
    result.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
    result.extend_from_slice(&0u16.to_be_bytes());
    let mut count = 0u16;
    for (offset, flags, bytes) in segments
    {
        if bytes.is_empty()
        {
            continue;
        }
        count = count.checked_add(1).ok_or(Error("too many RV64 segments"))?;
        result.extend_from_slice(&offset.to_be_bytes());
        result.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
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
        let end = address.checked_add(width as u64).ok_or(Error("relocation address overflow"))?;
        let segment = self.segments
                          .iter()
                          .find(|s| address >= s.offset && end <= s.offset + s.length as u64)
                          .ok_or(Error("relocation outside initialized image"))?;
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
        destination.checked_add(extent).ok_or(Error("native destination overflow"))?;
        let length = usize::try_from(extent).map_err(|_| Error("native reservation overflow"))?;
        let patches = if let Some(table) = relocations
        {
            crate::relocation::patches(table,
                                       descriptor.load_base,
                                       extent,
                                       destination,
                                       |offset, width| {
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
                return Err(Error("native relocation metadata is missing"));
            }
            if destination != descriptor.load_base
            {
                return Err(Error("System image must use its linked reservation"));
            }
            Vec::new()
        };
        let entry = destination + (descriptor.entry - descriptor.load_base);
        if entry & 1 != 0
        {
            return Err(Error("relocated entry is not instruction aligned"));
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
        if output.len() != self.length
        {
            return Err(Error("native destination has wrong length"));
        }
        // Zero the reservation first: BSS, inter-segment holes and trailing bytes
        // stay zero without erasing initialized segments or relocated pointers.
        output.fill(0);
        for segment in &self.image.segments
        {
            let start = segment.offset as usize;
            output[start..start + segment.length].copy_from_slice(&self.bytes[segment.data_offset
                                                                              ..segment.data_offset
                                                                                + segment.length]);
        }
        for patch in &self.patches
        {
            let at = (patch.address - self.destination) as usize;
            let value = patch.value.to_le_bytes();
            output[at..at + usize::from(patch.width)].copy_from_slice(&value[..usize::from(patch.width)]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests
{
    use alloc::vec;

    use super::*;
    fn descriptor() -> Descriptor
    {
        Descriptor { role:        super::super::Role::CodeResource,
                     abi_version: 7,
                     load_base:   0x82000000,
                     load_limit:  0x82001000,
                     entry:       0x82000000 }
    }
    fn fixture() -> (Descriptor, Vec<u8>, Vec<u8>)
    {
        let d = descriptor();
        let pointer = (d.load_base + 0x800).to_le_bytes();
        let code =
            encode(&d, [(0, 5, [0x82, 0x80].as_slice()), (0x100, 6, pointer.as_slice())]).unwrap();
        let table = [b"RLC1".as_slice(), &1u32.to_le_bytes(), &[0, 1, 0, 0, 8, 0, 0, 0]].concat();
        (d, code, table)
    }
    #[test]
    fn compact_segments_relocate_and_zero_every_uninitialized_byte()
    {
        let (d, code, table) = fixture();
        assert_eq!(code.len(), 8 + 20 + 2 + 20 + 8);
        assert_eq!(&code[..8], b"RV64\0\x01\0\x02");
        for destination in [0x82000000, 0x82f00000, 0x81000000, 0x1000]
        {
            let plan = LoadPlan::new(&code, &d, Some(&table), destination).unwrap();
            let mut ram = vec![0xa5; 4096];
            plan.write(&mut ram).unwrap();
            assert_eq!(plan.entry(), destination);
            assert_eq!(&ram[..2], &[0x82, 0x80]);
            assert_eq!(&ram[0x100..0x108], &(destination + 0x800).to_le_bytes());
            assert!(ram[2..0x100].iter().chain(&ram[0x108..]).all(|b| *b == 0));
            let mut wrong = vec![0xa5; 4095];
            assert!(plan.write(&mut wrong).is_err());
            assert!(wrong.iter().all(|b| *b == 0xa5));
        }
    }
    #[test]
    fn malformed_streams_and_relocations_never_produce_a_load_plan()
    {
        let (d, code, table) = fixture();
        for len in 0..code.len()
        {
            assert!(LoadPlan::new(&code[..len], &d, Some(&table), 0x82f00000).is_err(),
                    "length {len}");
        }
        for (offset, value) in
            [(0, b'X'), (4, 1), (5, 2), (6, 2), (7, 0), (8, 0xff), (16, 0xff), (24, 0x80), (27, 7)]
        {
            let mut bad = code.clone();
            bad[offset] = value;
            assert!(LoadPlan::new(&bad, &d, Some(&table), 0x82f00000).is_err(), "offset {offset}");
        }
        let mut overlap = code.clone();
        overlap[30..38].copy_from_slice(&1u64.to_be_bytes());
        assert!(decode(&overlap, &d).is_err());
        let mut trailing = code.clone();
        trailing.push(0);
        assert!(decode(&trailing, &d).is_err());
        for len in 0..table.len()
        {
            assert!(LoadPlan::new(&code, &d, Some(&table[..len]), 0x82f00000).is_err());
        }
        let mut bad = table.clone();
        bad[8..12].copy_from_slice(&0x102u32.to_le_bytes());
        assert!(LoadPlan::new(&code, &d, Some(&bad), 0x82f00000).is_err());
        assert!(LoadPlan::new(&code, &d, None, d.load_base).is_err());
        assert!(LoadPlan::new(&code, &d, Some(&table), 0x82000001).is_err());
        assert!(LoadPlan::new(&code, &d, Some(&table), u64::MAX - 1).is_err());
        for entry in [d.load_base + 1, d.load_base + 2, d.load_base + 0x100, d.load_base + 0x800]
        {
            let invalid = Descriptor { entry,
                                       ..d };
            assert!(decode(&code, &invalid).is_err());
        }
    }
    #[test]
    fn obsolete_elf_layouts_are_rejected_without_a_compatibility_path()
    {
        let (d, _, table) = fixture();
        assert!(Descriptor::decode(&d.encode().unwrap()[..32]).unwrap_err()
                                                              .0
                                                              .contains("obsolete ELF-backed"));
        assert!(decode(b"\x7fELF\x02\x01\x01\0", &d).unwrap_err().0.contains("ELF-backed"));
        let mut unsupported = d.encode().unwrap();
        unsupported[5] = 2;
        assert!(Descriptor::decode(&unsupported).is_err());
        assert!(LoadPlan::new(b"\x7fELF\x02\x01\x01\0", &d, Some(&table), d.load_base).is_err());
    }
}
