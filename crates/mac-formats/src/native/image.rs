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
        count = count.checked_add(1).ok_or(Error::Malformed("too many RV64 segments"))?;
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
        // Clear BSS, inter-segment holes and trailing reservation bytes before
        // copying initialized payload. Unpublished owner survives any refusal.
        output.clear().map_err(WriteError::Destination)?;
        for segment in &self.image.segments
        {
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

#[cfg(test)]
mod tests
{
    use alloc::vec;

    use super::*;
    fn descriptor() -> Descriptor
    {
        Descriptor { role:        super::super::Role::CodeResource,
                     abi_version: super::super::ABI_VERSION,
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
    #[derive(Debug, PartialEq, Eq)]
    enum StorageRefusal
    {
        MissingPage(usize),
        Write(usize),
        Range
    }
    impl core::fmt::Display for StorageRefusal
    {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
        {
            match self
            {
                Self::MissingPage(page) => write!(f, "image storage page {page} is unavailable"),
                Self::Write(call) => write!(f, "image storage refused write operation {call}"),
                Self::Range => f.write_str("image storage write is outside its reservation")
            }
        }
    }
    impl core::error::Error for StorageRefusal
    {
    }
    struct Pages
    {
        pages:         Vec<Option<alloc::boxed::Box<[u8; 4096]>>>,
        length:        usize,
        clears:        usize,
        writes:        usize,
        fail_on_write: Option<usize>
    }
    impl Pages
    {
        fn new(length: usize) -> Self
        {
            Self { pages: (0..length.div_ceil(4096)).map(|_| {
                                                        Some(alloc::boxed::Box::new([0xa5; 4096]))
                                                    })
                                                    .collect(),
                   length,
                   clears: 0,
                   writes: 0,
                   fail_on_write: None }
        }

        fn bytes(&self) -> Vec<u8>
        {
            self.pages
                .iter()
                .flat_map(|page| page.as_ref().unwrap().iter().copied())
                .take(self.length)
                .collect()
        }
    }
    impl Destination for Pages
    {
        type Error = StorageRefusal;

        fn length(&self) -> usize
        {
            self.length
        }

        fn clear(&mut self) -> core::result::Result<(), StorageRefusal>
        {
            self.clears += 1;
            for (index, page) in self.pages.iter_mut().enumerate()
            {
                page.as_mut().ok_or(StorageRefusal::MissingPage(index))?.fill(0);
            }
            Ok(())
        }

        fn write(&mut self, offset: usize, bytes: &[u8])
                 -> core::result::Result<(), StorageRefusal>
        {
            if offset.checked_add(bytes.len()).is_none_or(|end| end > self.length)
            {
                return Err(StorageRefusal::Range);
            }
            self.writes += 1;
            if self.fail_on_write == Some(self.writes)
            {
                return Err(StorageRefusal::Write(self.writes));
            }
            let mut done = 0;
            while done < bytes.len()
            {
                let index = (offset + done) / 4096;
                let within = (offset + done) % 4096;
                let count = (4096 - within).min(bytes.len() - done);
                let page = self.pages
                               .get_mut(index)
                               .and_then(Option::as_mut)
                               .ok_or(StorageRefusal::MissingPage(index))?;
                page[within..within + count].copy_from_slice(&bytes[done..done + count]);
                done += count;
            }
            Ok(())
        }
    }
    #[test]
    fn page_backing_preserves_execution_relocations_zeros_and_typed_write_refusals()
    {
        use alloc::string::ToString;
        let mut descriptor = descriptor();
        descriptor.load_limit = descriptor.load_base + 16384;
        let code = vec![0x82; 6000];
        let pointer = (descriptor.load_base + 0x800).to_le_bytes();
        let image =
            encode(&descriptor, [(0, 5, code.as_slice()), (8192, 6, pointer.as_slice())]).unwrap();
        let mut relocations = b"RLC1".to_vec();
        relocations.extend_from_slice(&1u32.to_le_bytes());
        relocations.extend_from_slice(&8192u32.to_le_bytes());
        relocations.extend_from_slice(&[8, 0, 0, 0]);
        let plan = LoadPlan::new(&image, &descriptor, Some(&relocations), 0x400000).unwrap();
        let mut expected = vec![0xa5; 16384];
        plan.write(&mut expected).unwrap();
        assert_eq!(&expected[..6000], code.as_slice());
        assert_eq!(&expected[8192..8200], &(0x400800u64).to_le_bytes());
        assert!(expected[6000..8192].iter().chain(&expected[8200..]).all(|b| *b == 0));
        let mut output = Pages::new(16384);
        plan.write_to(&mut output).unwrap();
        assert_eq!(output.bytes(), expected);
        assert_eq!(plan.entry(), 0x400000);
        let mut short = Pages::new(16383);
        assert!(matches!(plan.write_to(&mut short), Err(WriteError::Format(Error::Malformed(_)))));
        assert_eq!((short.clears, short.writes), (0, 0));
        assert!(short.bytes().iter().all(|b| *b == 0xa5));
        let mut missing = Pages::new(16384);
        missing.pages[2] = None;
        let error = plan.write_to(&mut missing).unwrap_err();
        assert!(matches!(error, WriteError::Destination(StorageRefusal::MissingPage(2))));
        assert!(error.to_string().contains("page 2 is unavailable"));
        assert_eq!(missing.writes, 0, "payload copied after a failed clear");
        let mut retry = Pages::new(16384);
        retry.fail_on_write = Some(3); // fail specifically on relocation publication
        let error = plan.write_to(&mut retry).unwrap_err();
        assert!(matches!(error, WriteError::Destination(StorageRefusal::Write(3))));
        assert!(error.to_string().contains("write operation 3"));
        assert_eq!(core::error::Error::source(&error).unwrap().to_string(),
                   "image storage refused write operation 3");
        retry.fail_on_write = None;
        retry.writes = 0;
        plan.write_to(&mut retry).unwrap();
        assert_eq!(retry.bytes(), expected, "retained page preparation could not retry");
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
        assert!(matches!(Descriptor::decode(&d.encode().unwrap()[..32]).map_err(Error::from),
                         Err(Error::Malformed(_))));
        assert!(matches!(decode(b"\x7fELF\x02\x01\x01\0", &d), Err(Error::Malformed(_))));
        let mut unsupported = d.encode().unwrap();
        unsupported[5] = 2;
        assert!(Descriptor::decode(&unsupported).is_err());
        assert!(LoadPlan::new(b"\x7fELF\x02\x01\x01\0", &d, Some(&table), d.load_base).is_err());
    }
}
