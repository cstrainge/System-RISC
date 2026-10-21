use mac_fork_file::*;
#[derive(Default)]
struct Bytes
{
    bytes:      Vec<u8>,
    writes:     usize,
    reads:      usize,
    flushes:    usize,
    fail_flush: bool
}
impl ReadAt for Bytes
{
    fn len(&self) -> u64
    {
        self.bytes.len() as u64
    }

    fn read_exact(&mut self, offset: u64, output: &mut [u8]) -> Result<()>
    {
        self.reads += 1;
        let start = usize::try_from(offset).map_err(|_| Error::Range)?;
        let end = start.checked_add(output.len()).ok_or(Error::Range)?;
        output.copy_from_slice(self.bytes.get(start..end).ok_or(Error::Io)?);
        Ok(())
    }
}
impl WriteAt for Bytes
{
    fn set_len(&mut self, length: u64) -> Result<()>
    {
        let length = usize::try_from(length).map_err(|_| Error::Range)?;
        self.bytes.resize(length, 0);
        self.writes += 1;
        Ok(())
    }

    fn write_all(&mut self, offset: u64, input: &[u8]) -> Result<()>
    {
        let start = usize::try_from(offset).map_err(|_| Error::Range)?;
        let end = start.checked_add(input.len()).ok_or(Error::Range)?;
        self.bytes.get_mut(start..end).ok_or(Error::Io)?.copy_from_slice(input);
        self.writes += 1;
        Ok(())
    }

    fn flush(&mut self) -> Result<()>
    {
        self.flushes += 1;
        if self.fail_flush { Err(Error::Io) } else { Ok(()) }
    }
}
fn metadata() -> Metadata
{
    let mut value =
        Metadata::new(Kind::File, 16, [0x23; 16], "Cafe\u{301}", 3_875_171_696_123_456).unwrap();
    value.backup = Some(0);
    value.comment = "A comment 🦀".into();
    value.attributes = 1;
    value.finder_info = core::array::from_fn(|i| i as u8);
    value.modified += 9;
    value
}
fn payload(n: usize, salt: u8) -> Vec<u8>
{
    (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(salt)).collect()
}
fn container() -> (Bytes, Vec<u8>, Vec<u8>)
{
    let data = payload(CHUNK_BYTES * 3 + 17, 9);
    let resource = payload(CHUNK_BYTES + 13, 77);
    let mut writer = Writer::begin(Bytes::default(),
                                   &metadata(),
                                   data.len() as u64,
                                   resource.len() as u64).unwrap();
    for bytes in data.chunks(CHUNK_BYTES)
    {
        writer.write_chunk(Fork::Data, bytes).unwrap();
    }
    for bytes in resource.chunks(CHUNK_BYTES)
    {
        writer.write_chunk(Fork::Resource, bytes).unwrap();
    }
    writer.finish().unwrap();
    (writer.into_inner(), data, resource)
}
#[test]
fn both_forks_metadata_and_random_seeks_roundtrip_without_fork_sized_buffers()
{
    let (mut source, data, resources) = container();
    let mut opened = Opened::open(&mut source).unwrap();
    assert_eq!(opened.metadata(), &metadata());
    assert_eq!(opened.metadata().name, "Café");
    assert_eq!(opened.metadata().backup, Some(0));
    assert_eq!(opened.layout().data.offset, METADATA_BYTES as u64);
    opened.verify().unwrap();
    for (kind, expected) in [(Fork::Data, data), (Fork::Resource, resources)]
    {
        for (offset, length) in [(0, 29),
                                 (4095, 100),
                                 (4096, 64),
                                 (1, 4096),
                                 (expected.len() - 1, 100),
                                 (0, expected.len())]
        {
            let mut actual = vec![0xcc; length];
            let count = opened.read(kind, offset as u64, &mut actual).unwrap();
            assert_eq!(&actual[..count], &expected[offset..offset + count]);
            assert!(actual[count..].iter().all(|&b| b == 0xcc));
        }
        assert_eq!(opened.read(kind, u64::MAX, &mut [0; 1]), Ok(0));
    }
}
#[test]
fn metadata_chunk_and_hash_corruption_are_never_returned_as_valid_bytes()
{
    let (source, _, _) = container();
    for offset in
        [0, 8, 12, 16, 20, 24, 32, 64, 72, 128, 132, 136, 152, 184, 192, 196, 200, 232, 256, 700]
    {
        let mut bad = Bytes { bytes: source.bytes.clone(),
                              ..Default::default() };
        bad.bytes[offset] ^= 0x40;
        assert!(Opened::open(&mut bad).is_err(), "offset {offset}");
    }
    let mut valid = Bytes { bytes: source.bytes.clone(),
                            ..Default::default() };
    let opened = Opened::open(&mut valid).unwrap();
    for fork in [opened.layout().data, opened.layout().resources]
    {
        for offset in [fork.offset, fork.offset + fork.length - 1, fork.hashes]
        {
            let mut bad = Bytes { bytes: source.bytes.clone(),
                                  ..Default::default() };
            bad.bytes[offset as usize] ^= 1;
            let mut opened = Opened::open(&mut bad).unwrap();
            assert_eq!(opened.verify(), Err(Error::Corrupt));
        }
    }
    for length in [0, 1, 255, 256, 1023, source.bytes.len() - 1]
    {
        let mut bad = Bytes { bytes: source.bytes[..length].to_vec(),
                              ..Default::default() };
        assert!(Opened::open(&mut bad).is_err());
    }
}
#[test]
fn checked_lengths_and_nfc_limits_fail_before_mutating_target()
{
    let meta = metadata();
    assert_eq!(Layout::new(&meta, u64::MAX, 0), Err(Error::Range));
    assert!(Metadata::new(Kind::File, 16, [2; 16], &"e\u{301}".repeat(127), 0).is_ok());
    assert!(Metadata::new(Kind::File, 16, [2; 16], &"e\u{301}".repeat(128), 0).is_err());
    let mut invalid = meta.clone();
    invalid.name = "e\u{301}".into();
    assert!(Writer::begin(Bytes::default(), &invalid, 0, 0).is_err());
    for name in ["", ".", "..", "a/b", "a:b", "a\0b"]
    {
        assert!(Metadata::new(Kind::File, 16, [2; 16], name, 0).is_err());
    }
    let mut renamed = meta.clone();
    assert!(renamed.set_name(&"x".repeat(256)).is_err());
    assert_eq!(renamed, meta);
}
#[test]
fn incomplete_writes_are_invalid_and_failed_flush_can_be_retried_without_rewriting()
{
    let mut writer = Writer::begin(Bytes::default(), &metadata(), 17, 1).unwrap();
    assert_eq!(writer.finish(), Err(Error::Incomplete));
    assert!(!writer.is_finished());
    assert!(Opened::open(writer.target()).is_err());
    let writes = writer.target().writes;
    assert_eq!(writer.write_chunk(Fork::Data, &[0; 16]), Err(Error::Invalid));
    assert_eq!(writer.target().writes, writes);
    writer.write_chunk(Fork::Data, &[3; 17]).unwrap();
    writer.write_chunk(Fork::Resource, &[4; 1]).unwrap();
    writer.target().fail_flush = true;
    assert_eq!(writer.finish(), Err(Error::Io));
    assert!(!writer.is_finished());
    let writes = writer.target().writes;
    writer.target().fail_flush = false;
    writer.finish().unwrap();
    assert_eq!(writer.target().writes, writes);
    let flushes = writer.target().flushes;
    writer.finish().unwrap();
    assert_eq!(writer.target().flushes, flushes);
    assert_eq!(writer.write_chunk(Fork::Data, &[]), Err(Error::Invalid));
}
#[test]
fn host_tokens_cover_reserved_and_maximum_length_guest_names_without_collisions()
{
    for name in ["CON", "AUX.", "LPT1 ", "Café", "café"]
    {
        let meta = Metadata::new(Kind::File, 16, [2; 16], name, 0).unwrap();
        let token = host_name(meta.kind, meta.identity).unwrap();
        assert_eq!(token.len(), 34);
        assert!(token.iter().all(u8::is_ascii));
        assert_eq!(parse_host_name(&token), Ok((meta.kind, meta.identity)));
    }
    let name = "a".repeat(255);
    assert!(Metadata::new(Kind::File, 16, [2; 16], &name, 0).is_ok());
    assert_ne!(host_name(Kind::File, [1; 16]), host_name(Kind::File, [2; 16]));
    assert_ne!(host_name(Kind::File, [1; 16]), host_name(Kind::Directory, [1; 16]));
    let upper = host_name(Kind::File, [0xfa; 16]).unwrap().to_ascii_uppercase();
    assert!(parse_host_name(&upper).is_err());
}
#[test]
fn root_and_directory_records_cannot_contain_forks_or_reused_root_ids()
{
    let root = Metadata::new(Kind::Root, 2, [9; 16], "Shared Volume", 0).unwrap();
    let mut writer = Writer::begin(Bytes::default(), &root, 0, 0).unwrap();
    writer.finish().unwrap();
    let opened = Opened::open(writer.target()).unwrap();
    assert_eq!(opened.metadata(), &root);
    assert!(Writer::begin(Bytes::default(), &root, 1, 0).is_err());
    assert!(Metadata::new(Kind::Directory, 2, [2; 16], "Dir", 0).is_err());
    let mut invalid = root.clone();
    invalid.identity = [2; 16];
    assert_eq!(invalid.validate(), Err(Error::Invalid));
}

#[test]
fn physical_chunk_hashes_are_interleaved_for_sequential_io()
{
    let data = payload(8193, 51);
    let resources = [72, 73];
    let mut writer = Writer::begin(Bytes::default(), &metadata(), data.len() as u64, 2).unwrap();
    for chunk in data.chunks(4096)
    {
        writer.write_chunk(Fork::Data, chunk).unwrap();
    }
    writer.write_chunk(Fork::Resource, &resources).unwrap();
    writer.finish().unwrap();
    let source = writer.into_inner();
    assert_eq!(source.bytes.len(), 9347);
    assert_eq!(&source.bytes[1024..5120], &data[..4096]);
    assert_eq!(&source.bytes[5152..9248], &data[4096..8192]);
    assert_eq!(source.bytes[9280], data[8192]);
    assert_eq!(&source.bytes[9313..9315], &resources);
    assert_eq!(u64::from_le_bytes(source.bytes[160..168].try_into().unwrap()), 5120);
    assert_eq!(u64::from_le_bytes(source.bytes[168..176].try_into().unwrap()), 9313);
}


#[test]
fn each_metadata_or_chunk_record_uses_one_transfer_and_flush_is_still_real()
{
    let (mut source, data, resources) = container();
    let chunks = data.len().div_ceil(CHUNK_BYTES) + resources.len().div_ceil(CHUNK_BYTES);
    // Length, invalid metadata record, each complete chunk, then the valid header.
    assert_eq!(source.writes, chunks + 3);
    assert_eq!(source.flushes, 1);
    let mut opened = Opened::open(&mut source).unwrap();
    let mut bytes = vec![0; data.len().max(resources.len())];
    opened.read(Fork::Data, 0, &mut bytes[..data.len()]).unwrap();
    assert_eq!(&bytes[..data.len()], data);
    opened.read(Fork::Resource, 0, &mut bytes[..resources.len()]).unwrap();
    assert_eq!(&bytes[..resources.len()], resources);
    assert_eq!(opened.into_inner().reads, chunks + 1);
}

#[test]
fn pure_chunk_records_reject_bad_ranges_before_modifying_destination()
{
    let mut encoded = [0xa5; CHUNK_BYTES + HASH_BYTES as usize];
    assert_eq!(encode_chunk(b"abc", 4, &mut encoded), Err(Error::Invalid));
    assert!(encoded.iter().all(|&byte| byte == 0xa5));
    assert_eq!(encode_chunk(&[], 0, &mut encoded), Err(Error::Range));
    assert_eq!(encode_chunk(b"abc", 3, &mut encoded[..34]), Err(Error::Range));
    let length = encode_chunk(b"abc", 3, &mut encoded).unwrap();
    assert_eq!(length, 35);
    assert_eq!(decode_chunk(&encoded[..length], 3), Ok(&b"abc"[..]));
    assert!(decode_chunk(&encoded[..length - 1], 3).is_err());
    assert!(decode_chunk(&encoded[..length + 1], 3).is_err());
    encoded[0] ^= 1;
    assert_eq!(decode_chunk(&encoded[..length], 3), Err(Error::Corrupt));
}
