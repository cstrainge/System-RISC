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
    // The class excludes NUL but leaves separators and normalization policy to
    // providers (including classic Mac names addressed by ID). Comments are
    // Rust strings, not legacy C strings.
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

#[test]
fn creation_properties_are_lossless_bounded_and_separate_from_persistent_ids()
{
    let mut props = fs::FileProperties::fresh("Cafe\u{301}", 3_875_171_696_123_456);
    props.modified += 42;
    props.backup = Some(0);
    props.finder_info = [0x53; 32];
    props.attributes = 1;
    props.comment = "before\0after\r🦀";
    let mut bytes = [0; 2048];
    let count = props.encode(&mut bytes).unwrap();
    assert_eq!(fs::FileProperties::decode(&bytes[..count]), Ok(props));
    for len in 0..count
    {
        assert!(fs::FileProperties::decode(&bytes[..len]).is_err());
    }
    assert!(fs::FileProperties::decode(&bytes[..count + 1]).is_err());
    for at in [0, 2, 10, 12]
    {
        let old = bytes[at];
        bytes[at] = 0xff;
        assert!(fs::FileProperties::decode(&bytes[..count]).is_err());
        bytes[at] = old;
    }
    bytes[12] = 0;
    bytes[32] = 1;
    assert_eq!(fs::FileProperties::decode(&bytes[..count]), Err(Error::Invalid));
    let mut untouched = [0xa5; 2048];
    let long = "a".repeat(256);
    assert_eq!(fs::FileProperties { comment: &long,
                                    ..props }.encode(&mut untouched),
               Err(Error::Invalid));
    assert_eq!(untouched, [0xa5; 2048]);
    let decomposed = "e\u{301}".repeat(127) + "x";
    assert!(decomposed.len() > 255);
    let props = fs::FileProperties::fresh(&decomposed, 0);
    let count = props.encode(&mut bytes).unwrap();
    assert_eq!(fs::FileProperties::decode(&bytes[..count]), Ok(props));
    assert_eq!(Arguments { payload: decomposed.as_bytes(),
                           ..Default::default() }.name(),
               Ok(decomposed.as_str()));
    let name = "Quarter/Final";
    let count = Metadata { name,
                           ..metadata() }.encode(&mut bytes)
                                         .unwrap();
    assert_eq!(Metadata::decode(&bytes[..count]).unwrap().name, name);
}

#[test]
fn published_receipts_preserve_actual_file_and_volume_and_reject_malformed_envelopes()
{
    let file = metadata();
    let root = Metadata { id: 2,
                          parent: 0,
                          directory: true,
                          data_length: 0,
                          resource_length: 0,
                          child_count: 42,
                          ..file };
    let value = fs::WriteReceipt { file,
                                   volume: Mount { capabilities: fs::READ | fs::WRITE,
                                                   filesystem: *b"HOST",
                                                   filesystem_version: 1,
                                                   capacity: None,
                                                   root } };
    let mut bytes = [0; fs::MAX_WRITE_RECEIPT_BYTES];
    let n = value.encode(&mut bytes).unwrap();
    assert_eq!(fs::WriteReceipt::decode(&bytes[..n]), Ok(value));
    let folder = fs::WriteReceipt { file: Metadata { directory: true,
                                                     data_length: 0,
                                                     resource_length: 0,
                                                     child_count: 0,
                                                     ..file },
                                    ..value };
    let mut folder_bytes = [0; fs::MAX_WRITE_RECEIPT_BYTES];
    let folder_length = folder.encode(&mut folder_bytes).unwrap();
    assert_eq!(fs::WriteReceipt::decode(&folder_bytes[..folder_length]), Ok(folder));
    let abort = fs::AbortReceipt(Some(value));
    let mut receipt_bytes = [0; fs::MAX_ABORT_RECEIPT_BYTES];
    let m = abort.encode(&mut receipt_bytes).unwrap();
    assert_eq!(m, n + 4);
    assert_eq!(fs::AbortReceipt::decode(&receipt_bytes[..m]), Ok(abort));
    assert!(fs::AbortReceipt::decode(&receipt_bytes[..m - 1]).is_err());
    assert!(fs::AbortReceipt::decode(&receipt_bytes[..m + 1]).is_err());
    receipt_bytes[0] = 2;
    assert!(fs::AbortReceipt::decode(&receipt_bytes[..m]).is_err());
    assert_eq!(fs::AbortReceipt::decode(&[0; 4]), Ok(fs::AbortReceipt(None)));
    assert!(fs::AbortReceipt::decode(&[0; 5]).is_err());
    assert!(fs::AbortReceipt::decode(&[1, 0, 0, 0]).is_err());
    assert_eq!(fs::WriteReceipt::decode(&bytes[..n]).unwrap().file.backup, Some(0));
    for len in 0..n
    {
        assert!(fs::WriteReceipt::decode(&bytes[..len]).is_err());
    }
    assert_eq!(fs::WriteReceipt::decode(&bytes[..n + 1]), Err(Error::Length));
    for at in [0, 2, 4, 8, 12]
    {
        let old = bytes[at];
        bytes[at] = 0xff;
        assert!(fs::WriteReceipt::decode(&bytes[..n]).is_err());
        bytes[at] = old;
    }
    let mut untouched = [0xa5; fs::MAX_WRITE_RECEIPT_BYTES];
    let invalid = fs::WriteReceipt { file: root,
                                     ..value };
    assert_eq!(invalid.encode(&mut untouched), Err(Error::Invalid));
    assert_eq!(untouched, [0xa5; fs::MAX_WRITE_RECEIPT_BYTES]);
    assert_eq!(value.encode(&mut untouched[..n - 1]), Err(Error::Length));
    assert_eq!(untouched, [0xa5; fs::MAX_WRITE_RECEIPT_BYTES]);
    assert_eq!(fs::WriteReceipt { volume: Mount { capacity: Some((1, 2)),
                                                  ..value.volume },
                                  ..value }.encode(&mut untouched),
               Err(Error::Invalid));
    assert_eq!(untouched, [0xa5; fs::MAX_WRITE_RECEIPT_BYTES]);
}
