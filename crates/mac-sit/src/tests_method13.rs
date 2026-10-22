use super::*;

fn data() -> Vec<u8>
{
    let mut bytes = alloc::vec![0; 3];
    bytes.extend((0..2048).map(|i| i as u8));
    bytes.push(0x90);
    bytes.extend_from_slice(&[b'X'; 100]);
    bytes.push(b'!');
    bytes
}
fn resource() -> Vec<u8>
{
    let mut bytes: Vec<_> = b"Resource fork\r".iter().copied().cycle().take(414).collect();
    bytes.extend_from_slice(&[0, 0x90]);
    bytes
}
fn forks(bytes: &[u8]) -> FileHeader
{
    let (index, _) = catalog(bytes, Limits::default()).unwrap();
    let Contents::File(file) = index.nodes()[0].contents
    else
    {
        panic!("file expected")
    };
    file
}
const SHARED: &[u8] = include_bytes!("../tests/fixtures/method13-shared-v5.sit");

#[test]
fn all_codebooks_and_dynamic_tables_preserve_both_forks_at_arbitrary_boundaries()
{
    let data = data();
    let resource = resource();
    for bytes in [include_bytes!("../tests/fixtures/method13-book1-v2.sit").as_slice(),
                  include_bytes!("../tests/fixtures/method13-book2-v2.sit").as_slice(),
                  include_bytes!("../tests/fixtures/method13-book3-v2.sit").as_slice(),
                  include_bytes!("../tests/fixtures/method13-book4-v2.sit").as_slice(),
                  include_bytes!("../tests/fixtures/method13-book5-v2.sit").as_slice(),
                  include_bytes!("../tests/fixtures/method13-book1-v5.sit").as_slice(),
                  include_bytes!("../tests/fixtures/method13-book2-v5.sit").as_slice(),
                  include_bytes!("../tests/fixtures/method13-book3-v5.sit").as_slice(),
                  include_bytes!("../tests/fixtures/method13-book4-v5.sit").as_slice(),
                  include_bytes!("../tests/fixtures/method13-book5-v5.sit").as_slice(),
                  include_bytes!("../tests/fixtures/method13-shared-v2.sit").as_slice(),
                  include_bytes!("../tests/fixtures/method13-independent-v5.sit").as_slice(),
                  SHARED,
                  include_bytes!("../tests/fixtures/method13-separate-v2.sit").as_slice(),
                  include_bytes!("../tests/fixtures/method13-separate-v5.sit").as_slice()]
    {
        let file = forks(bytes);
        for (fork, expected) in [(file.data, &data), (file.resource, &resource)]
        {
            assert_eq!(fork.method(), Ok(Method::LzHuffman));
            let packed = &bytes[fork.offset as usize..fork.end().unwrap() as usize];
            for (input, output) in [(1, 1), (2, 17), (7, 31), (4096, 4096)]
            {
                assert_eq!(expand(fork, packed, input, output).unwrap(), *expected);
            }
        }
    }
}

#[test]
fn maximum_distance_and_length_wrap_the_history_without_unbounded_steps()
{
    let bytes = include_bytes!("../tests/fixtures/method13-window-v5.sit");
    let fork = forks(bytes).data;
    let packed = &bytes[fork.offset as usize..fork.end().unwrap() as usize];
    let expected: Vec<_> = (0..99969).map(|i| i as u8).collect();
    for (input, output) in [(1, 1), (7, 129), (4096, 8192)]
    {
        assert_eq!(expand(fork, packed, input, output).unwrap(), expected);
    }
    let mut decoder = Decoder::new(fork).unwrap();
    let (mut at, mut total, mut steps) = (0, 0, 0);
    let mut buffer = [0; 8192];
    loop
    {
        let progress = decoder.step(&packed[at..], &mut buffer).unwrap();
        assert!(progress.consumed <= MAX_STEP && progress.written <= MAX_STEP);
        at += progress.consumed;
        total += progress.written;
        steps += 1;
        assert!(steps < 100, "decoder stopped making bounded progress");
        if progress.complete
        {
            break;
        }
    }
    assert_eq!(total, expected.len());
    assert!(core::mem::size_of::<Decoder>() < 512);
}

#[test]
fn corruption_length_mismatch_and_every_truncation_fail_without_poisoning_other_forks()
{
    let file = forks(SHARED);
    let packed = &SHARED[file.data.offset as usize..file.data.end().unwrap() as usize];
    // Every prefix before the complete logical output must reject truncation;
    // final end-code/padding bytes are optional when all declared bytes exist.
    for length in 0..packed.len() - 2
    {
        assert!(expand(file.data, &packed[..length], 1, 31).is_err(), "prefix {length}");
    }
    for delta in [-1, 1]
    {
        let fork = Fork { length: (file.data.length as i64 + delta) as u64,
                          ..file.data };
        assert_eq!(expand(fork, packed, 7, 129),
                   Err(if delta < 0 { Error::ForkChecksum } else { Error::Length }));
    }
    assert_eq!(expand(Fork { length: 1000,
                             ..file.data },
                      packed,
                      7,
                      129),
               Err(Error::Length));
    let bad = include_bytes!("../tests/fixtures/method13-bad-crc-v5.sit");
    let bad_file = forks(bad);
    let bad_fork = bad_file.data;
    let packed = &bad[bad_fork.offset as usize..bad_fork.end().unwrap() as usize];
    let mut decoder = Decoder::new(bad_fork).unwrap();
    assert_eq!(decoder.step(packed, &mut [0; 4096]), Err(Error::ForkChecksum));
    assert_eq!(decoder.step(&[], &mut [0; 4096]), Err(Error::InvalidState));
    let packed = &bad[bad_file.resource.offset as usize..bad_file.resource.end().unwrap() as usize];
    assert_eq!(expand(bad_file.resource, packed, 1, 1).unwrap(), resource());
    let packed = &SHARED[file.data.offset as usize..file.data.end().unwrap() as usize];
    assert_eq!(expand(file.data, packed, 1, 1).unwrap(), data());
}

// Explicit little-endian meta codes, independent of the decoder's table/tree.
fn meta(bits: &mut Vec<u8>, symbol: u8)
{
    let (code, count) = match symbol
    {
        0 => (0x5d8, 11),
        8 => (0x00c, 5),
        31 => (0x004, 5),
        32 => (0x001, 2),
        33 => (0x002, 2),
        36 => (0x008, 5),
        _ => panic!("unused test meta code")
    };
    for at in 0..count
    {
        bits.push(((code >> at) & 1) as u8);
    }
}
fn little_bytes(bits: &[u8]) -> Vec<u8>
{
    bits.chunks(8).map(|part| part.iter().enumerate().fold(0, |v, (i, b)| v | b << i)).collect()
}
#[test]
fn malformed_book_lengths_oversubscription_and_run_overflow_are_typed_failures()
{
    for book in 6..=15
    {
        let packed = [book << 4];
        assert_eq!(expand(stream(13, &packed, b"A"), &packed, 1, 1), Err(Error::CompressedData));
    }
    let mut streams = Vec::new();
    // The initial length zero cannot be decremented below -1.
    let mut bits = Vec::new();
    meta(&mut bits, 33);
    meta(&mut bits, 33);
    streams.push(bits);
    // Three one-bit codes oversubscribe the prefix space.
    let mut bits = Vec::new();
    meta(&mut bits, 0);
    meta(&mut bits, 0);
    meta(&mut bits, 0);
    for _ in 3..321
    {
        meta(&mut bits, 31);
    }
    streams.push(bits);
    // A fifth 74-element run exceeds the first table's 321-element bound.
    let mut bits = Vec::new();
    meta(&mut bits, 8);
    for _ in 0..5
    {
        meta(&mut bits, 36);
        bits.extend_from_slice(&[1; 6]);
    }
    streams.push(bits);
    // Thirty-three increments exceed the maximum 32-bit code length.
    let mut bits = Vec::new();
    for _ in 0..33
    {
        meta(&mut bits, 32);
    }
    streams.push(bits);
    for bits in streams
    {
        let mut packed = alloc::vec![0x0f];
        packed.extend_from_slice(&little_bytes(&bits));
        assert_eq!(expand(stream(13, &packed, b"A"), &packed, 1, 1), Err(Error::CompressedData));
    }
}
