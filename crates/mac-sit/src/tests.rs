use alloc::vec::Vec;

use super::*;
const NESTED: &[u8] = include_bytes!("../tests/fixtures/nested-v1.sit");

fn catalog(bytes: &[u8], limits: Limits) -> Result<(Index, usize)>
{
    let mut builder = IndexBuilder::new(bytes.len() as u64, limits)?;
    let mut read = 0;
    while let Some(request) = builder.request()?
    {
        let offset = usize::try_from(request.offset).map_err(|_| Error::Bounds)?;
        builder.feed(bytes.get(offset..offset + request.length).ok_or(Error::Truncated)?)?;
        read += request.length;
    }
    Ok((builder.finish()?, read))
}
fn checksum(header: &mut [u8; ENTRY_HEADER_BYTES])
{
    let crc = Crc16::checksum(&header[..110]);
    header[110..].copy_from_slice(&crc.to_be_bytes());
}
fn archive(headers: &[[u8; ENTRY_HEADER_BYTES]], count: u16) -> Vec<u8>
{
    let mut bytes = alloc::vec![0; ARCHIVE_HEADER_BYTES];
    bytes[..4].copy_from_slice(b"SIT!");
    bytes[4..6].copy_from_slice(&count.to_be_bytes());
    let length = (ARCHIVE_HEADER_BYTES + headers.len() * ENTRY_HEADER_BYTES) as u32;
    bytes[6..10].copy_from_slice(&length.to_be_bytes());
    bytes[10..14].copy_from_slice(b"rLau");
    bytes[14] = 1;
    for header in headers
    {
        bytes.extend_from_slice(header);
    }
    bytes
}
fn entry(resource: u8, data: u8) -> [u8; ENTRY_HEADER_BYTES]
{
    let mut header = [0; ENTRY_HEADER_BYTES];
    header[0] = resource;
    header[1] = data;
    header[2] = 1;
    header[3] = b'A';
    checksum(&mut header);
    header
}
#[test]
fn original_writer_nested_archive_is_indexed_by_headers_without_reading_payloads()
{
    let (index, read) = catalog(NESTED, Limits::default()).unwrap();
    assert_eq!(index.header().top_level_count, 1, "root folder counts once, not seven headers");
    assert_eq!(index.header().length, NESTED.len() as u64);
    assert_eq!(index.nodes().len(), 5);
    assert_eq!(read, ARCHIVE_HEADER_BYTES + 7 * ENTRY_HEADER_BYTES);
    let mut utf8 = [0; 189];
    let top = index.get(FIRST_ENTRY_ID).unwrap();
    assert_eq!((top.parent, top.child_count, top.name().utf8(&mut utf8).unwrap()),
               (ROOT_ID, 3, "Top"));
    let sub = index.get(17).unwrap();
    assert_eq!((sub.parent, sub.child_count, sub.name().utf8(&mut utf8).unwrap()), (16, 1, "Sub"));
    let Contents::File(file) = index.get(18).unwrap().contents
    else
    {
        panic!("expected Pattern");
    };
    assert_eq!(file.name.utf8(&mut utf8).unwrap(), "Pattern");
    assert_eq!(file.data.length, 8192);
    assert_eq!(file.data.method(), Ok(Method::Lzw));
    assert_eq!((file.file_type, file.creator), (*b"TEXT", *b"TEST"));
    assert_eq!(file.resource.length, 0);
    assert_eq!(file.data.offset, file.resource.end().unwrap());
    assert_eq!(index.get(19).unwrap().name().utf8(&mut utf8).unwrap(), "Café");
    assert!(index.get(ROOT_ID).is_none() && index.get(15).is_none() && index.get(21).is_none());
    // Real folder subtree hints are nonzero. They are not fork payload lengths.
    assert!(u32::from_be_bytes(NESTED[22 + 96..22 + 100].try_into().unwrap()) > 8000);
}
#[test]
fn ibm_crc_matches_the_published_check_and_real_archive_headers()
{
    assert_eq!(Crc16::checksum(b"123456789"), 0xbb3d);
    assert_eq!(Crc16::checksum(&[]), 0);
    let mut crc = Crc16::new();
    crc.update(b"1234");
    crc.update(b"56789");
    assert_eq!(crc.value(), 0xbb3d);
    assert_eq!(Crc16::checksum(&NESTED[22..132]), u16::from_be_bytes([NESTED[132], NESTED[133]]));
    let mut bad = NESTED.to_vec();
    bad[22 + 104] ^= 1;
    assert_eq!(catalog(&bad, Limits::default()).err(), Some(Error::HeaderChecksum));
}
#[test]
fn sentinels_ignore_unspecified_fields_and_validate_nesting()
{
    let begin = entry(32, 32);
    let mut end = entry(255, 33);
    end[2] = 255;
    end[84..100].fill(255);
    checksum(&mut end);
    let (index, _) = catalog(&archive(&[begin, end], 1), Limits::default()).unwrap();
    assert_eq!(index.nodes().len(), 1);
    assert_eq!(catalog(&archive(&[end], 0), Limits::default()).err(), Some(Error::Hierarchy));
    assert_eq!(catalog(&archive(&[begin], 1), Limits::default()).err(), Some(Error::Hierarchy));
    assert_eq!(catalog(&archive(&[entry(0, 0)], 0), Limits::default()).err(), Some(Error::Count));
}
#[test]
fn both_signatures_and_only_the_documented_container_version_are_accepted()
{
    for at in [0, 10]
    {
        let mut bytes = archive(&[], 0);
        bytes[at] ^= 1;
        assert_eq!(ArchiveHeader::decode(&bytes, bytes.len() as u64), Err(Error::Signature));
    }
    for version in [0, 2, 5, 255]
    {
        let mut bytes = archive(&[], 0);
        bytes[14] = version;
        assert_eq!(ArchiveHeader::decode(&bytes, bytes.len() as u64),
                   Err(Error::UnsupportedVersion(version)));
    }
    let mut bytes = archive(&[], 0);
    bytes[15..].fill(255);
    assert!(catalog(&bytes, Limits::default()).unwrap().0.nodes().is_empty());
}
#[test]
fn overflowing_fork_ranges_and_invalid_names_fail_without_panics_or_more_reads()
{
    let mut file = entry(0, 0);
    file[96..100].copy_from_slice(&u32::MAX.to_be_bytes());
    checksum(&mut file);
    assert_eq!(catalog(&archive(&[file], 1), Limits::default()).err(), Some(Error::Bounds));
    for length in [0, 64, 255]
    {
        let mut file = entry(0, 0);
        file[2] = length;
        checksum(&mut file);
        assert_eq!(catalog(&archive(&[file], 1), Limits::default()).err(), Some(Error::Name));
    }
    let mut builder = IndexBuilder::new(NESTED.len() as u64,
                                        Limits { entries: 1,
                                                 depth:   64 }).unwrap();
    builder.feed(&NESTED[..22]).unwrap();
    builder.feed(&NESTED[22..134]).unwrap();
    assert_eq!(builder.feed(&NESTED[134..246]), Err(Error::Limit));
    assert_eq!(builder.request(), Err(Error::InvalidState));
    assert_eq!(catalog(NESTED,
                       Limits { entries: 4096,
                                depth:   1 }).err(),
               Some(Error::Limit));
    assert_eq!(IndexBuilder::new(21, Limits::default()).err().map(|_| Error::Truncated),
               Some(Error::Truncated));
}
#[test]
fn encrypted_and_unknown_forks_remain_in_the_catalog_with_typed_read_failures()
{
    for (method, expected) in [(16, Error::Encrypted),
                               (19, Error::Encrypted),
                               (4, Error::UnsupportedMethod(4)),
                               (128, Error::UnsupportedMethod(128))]
    {
        let (index, _) = catalog(&archive(&[entry(0, method)], 1), Limits::default()).unwrap();
        let Contents::File(file) = index.nodes()[0].contents
        else
        {
            panic!("file expected");
        };
        assert_eq!(file.data.method(), Err(expected));
        assert_eq!(file.resource.method(), Ok(Method::Stored));
    }
    assert_eq!(mac_microseconds(3_875_171_696), Ok(3_875_171_696_000_000));
}

fn expand(fork: Fork, packed: &[u8], input_chunk: usize, output_chunk: usize) -> Result<Vec<u8>>
{
    let mut decoder = Decoder::new(fork)?;
    let mut at = 0;
    let mut output = Vec::new();
    let mut buffer = alloc::vec![0; output_chunk];
    loop
    {
        let end = packed.len().min(at + input_chunk);
        let progress = decoder.step(&packed[at..end], &mut buffer)?;
        at += progress.consumed;
        output.extend_from_slice(&buffer[..progress.written]);
        if progress.complete
        {
            assert_eq!(at, packed.len());
            return Ok(output);
        }
        if progress.consumed == 0 && progress.written == 0
        {
            return Err(Error::Truncated);
        }
    }
}
fn stream(method: u8, packed: &[u8], expected: &[u8]) -> Fork
{
    Fork { offset:        0,
           packed_length: packed.len() as u64,
           length:        expected.len() as u64,
           crc:           Crc16::checksum(expected),
           method_byte:   method }
}
#[test]
fn original_writer_forks_expand_across_every_small_input_and_output_boundary()
{
    let (index, _) = catalog(NESTED, Limits::default()).unwrap();
    let pattern: Vec<u8> = (0..8192).map(|i| i as u8).collect();
    for node in index.nodes()
    {
        let Contents::File(file) = node.contents
        else
        {
            continue;
        };
        let fork = file.data;
        let packed = &NESTED[fork.offset as usize..fork.end().unwrap() as usize];
        for (input, output) in [(1, 1), (2, 17), (13, 7), (14, 4096), (4096, 8192)]
        {
            let result = expand(fork, packed, input, output).unwrap();
            assert_eq!(result.len() as u64, fork.length);
            match node.id
            {
                18 => assert_eq!(result, pattern),
                19 => assert_eq!(result, b"Native MacRoman name\r"),
                20 => assert_eq!(result, include_bytes!("../tests/fixtures/stored.bin")),
                _ => panic!("unexpected file")
            }
        }
        let empty = file.resource;
        assert!(expand(empty, &[], 1, 1).unwrap().is_empty());
    }
}

#[test]
fn real_archive_keeps_resource_and_data_forks_distinct_and_detects_payload_corruption()
{
    let archive = include_bytes!("../tests/fixtures/forks-v1.sit");
    let (index, _) = catalog(archive, Limits::default()).unwrap();
    let Contents::File(file) = index.nodes()[0].contents
    else
    {
        panic!("expected file");
    };
    assert_eq!(file.file_type, *b"APPL");
    let resource: Vec<u8> = (0..2048).map(|i| i as u8).collect();
    for (fork, expected) in [(file.resource, resource.as_slice()), (file.data, &b"Data fork\r"[..])]
    {
        let packed = &archive[fork.offset as usize..fork.end().unwrap() as usize];
        assert_eq!(expand(fork, packed, 1, 1).unwrap(), expected);
        let mut corrupt = packed.to_vec();
        corrupt[0] ^= 1;
        assert_eq!(expand(fork, &corrupt, 7, 13), Err(Error::ForkChecksum));
        assert_eq!(expand(fork, &packed[..packed.len() - 1], 7, 13), Err(Error::Truncated));
    }
}

fn push_bits(bits: &mut Vec<u8>, value: u16, count: usize)
{
    for at in (0..count).rev()
    {
        bits.push(((value >> at) & 1) as u8);
    }
}
fn packed_bits(bits: &[u8]) -> Vec<u8>
{
    bits.chunks(8)
        .map(|chunk| chunk.iter().enumerate().fold(0, |value, (i, bit)| value | bit << (7 - i)))
        .collect()
}
fn full_tree(bits: &mut Vec<u8>, first: u16, count: u16)
{
    if count == 1
    {
        push_bits(bits, 1, 1);
        push_bits(bits, first, 8);
    }
    else
    {
        push_bits(bits, 0, 1);
        full_tree(bits, first, count / 2);
        full_tree(bits, first + count / 2, count / 2);
    }
}
#[test]
fn huffman_branch_traversal_accepts_full_256_symbol_tree_at_all_chunk_boundaries()
{
    let mut bits = Vec::new();
    full_tree(&mut bits, 0, 256);
    let expected: Vec<u8> = (0..1024).map(|i| i as u8).collect();
    for byte in &expected
    {
        push_bits(&mut bits, u16::from(*byte), 8);
    }
    let packed = packed_bits(&bits);
    let fork = stream(3, &packed, &expected);
    for input in 1..=14
    {
        assert_eq!(expand(fork, &packed, input, 1).unwrap(), expected);
    }
}
#[test]
fn original_compress_stream_reaches_fourteen_bits_and_resets_without_buffering_the_fork()
{
    let archive = include_bytes!("../tests/fixtures/lzw-reset-v1.sit");
    let (index, _) = catalog(archive, Limits::default()).unwrap();
    let Contents::File(file) = index.nodes()[0].contents
    else
    {
        panic!("file expected");
    };
    let mut expected: Vec<u8> = (0..65536).map(|i| i as u8).collect();
    let mut state = 0x12345678u32;
    for _ in 0..131072
    {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        expected.push(state as u8);
    }
    expected.extend((0..131072).map(|i| i as u8));
    let fork = file.data;
    let packed = &archive[fork.offset as usize..fork.end().unwrap() as usize];
    for (input, output) in [(1, 31), (17, 1), (4096, 4096)]
    {
        assert_eq!(expand(fork, packed, input, output).unwrap(), expected);
    }
}
#[test]
fn stored_rle90_and_huffman_stream_with_length_and_crc_validation()
{
    let cases: &[(u8, &[u8], &[u8])] = &[(0, b"raw fork\0\x90", b"raw fork\0\x90"),
                                         (1, b"A\x90\x05B\x90\0\x90\x03", b"AAAAAB\x90\x90\x90"),
                                         // Preorder leaf 'A' = 1,01000001 then padding. No symbol
                                         // bits.
                                         (3, &[0xa0, 0x80], b"AAAAAAAA")];
    for &(method, packed, expected) in cases
    {
        for input in 1..=4
        {
            for output in 1..=4
            {
                let fork = stream(method, packed, expected);
                assert_eq!(expand(fork, packed, input, output).unwrap(), expected);
                assert_eq!(expand(Fork { crc: fork.crc ^ 1,
                                         ..fork },
                                  packed,
                                  input,
                                  output),
                           Err(Error::ForkChecksum));
            }
        }
    }
}
#[test]
fn malformed_streams_fail_and_cannot_resume_as_success()
{
    for (method, packed, expected, error) in
        [(1, &b"A\x90"[..], &b"A"[..], Error::Truncated),
         (1, &b"A\x90\x01"[..], &b"A"[..], Error::CompressedData),
         (1, &b"\x90\x02"[..], &b"A"[..], Error::CompressedData),
         (1, &b"A\x90\xff"[..], &b"A"[..], Error::Length),
         (2, &b"\xff\x01"[..], &b"A"[..], Error::CompressedData),
         (3, &b"\xa0"[..], &b"A"[..], Error::Truncated),
         (3, &b"\xa0\x80\0"[..], &b"A"[..], Error::Length)]
    {
        let fork = stream(method, packed, expected);
        assert_eq!(expand(fork, packed, 1, 1), Err(error));
    }
    let fork = stream(0, b"A", b"A");
    assert_eq!(Decoder::new(Fork { length: 2,
                                   ..fork }).err(),
               Some(Error::Length));
    let mut decoder = Decoder::new(Fork { crc: 0,
                                          ..fork }).unwrap();
    assert_eq!(decoder.step(b"A", &mut [0; 16]), Err(Error::ForkChecksum));
    assert_eq!(decoder.step(&[], &mut [0; 16]), Err(Error::InvalidState));
    assert_eq!(expand(stream(3, &[0; 640], b"A"), &[0; 640], 1, 1), Err(Error::Limit));
}
