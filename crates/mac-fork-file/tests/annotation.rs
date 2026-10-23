use mac_filesystem_interface::MetadataPatch;
use mac_fork_file::{Error,
                    annotation::{Annotation, BYTES, key}};
use sha2::{Digest, Sha256};
fn annotation<'a>() -> Annotation<'a>
{
    Annotation { volume:     [1; 16],
                 object:     [2; 32],
                 catalog_id: 42,
                 revision:   1,
                 patch:      MetadataPatch { comment: Some("café 🦀"),
                                             ..Default::default() } }
}
fn checksum(bytes: &mut [u8; BYTES])
{
    let hash = Sha256::digest(&bytes[..480]);
    bytes[480..].copy_from_slice(&hash);
}
#[test]
fn fixed_record_preserves_only_explicit_metadata_and_validates_every_byte()
{
    let expected = annotation();
    let mut bytes = [0; BYTES];
    expected.encode(&mut bytes).unwrap();
    assert_eq!(Annotation::decode(&bytes), Ok(expected));
    for at in 0..BYTES
    {
        let mut bad = bytes;
        bad[at] ^= 1;
        assert!(Annotation::decode(&bad).is_err(), "changed byte {at}");
    }
    for at in [12, 70, 80, 470]
    {
        let mut bad = bytes;
        bad[at] ^= 1;
        checksum(&mut bad);
        assert_eq!(Annotation::decode(&bad), Err(Error::Corrupt));
    }
    let mut stale = bytes;
    stale[8] = 2;
    checksum(&mut stale);
    assert_eq!(Annotation::decode(&stale), Err(Error::Version));
    let mut bad = bytes;
    bad[68..70].copy_from_slice(&u16::MAX.to_le_bytes());
    checksum(&mut bad);
    assert_eq!(Annotation::decode(&bad), Err(Error::Corrupt));
}
#[test]
fn source_incarnation_and_catalog_identity_are_required_and_invalid_output_is_unchanged()
{
    let mut bytes = [0xa5; BYTES];
    for bad in [Annotation { object: [0; 32],
                             ..annotation() },
                Annotation { volume: [0; 16],
                             ..annotation() },
                Annotation { catalog_id: 1,
                             ..annotation() },
                Annotation { catalog_id: 0x8000_0000,
                             ..annotation() },
                Annotation { revision: 0,
                             ..annotation() }]
    {
        assert_eq!(bad.encode(&mut bytes), Err(Error::Invalid));
        assert_eq!(bytes, [0xa5; BYTES]);
    }
    let mut object = [0; 32];
    object[0] = 0xf1;
    object[31] = 0xa9;
    let name = key(&object);
    assert_eq!(&name[..2], b"f1");
    assert_eq!(&name[62..], b"a9");
    assert!(name.iter().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
}

#[test]
fn identity_only_binding_does_not_advance_metadata_revision_or_change_dates()
{
    let record = Annotation { revision: 0,
                              patch: MetadataPatch::default(),
                              ..annotation() };
    let mut bytes = [0; BYTES];
    record.encode(&mut bytes).unwrap();
    let decoded = Annotation::decode(&bytes).unwrap();
    let mut metadata = mac_fork_file::Metadata::new(mac_fork_file::Kind::File,
                                                    16,
                                                    record.volume,
                                                    "Unchanged",
                                                    42).unwrap();
    let revision = metadata.revision;
    decoded.apply(&mut metadata).unwrap();
    assert_eq!(metadata.catalog_id, record.catalog_id);
    assert_eq!(metadata.revision, revision);
    assert_eq!(metadata.created, 42);
    assert_eq!(metadata.modified, 42);
    assert_eq!(metadata.name, "Unchanged");
    assert_eq!(Annotation { patch: MetadataPatch { comment: Some("changed"),
                                                   ..Default::default() },
                            ..record }.validate(),
               Err(Error::Invalid));
}
