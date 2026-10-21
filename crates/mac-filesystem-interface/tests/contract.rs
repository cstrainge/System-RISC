use mac_filesystem_interface::{self as fs, Arguments, Error, Metadata, Mount};
fn metadata() -> Metadata<'static>
{
    Metadata { id:              16,
               parent:          2,
               directory:       false,
               data_length:     5_000_000_001,
               resource_length: 9000,
               created:         3_875_171_696_123_456,
               modified:        3_875_171_697_987_654,
               backup:          Some(0),
               child_count:     0,
               attributes:      1,
               finder_info:     [0x53; 32],
               revision:        u64::MAX,
               name:            "Café 🦀",
               comment:         "Both forks" }
}
#[test]
fn requests_preserve_64_bit_offsets_and_reject_stale_or_noncanonical_envelopes()
{
    let value = Arguments { volume:    0x1234_0001,
                            node:      16,
                            token:     0x1235_0001,
                            parent:    2,
                            flags:     fs::RESOURCE_FORK,
                            offset:    5_000_000_000,
                            length:    u64::MAX,
                            auxiliary: 9000,
                            when:      3_875_171_696_123_456,
                            payload:   "Café".as_bytes() };
    let mut bytes = [0; 512];
    let n = value.encode(&mut bytes).unwrap();
    assert_eq!(Arguments::decode(&bytes[..n]), Ok(value));
    assert_eq!(Arguments::decode(&bytes[..n]).unwrap().name(), Ok("Café"));
    for len in 0..n
    {
        assert!(Arguments::decode(&bytes[..len]).is_err());
    }
    assert_eq!(Arguments::decode(&bytes[..n + 1]), Err(Error::Length));
    bytes[60] = 1;
    assert_eq!(Arguments::decode(&bytes[..n]), Err(Error::Invalid));
    bytes[60] = 0;
    bytes[0] = 0;
    assert_eq!(Arguments::decode(&bytes[..n]), Err(Error::Version));
    let too_big = [0; fs::MAX_TRANSFER + 1];
    let mut untouched = [0xa5; 64];
    assert_eq!(Arguments { payload: &too_big,
                           ..Default::default() }.encode(&mut untouched),
               Err(Error::Length));
    assert_eq!(untouched, [0xa5; 64]);
}
#[test]
fn catalog_metadata_round_trips_both_forks_native_names_and_checked_dates()
{
    let value = metadata();
    let mut bytes = [0; 1024];
    let n = value.encode(&mut bytes).unwrap();
    assert_eq!(&bytes[16..24], &value.data_length.to_le_bytes());
    assert_eq!(Metadata::decode(&bytes[..n]), Ok(value));
    for len in 0..n
    {
        assert!(Metadata::decode(&bytes[..len]).is_err());
    }
    assert!(Metadata::decode(&bytes[..n + 1]).is_err());
    bytes[70] = 1;
    assert_eq!(Metadata::decode(&bytes[..n]), Err(Error::Invalid));
    bytes[70] = 0;
    bytes[56] = 0;
    bytes[48] = 1;
    assert_eq!(Metadata::decode(&bytes[..n]), Err(Error::Invalid));
    let mut untouched = [0xa5; 1024];
    assert_eq!(Metadata { directory: true,
                          ..value }.encode(&mut untouched),
               Err(Error::Invalid));
    assert_eq!(untouched, [0xa5; 1024]);
}
#[test]
fn mount_writability_and_unknown_capacity_are_explicit_for_every_filesystem()
{
    let root = Metadata { id: 2,
                          parent: 0,
                          directory: true,
                          data_length: 0,
                          resource_length: 0,
                          ..metadata() };
    let mut bytes = [0; 1024];
    for filesystem in [*b"HFSB", *b"HOST", *b"SIT!"]
    {
        for capabilities in [fs::READ, fs::READ | fs::WRITE]
        {
            let value = Mount { capabilities,
                                filesystem,
                                filesystem_version: 1,
                                capacity: None,
                                root };
            let n = value.encode(&mut bytes).unwrap();
            assert_eq!(Mount::decode(&bytes[..n]), Ok(value));
            assert_eq!(Mount::decode(&bytes[..n]).unwrap().capacity, None);
        }
    }
    let value = Mount { capabilities: fs::READ,
                        filesystem: *b"HOST",
                        filesystem_version: 1,
                        capacity: Some((100, 101)),
                        root };
    let mut untouched = [0xa5; 1024];
    assert_eq!(value.encode(&mut untouched), Err(Error::Invalid));
    assert_eq!(untouched, [0xa5; 1024]);
    let valid = Mount { capacity: Some((100, 42)),
                        ..value };
    let n = valid.encode(&mut bytes).unwrap();
    assert_eq!(Mount::decode(&bytes[..n]), Ok(valid));
    bytes[16] = 0;
    assert_eq!(Mount::decode(&bytes[..n]), Err(Error::Invalid));
}

#[test]
fn filesystem_projection_keeps_length_delimited_rust_text_and_rejects_self_parent_cycles()
{
    // The filesystem name contract excludes NUL/slash/colon, not every Unicode
    // control character. Comments are Rust strings, not legacy C strings.
    let value = Metadata { name: "Line\nBreak",
                           comment: "before\0after",
                           ..metadata() };
    let mut bytes = [0; 1024];
    let count = value.encode(&mut bytes).unwrap();
    assert_eq!(Metadata::decode(&bytes[..count]), Ok(value));
    for invalid in [Metadata { parent: value.id,
                               ..value },
                    Metadata { revision: 0,
                               ..value },
                    Metadata { child_count: 1,
                               ..value }]
    {
        let mut untouched = [0xa5; 1024];
        assert_eq!(invalid.encode(&mut untouched), Err(Error::Invalid));
        assert_eq!(untouched, [0xa5; 1024]);
    }
}
