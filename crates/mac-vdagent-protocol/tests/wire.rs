use mac_vdagent_protocol::*;

fn caps() -> Capabilities
{
    Capabilities::empty().with(Capability::ClipboardByDemand)
                         .with(Capability::ClipboardSelection)
                         .with(Capability::ClipboardGrabSerial)
}
fn encoded(message: &OwnedMessage, chunk_bytes: usize) -> Vec<u8>
{
    let mut result = Vec::new();
    for chunk in message.chunks(chunk_bytes).unwrap()
    {
        let parts = chunk.segments();
        let header = ChunkHeader::decode(parts[0]).unwrap();
        assert_eq!(header.port(), Port::Client);
        assert!(header.size() <= chunk_bytes);
        assert_eq!(chunk.encoded_bytes(), CHUNK_HEADER_BYTES + header.size());
        for part in parts
        {
            result.extend_from_slice(part);
        }
    }
    result
}
fn raw_stream(kind: MessageType, payload: &[u8], chunk_bytes: usize) -> Vec<u8>
{
    let mut message = Header::new(kind, payload.len(), Limits::qemu()).unwrap().encode().to_vec();
    message.extend_from_slice(payload);
    let mut stream = Vec::new();
    for part in message.chunks(chunk_bytes)
    {
        stream.extend_from_slice(&ChunkHeader::new(Port::Client, part.len()).unwrap().encode());
        stream.extend_from_slice(part);
    }
    stream
}
fn drive(mut input: &[u8], fragment: usize, decoder: &mut Decoder, capabilities: Capabilities,
         mut consume: impl FnMut(Message<'_>))
{
    while !input.is_empty()
    {
        let boundary = input.len().min(fragment);
        let mut part = &input[..boundary];
        while !part.is_empty()
        {
            let feed = decoder.feed(part).unwrap();
            assert!(feed.consumed > 0 || feed.frame.is_some() || feed.ignored.is_some(),
                    "decoder made no progress");
            part = &part[feed.consumed..];
            if let Some(frame) = feed.frame
            {
                consume(frame.message(capabilities).unwrap());
            }
        }
        input = &input[boundary..];
    }
}

#[test]
fn external_header_layout_is_exact_little_endian_and_length_checked()
{
    let header = Header::new(MessageType::CLIPBOARD_REQUEST, 8, Limits::qemu()).unwrap();
    let bytes = header.encode();
    assert_eq!(&bytes[..8], &[1, 0, 0, 0, 8, 0, 0, 0]);
    assert_eq!(&bytes[8..16], &[0; 8]);
    assert_eq!(&bytes[16..], &[8, 0, 0, 0]);
    assert_eq!(Header::decode(&bytes, Limits::qemu()).unwrap(), header);
    for count in 0..MESSAGE_HEADER_BYTES
    {
        assert_eq!(Header::decode(&bytes[..count], Limits::qemu()), Err(Error::Length));
    }
    let chunk = ChunkHeader::new(Port::Client, 2048).unwrap();
    assert_eq!(chunk.encode(), [1, 0, 0, 0, 0, 8, 0, 0]);
    assert_eq!(ChunkHeader::decode(&chunk.encode()).unwrap(), chunk);
    assert!(ChunkHeader::new(Port::Client, 2049).is_err());
    assert!(ChunkHeader::decode(&[9, 0, 0, 0, 1, 0, 0, 0]).is_err());
    assert!(Limits::new(MESSAGE_HEADER_BYTES - 1).is_err());
    assert!(Limits::new(QEMU_MAX_MESSAGE_BYTES + 1).is_err());
}

#[test]
fn every_header_and_payload_fragment_boundary_preserves_owned_utf8_and_empty_text()
{
    for text in ["", "Ω 日本 🦀\nline two\r\n", "nul\0is not silently truncated"]
    {
        let message =
            OwnedMessage::text(Selection::Clipboard, text, caps(), Limits::qemu()).unwrap();
        for chunk in [1, 2, 7, 19, 20, 21, 1024, 2048]
        {
            let wire = encoded(&message, chunk);
            for fragment in [1, 2, 3, 7, 8, 19, 20, 31, 1024, 2048]
            {
                let mut decoder = Decoder::new(Limits::qemu());
                let mut received = 0;
                drive(&wire, fragment, &mut decoder, caps(), |message| {
                    match message
                    {
                        Message::Data(ClipboardData::Text { selection,
                                                            text: actual }) =>
                        {
                            assert_eq!(selection, Selection::Clipboard);
                            assert_eq!(actual, text);
                            received += 1;
                        }
                        other => panic!("unexpected {other:?}")
                    }
                });
                assert_eq!(received, 1, "chunk {chunk}, input fragment {fragment}");
                assert!(!decoder.is_poisoned());
            }
        }
    }
}

#[test]
fn input_scratch_can_be_changed_between_fragments_without_rebasing_saved_bytes()
{
    let message = OwnedMessage::text(Selection::Clipboard,
                                     "retained across changing scratch 🦀",
                                     caps(),
                                     Limits::qemu()).unwrap();
    let wire = encoded(&message, 1024);
    let mut decoder = Decoder::new(Limits::qemu());
    let split = CHUNK_HEADER_BYTES + MESSAGE_HEADER_BYTES + 9;
    let mut first = wire[..split].to_vec();
    assert!(decoder.feed(&first).unwrap().frame.is_none());
    first.fill(0xff);
    let second = wire[split..].to_vec();
    let feed = decoder.feed(&second).unwrap();
    let frame = feed.frame.unwrap();
    assert!(matches!(frame.message(caps()).unwrap(),
                     Message::Data(ClipboardData::Text { text: "retained across changing \
                                                                scratch 🦀",
                                                         .. })));
}

#[test]
fn multiple_messages_inside_one_chunk_and_multiple_chunks_in_one_feed_keep_boundaries()
{
    let release = OwnedMessage::release(Selection::Clipboard, caps(), Limits::qemu()).unwrap();
    let request = OwnedMessage::request(Selection::Clipboard,
                                        Format::UTF8_TEXT,
                                        caps(),
                                        Limits::qemu()).unwrap();
    let text =
        OwnedMessage::text(Selection::Clipboard, "coalesced", caps(), Limits::qemu()).unwrap();
    let mut combined = Vec::new();
    for message in [&release, &request, &text]
    {
        combined.extend_from_slice(&message.header().encode());
        combined.extend_from_slice(message.payload());
    }
    let mut stream = ChunkHeader::new(Port::Client, combined.len()).unwrap().encode().to_vec();
    stream.extend_from_slice(&combined);
    // Add a separate chunk to the same read, following the combined chunk.
    stream.extend_from_slice(&encoded(&text, 3));
    let mut decoder = Decoder::new(Limits::qemu());
    let mut kinds = Vec::new();
    drive(&stream, stream.len(), &mut decoder, caps(), |message| {
        kinds.push(match message
                   {
                       Message::Release { .. } => 9,
                       Message::Request { .. } => 8,
                       Message::Data(_) => 4,
                       other => panic!("{other:?}")
                   })
    });
    assert_eq!(kinds, [9, 8, 4, 4]);
}

#[test]
fn unknown_message_bodies_and_server_port_chunks_are_explicitly_ignored_without_payload_allocation(
    )
{
    let unknown = raw_stream(MessageType::from_raw(0xf123), &vec![0xff; 65536], MAX_CHUNK_BYTES);
    let mut decoder = Decoder::new(Limits::qemu());
    let mut ignored = 0;
    drive(&unknown, 77, &mut decoder, caps(), |message| {
        assert!(matches!(message,Message::Ignored{kind,bytes:65536} if kind.raw()==0xf123));
        ignored += 1;
    });
    assert_eq!(ignored, 1);
    assert_eq!(decoder.retained_capacity(), 0);
    let message =
        OwnedMessage::text(Selection::Clipboard, "multiplexed", caps(), Limits::qemu()).unwrap();
    let mut plain = message.header().encode().to_vec();
    plain.extend_from_slice(message.payload());
    let mut stream = ChunkHeader::new(Port::Client, 10).unwrap().encode().to_vec();
    stream.extend_from_slice(&plain[..10]);
    stream.extend_from_slice(&ChunkHeader::new(Port::Server, 3).unwrap().encode());
    stream.extend_from_slice(&[0xde, 0xad, 0xff]);
    stream.extend_from_slice(&ChunkHeader::new(Port::Client, plain.len() - 10).unwrap().encode());
    stream.extend_from_slice(&plain[10..]);
    let mut decoder = Decoder::new(Limits::qemu());
    let mut offset = 0;
    let mut seen_server = false;
    let mut seen_text = false;
    while offset < stream.len()
    {
        let feed = decoder.feed(&stream[offset..]).unwrap();
        offset += feed.consumed;
        if let Some(ignored) = feed.ignored
        {
            assert_eq!(ignored,
                       IgnoredChunk { port:  Port::Server,
                                      bytes: 3 });
            seen_server = true;
        }
        if let Some(frame) = feed.frame
        {
            seen_text = matches!(frame.message(caps()).unwrap(),
                                 Message::Data(ClipboardData::Text { text: "multiplexed",
                                                                     .. }));
        }
    }
    assert!(seen_server && seen_text);
}

#[test]
fn declared_oversize_and_bad_framing_poison_before_any_receive_allocation()
{
    for (offset, bytes, expected) in [(0, 2u32.to_le_bytes(), Error::Protocol { observed: 2 }),
                                      (16,
                                       u32::MAX.to_le_bytes(),
                                       Error::MessageSize { bytes:   u64::from(u32::MAX) + 20,
                                                            maximum: QEMU_MAX_MESSAGE_BYTES })]
    {
        let mut message = Header::new(MessageType::CLIPBOARD, 0, Limits::qemu()).unwrap().encode();
        message[offset..offset + 4].copy_from_slice(&bytes);
        let mut stream = ChunkHeader::new(Port::Client, message.len()).unwrap().encode().to_vec();
        stream.extend_from_slice(&message);
        let mut decoder = Decoder::new(Limits::qemu());
        let failure = decoder.feed(&stream).unwrap_err();
        assert_eq!(failure.cause, expected);
        assert_eq!(failure.consumed, CHUNK_HEADER_BYTES + MESSAGE_HEADER_BYTES);
        assert!(decoder.is_poisoned());
        assert_eq!(decoder.retained_capacity(), 0);
        assert_eq!(decoder.feed(&[]).unwrap_err().cause, Error::Poisoned);
        let detail = failure.to_string();
        assert!(detail.contains("Host Clipboard"));
        assert!(detail.contains("reset"));
    }
    let mut decoder = Decoder::new(Limits::qemu());
    let bad_chunk = [1, 0, 0, 0, 1, 8, 0, 0];
    let error = decoder.feed(&bad_chunk).unwrap_err();
    assert_eq!(error.consumed, 8);
    assert_eq!(error.cause, Error::ChunkSize { observed: 2049 });
    assert_eq!(decoder.retained_capacity(), 0);
}

#[test]
fn capabilities_preserve_unknown_words_without_negotiating_unimplemented_rights()
{
    let bits = (1u32 << 5) | (1u32 << 31);
    let mut payload = 1u32.to_le_bytes().to_vec();
    payload.extend_from_slice(&bits.to_le_bytes());
    payload.extend_from_slice(&4u32.to_le_bytes());
    let stream = raw_stream(MessageType::ANNOUNCE_CAPABILITIES, &payload, 1024);
    let mut decoder = Decoder::new(Limits::qemu());
    let frame = decoder.feed(&stream).unwrap().frame.unwrap();
    let Message::Capabilities(peer) = frame.message(Capabilities::empty()).unwrap()
    else
    {
        panic!("not capabilities")
    };
    assert!(peer.requests_reply());
    assert_eq!(peer.word_count(), 2);
    assert!(peer.unknown_bits_present());
    assert!(peer.supports(Capability::ClipboardByDemand));
    let negotiated = peer.negotiate(caps());
    assert!(negotiated.contains(Capability::ClipboardByDemand));
    assert!(!negotiated.contains(Capability::ClipboardSelection));
    assert!(Capabilities::from_known_bits(1 << 31).is_err());
    // A peer may advertise an empty variable bitset, which grants nothing.
    let stream = raw_stream(MessageType::ANNOUNCE_CAPABILITIES, &0u32.to_le_bytes(), 1024);
    let frame = decoder.feed(&stream).unwrap().frame.unwrap();
    let Message::Capabilities(peer) = frame.message(Capabilities::empty()).unwrap()
    else
    {
        panic!("not capabilities")
    };
    assert_eq!(peer.word_count(), 0);
    assert_eq!(peer.negotiate(caps()), Capabilities::empty());
    assert!(!peer.unknown_bits_present());
}

#[test]
fn serials_are_grab_only_and_selection_padding_and_message_shapes_are_checked()
{
    let grab = OwnedMessage::grab(Selection::Clipboard,
                                  Some(17),
                                  &[Format::UTF8_TEXT, Format::from_raw(900)],
                                  caps(),
                                  Limits::qemu()).unwrap();
    let mut decoder = Decoder::new(Limits::qemu());
    let stream = encoded(&grab, 1024);
    let frame = decoder.feed(&stream).unwrap().frame.unwrap();
    let Message::Grab(grab) = frame.message(caps()).unwrap()
    else
    {
        panic!("not grab")
    };
    assert_eq!(grab.serial(), Some(17));
    assert_eq!(grab.formats().iter().map(Format::raw).collect::<Vec<_>>(), [1, 900]);
    let request = OwnedMessage::request(Selection::Clipboard,
                                        Format::UTF8_TEXT,
                                        caps(),
                                        Limits::qemu()).unwrap();
    assert_eq!(request.payload().len(), 8, "serial capability must not add a serial to REQUEST");
    assert!(OwnedMessage::grab(Selection::Clipboard, None, &[], caps(), Limits::qemu()).is_err());
    let unnumbered = Capabilities::empty().with(Capability::ClipboardByDemand);
    assert!(OwnedMessage::grab(Selection::Clipboard,Some(1),&[],unnumbered,Limits::qemu()).is_err());
    assert!(OwnedMessage::request(Selection::Primary,
                                  Format::UTF8_TEXT,
                                  unnumbered,
                                  Limits::qemu()).is_err());
    for (kind, payload) in [(MessageType::CLIPBOARD_REQUEST, vec![0, 1, 0, 0, 1, 0, 0, 0]),
                            (MessageType::CLIPBOARD_REQUEST, vec![3, 0, 0, 0, 1, 0, 0, 0]),
                            (MessageType::CLIPBOARD_REQUEST, vec![0, 0, 0, 0, 1, 0, 0, 0, 0]),
                            (MessageType::CLIPBOARD_RELEASE, vec![0, 0, 0, 0, 0]),
                            (MessageType::ANNOUNCE_CAPABILITIES, vec![2, 0, 0, 0, 0, 0, 0, 0])]
    {
        let stream = raw_stream(kind, &payload, 1024);
        let frame = decoder.feed(&stream).unwrap().frame.unwrap();
        assert!(frame.message(caps()).is_err(), "{kind:?}: {payload:?}");
    }
}

#[test]
fn empty_text_unavailable_unknown_format_and_invalid_unicode_are_distinct()
{
    let mut decoder = Decoder::new(Limits::qemu());
    for (format, bytes) in [(0, &[][..]), (1, &[][..]), (12, &b"\xff"[..])]
    {
        let mut payload = vec![0; 4];
        payload.extend_from_slice(&u32::to_le_bytes(format));
        payload.extend_from_slice(bytes);
        let stream = raw_stream(MessageType::CLIPBOARD, &payload, 1024);
        let frame = decoder.feed(&stream).unwrap().frame.unwrap();
        match (format, frame.message(caps()).unwrap())
        {
            (0, Message::Data(ClipboardData::Unavailable { .. })) => (),
            (1,
             Message::Data(ClipboardData::Text { text: "",
                                                 .. })) => (),
            (12,
             Message::Data(ClipboardData::Unsupported { format,
                                                        bytes,
                                                        .. })) =>
            {
                assert_eq!(format.raw(), 12);
                assert_eq!(bytes, b"\xff");
            }
            other => panic!("unexpected {other:?}")
        }
    }
    let stream = raw_stream(MessageType::CLIPBOARD, &[0, 0, 0, 0, 1, 0, 0, 0, 0xff], 1024);
    assert!(matches!(decoder.feed(&stream).unwrap().frame.unwrap().message(caps()),
                     Err(Error::Utf8)));
    let stream = raw_stream(MessageType::CLIPBOARD, &[0, 0, 0, 0, 0, 0, 0, 0, 99], 1024);
    assert!(decoder.feed(&stream).unwrap().frame.unwrap().message(caps()).is_err());
}

#[test]
fn maximum_bounds_include_all_framing_and_unnegotiated_maximum_is_not_applied()
{
    let limits = Limits::new(100).unwrap();
    assert_eq!(limits.maximum_text_bytes(caps()), 72);
    assert!(OwnedMessage::text(Selection::Clipboard, &"x".repeat(72), caps(), limits).is_ok());
    assert!(OwnedMessage::text(Selection::Clipboard, &"x".repeat(73), caps(), limits).is_err());
    let maximum_caps = caps().with(Capability::MaxClipboard);
    for maximum in
        [MaximumClipboard::Unlimited, MaximumClipboard::Bytes(0), MaximumClipboard::Bytes(900)]
    {
        let message = OwnedMessage::maximum(maximum, maximum_caps, Limits::qemu()).unwrap();
        let stream = encoded(&message, 1024);
        let mut decoder = Decoder::new(Limits::qemu());
        let frame = decoder.feed(&stream).unwrap().frame.unwrap();
        assert!(matches!(frame.message(maximum_caps).unwrap(),Message::MaximumClipboard(value) if value==maximum));
        assert!(matches!(frame.message(caps()),
                         Err(Error::CapabilityRequired(Capability::MaxClipboard))));
    }
    assert!(MaximumClipboard::from_raw(-2).is_err());
    assert!(OwnedMessage::maximum(MaximumClipboard::Bytes(u32::MAX),maximum_caps,Limits::qemu()).is_err());
}

#[test]
fn chunk_iterator_is_fused_and_rejects_invalid_chunk_policy()
{
    let message =
        OwnedMessage::text(Selection::Clipboard, "hello", caps(), Limits::qemu()).unwrap();
    assert!(message.chunks(0).is_err());
    assert!(message.chunks(MAX_CHUNK_BYTES + 1).is_err());
    let mut chunks = message.chunks(7).unwrap();
    let expected = message.message_bytes().div_ceil(7);
    assert_eq!(chunks.len(), expected);
    for remaining in (0..expected).rev()
    {
        assert!(chunks.next().is_some());
        assert_eq!(chunks.len(), remaining);
    }
    assert!(chunks.next().is_none());
    assert!(chunks.next().is_none());
}

#[test]
fn disconnect_at_any_partial_header_or_body_is_reported_as_truncation()
{
    let message = OwnedMessage::text(Selection::Clipboard,
                                     "unfinished text 🦀",
                                     caps(),
                                     Limits::qemu()).unwrap();
    let stream = encoded(&message, 7);
    for cut in 1..stream.len()
    {
        let mut decoder = Decoder::new(Limits::qemu());
        let result = decoder.feed(&stream[..cut]).unwrap();
        assert!(result.frame.is_none());
        assert_eq!(decoder.end_of_stream(), Err(Error::Length), "cut {cut}");
    }
    let mut decoder = Decoder::new(Limits::qemu());
    assert!(decoder.end_of_stream().is_ok());
    let result = decoder.feed(&stream).unwrap();
    assert!(result.frame.is_some());
    assert!(decoder.end_of_stream().is_ok());
    // A zero-length external chunk is a no-op, rather than a fabricated frame.
    let zero = ChunkHeader::new(Port::Client, 0).unwrap().encode();
    assert!(decoder.feed(&zero).unwrap().frame.is_none());
    assert!(decoder.end_of_stream().is_ok());
}
