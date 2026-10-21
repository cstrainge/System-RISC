use mac_host_protocol::*;
#[test]
fn fixed_little_endian_layout_and_every_truncation_are_checked()
{
    let mut header = Header::request(Operation::Read, 0x10001, 0x20002, 0x30003);
    header.offset = 0x1020_3040_5060_7080;
    header.value = 4096;
    let bytes = header.encode().unwrap();
    assert_eq!(&bytes[..12], b"RSHOST01\x01\x00\x30\x00");
    assert_eq!(&bytes[16..24], b"\x01\x00\x01\x00\x07\x00\x00\x00");
    assert_eq!(&bytes[32..40], b"\x80\x70\x60\x50\x40\x30\x20\x10");
    assert_eq!(Header::decode(&bytes), Ok(header));
    for len in 0..HEADER_BYTES
    {
        assert!(Header::decode(&bytes[..len]).is_err());
    }
    for (offset, value) in [(0, b'x'), (8, 2), (10, 47), (21, 1), (22, 2), (23, 1)]
    {
        let mut bad = bytes;
        bad[offset] = value;
        assert!(Header::decode(&bad).is_err());
    }
    for payload in [MAX_PAYLOAD as u32 + 1, u32::MAX]
    {
        let mut bad = bytes;
        bad[12..16].copy_from_slice(&payload.to_le_bytes());
        assert_eq!(Header::decode(&bad), Err(Error::Length));
    }
    assert!(Header::request(Operation::Read, 1, 0, 1).encode().is_err());
    assert!(Header::request(Operation::Hello, 0, 0, 0).encode().is_err());
}
#[test]
fn only_bounded_portable_components_can_name_host_children()
{
    for name in ["", ".", "..", "/etc", "a/b", "a\\b", "C:drive", "a\0b", "a\nb"]
    {
        assert!(component(name.as_bytes()).is_err(), "{name:?}");
    }
    assert_eq!(component("café".as_bytes()), Ok("café"));
    assert!(component(&[b'a'; 256]).is_err());
    assert!(component(&[255]).is_err());
    assert_eq!(rename_names(b"\x03\x00oldnew"), Ok(("old", "new")));
    assert!(rename_names(b"\xff\xffold").is_err());
}

#[test]
fn receiver_assembles_arbitrary_fragments_without_consuming_the_next_frame()
{
    let mut reply = Header::request(Operation::Read, 0x10001, 0x20002, 0x30003).response();
    reply.payload = 6000;
    let mut bytes = reply.encode().unwrap().to_vec();
    bytes.extend((0..6000).map(|i| (i * 17) as u8));
    let expected = bytes[HEADER_BYTES..].to_vec();
    for chunk in [1, 7, 47, 48, 49, 512, 4096, 7000]
    {
        let mut receiver = Receiver::new(reply.request, reply.operation);
        let mut body = [0; MAX_PAYLOAD];
        for fragment in bytes.chunks(chunk)
        {
            assert_eq!(receiver.feed(fragment, &mut body), Ok(fragment.len()));
        }
        assert_eq!(receiver.completed(), Some(reply));
        assert_eq!(&body[..6000], expected);
        assert_eq!(receiver.feed(b"next packet", &mut body), Ok(0));
    }
    let mut receiver = Receiver::new(reply.request, reply.operation);
    let mut body = [0; MAX_PAYLOAD];
    bytes.extend_from_slice(b"next packet");
    assert_eq!(receiver.feed(&bytes, &mut body), Ok(HEADER_BYTES + 6000));
}
#[test]
fn receiver_rejects_wrong_tickets_directions_operations_and_small_storage()
{
    let mut reply = Header::request(Operation::Read, 7, 8, 9).response();
    reply.payload = 10;
    for (request, operation) in [(6, Operation::Read), (7, Operation::Write)]
    {
        let mut receiver = Receiver::new(request, operation);
        assert!(receiver.feed(&reply.encode().unwrap(), &mut [0; 10]).is_err());
        assert_eq!(receiver.completed(), None);
        assert!(receiver.feed(&[], &mut [0; 10]).is_err());
    }
    let mut receiver = Receiver::new(7, Operation::Read);
    assert_eq!(receiver.feed(&reply.encode().unwrap(), &mut [0; 9]), Err(Error::Length));
    reply.reply = false;
    let mut receiver = Receiver::new(7, Operation::Read);
    assert_eq!(receiver.feed(&reply.encode().unwrap(), &mut [0; 10]), Err(Error::Header));
}
