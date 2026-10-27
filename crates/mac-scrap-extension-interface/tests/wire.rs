use mac_scrap_extension_interface::{Content, MAX_REPLY, METADATA_BYTES, Metadata, Reply};
#[test]
fn metadata_preserves_empty_text_separately_from_absence_and_rejects_malformed_records()
{
    let text = Metadata { revision: u64::MAX,
                          content:  Content::Text,
                          bytes:    0 };
    let empty = Metadata { revision: u64::MAX,
                           content:  Content::Empty,
                           bytes:    0 };
    assert_eq!(Metadata::decode(&text.encode()), Some(text));
    assert_eq!(Metadata::decode(&empty.encode()), Some(empty));
    assert_ne!(text.encode(), empty.encode());
    for length in 0..METADATA_BYTES
    {
        assert!(Metadata::decode(&text.encode()[..length]).is_none());
    }
    let mut invalid = text.encode();
    invalid[..8].fill(0);
    assert!(Metadata::decode(&invalid).is_none());
    invalid = text.encode();
    invalid[8..12].copy_from_slice(&3u32.to_le_bytes());
    assert!(Metadata::decode(&invalid).is_none());
    for content in [Content::Empty, Content::Unsupported]
    {
        assert!(Metadata::decode(&Metadata { revision: 1,
                                             content,
                                             bytes: 1 }.encode()).is_none());
    }
}
#[test]
fn terminal_replies_keep_utf8_causes_and_refuse_lengths_pending_status_and_missing_causes()
{
    let cause = "Scrap snapshot belongs to another session; retry with the original pin Ω";
    let mut failure = (-4i32).to_le_bytes().to_vec();
    failure.extend_from_slice(&(cause.len() as u32).to_le_bytes());
    failure.extend_from_slice(cause.as_bytes());
    assert_eq!(Reply::decode(&failure),
               Some(Reply::Failure { status: -4,
                                     cause }));
    for length in 0..failure.len()
    {
        assert!(Reply::decode(&failure[..length]).is_none());
    }
    let mut invalid = failure.clone();
    invalid[0..4].copy_from_slice(&1i32.to_le_bytes());
    assert!(Reply::decode(&invalid).is_none());
    invalid = failure.clone();
    invalid[8] = 0xff;
    assert!(Reply::decode(&invalid).is_none());
    assert!(Reply::decode(&[0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]).is_none());
    let mut oversized = vec![0; MAX_REPLY + 1];
    oversized[4..8].copy_from_slice(&((MAX_REPLY + 1 - 8) as u32).to_le_bytes());
    assert!(Reply::decode(&oversized).is_none());
    assert_eq!(Reply::decode(&[0; 8]), Some(Reply::Success(&[])));
}
