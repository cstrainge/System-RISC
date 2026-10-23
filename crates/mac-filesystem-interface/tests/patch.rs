use mac_filesystem_interface::{Error, FileProperties, MAX_PATCH_BYTES, MetadataPatch, PATCH_BYTES};
#[test]
fn selective_changes_preserve_unrelated_raw_metadata_and_compose()
{
    let mut raw = FileProperties::fresh("Native.txt", 3_875_171_696_123_456);
    raw.attributes = 0xa0;
    raw.finder_info[..8].copy_from_slice(b"TEXTTEST");
    raw.finder_info[8] = 0xa0;
    let mut icon = MetadataPatch::default();
    icon.finder_mask[10..14].fill(0xff);
    icon.finder_info[10..14].copy_from_slice(&[1, 2, 3, 4]);
    let comment = MetadataPatch { comment: Some("café 🦀"),
                                  attributes_mask: 1,
                                  attributes: 1,
                                  ..Default::default() };
    let merged = icon.merged(comment).unwrap();
    let actual = merged.apply(raw).unwrap();
    assert_eq!(icon.apply(raw).unwrap().finder_info[..10], raw.finder_info[..10]);
    assert_eq!(actual.finder_info[..10], raw.finder_info[..10]);
    assert_eq!(&actual.finder_info[10..14], &[1, 2, 3, 4]);
    assert_eq!(actual.created, raw.created);
    assert_eq!(actual.modified, raw.modified);
    assert_eq!(actual.attributes, 0xa1);
    assert_eq!(actual.comment, "café 🦀");
    let clear = MetadataPatch { comment: Some(""),
                                attributes_mask: 1,
                                backup: Some(None),
                                ..Default::default() };
    let result = merged.merged(clear).unwrap().apply(raw).unwrap();
    assert_eq!(result.comment, "");
    assert_eq!(result.attributes, 0xa0);
    assert_eq!(result.finder_info, actual.finder_info);
    assert_eq!(result.backup, None);
}
#[test]
fn exact_roundtrip_checks_all_lengths_presence_flags_utf8_and_reserved_bytes()
{
    let patch = MetadataPatch { comment: Some("café 🦀"),
                                created: Some(u64::MAX),
                                modified: Some(0),
                                backup: Some(Some(0)),
                                attributes_mask: 1,
                                attributes: 1,
                                ..Default::default() };
    let mut bytes = [0xa5; MAX_PATCH_BYTES];
    let length = patch.encode(&mut bytes).unwrap();
    let wire = &bytes[..length];
    assert_eq!(MetadataPatch::decode(wire), Ok(patch));
    assert!(bytes[length..].iter().all(|b| *b == 0xa5));
    for end in 0..length
    {
        assert!(MetadataPatch::decode(&wire[..end]).is_err(), "{end}");
    }
    assert_eq!(MetadataPatch::decode(&bytes[..length + 1]), Err(Error::Length));
    for (at, value, error) in [(0, 2, Error::Version),
                               (2, 0, Error::Invalid),
                               (4, 0xff, Error::Invalid),
                               (14, 1, Error::Invalid),
                               (10, 2, Error::Invalid),
                               (72, 1, Error::Invalid),
                               (PATCH_BYTES, 0xff, Error::Utf8)]
    {
        let mut bad = wire.to_vec();
        bad[at] = value;
        assert_eq!(MetadataPatch::decode(&bad), Err(error), "byte {at}");
    }
    let mut short = [0xa5; PATCH_BYTES];
    assert_eq!(patch.encode(&mut short), Err(Error::Length));
    assert_eq!(short, [0xa5; PATCH_BYTES]);
}
#[test]
fn absent_empty_clear_and_zero_dates_have_distinct_canonical_representations()
{
    let mut encodings = Vec::new();
    for patch in [MetadataPatch::default(),
                  MetadataPatch { comment: Some(""),
                                  ..Default::default() },
                  MetadataPatch { backup: Some(None),
                                  ..Default::default() },
                  MetadataPatch { backup: Some(Some(0)),
                                  ..Default::default() },
                  MetadataPatch { modified: Some(0),
                                  ..Default::default() }]
    {
        let mut bytes = [0; PATCH_BYTES];
        patch.encode(&mut bytes).unwrap();
        assert_eq!(MetadataPatch::decode(&bytes), Ok(patch));
        assert!(!encodings.contains(&bytes));
        encodings.push(bytes);
    }
    let bad = MetadataPatch { attributes: 1,
                              ..Default::default() };
    assert_eq!(bad.apply(FileProperties::fresh("raw", 0)), Err(Error::Invalid));
    assert_eq!(MetadataPatch::default().merged(bad), Err(Error::Invalid));
    let mut bytes = [0xa5; MAX_PATCH_BYTES];
    assert_eq!(bad.encode(&mut bytes), Err(Error::Invalid));
    assert_eq!(bytes, [0xa5; MAX_PATCH_BYTES]);
    let long = "x".repeat(256);
    assert_eq!(MetadataPatch { comment: Some(&long),
                               ..Default::default() }.encode(&mut bytes),
               Err(Error::Invalid));
}
