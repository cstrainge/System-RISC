use std::sync::Arc;

use mac_host_clipboard::{CompletionError, Config, Content, Effect, Engine, Epoch, Error, Phase,
                         Sent, Snapshot};
use mac_vdagent_protocol::{Capabilities, Capability, Decoder, Format, Frame, Header, Limits,
                           Message, MessageType, OwnedMessage, Selection};

#[derive(Clone)]
struct Pin
{
    revision:    u64,
    text:        Option<Arc<String>>,
    unsupported: bool
}
impl Pin
{
    fn text(revision: u64, text: &str) -> Self
    {
        Self { revision,
               text: Some(Arc::new(text.into())),
               unsupported: false }
    }

    fn empty(revision: u64) -> Self
    {
        Self { revision,
               text: None,
               unsupported: false }
    }
}
impl Snapshot for Pin
{
    fn revision(&self) -> u64
    {
        self.revision
    }

    fn content(&self) -> Content<'_>
    {
        match &self.text
        {
            Some(text) => Content::Text(text),
            None if self.unsupported => Content::Unsupported,
            None => Content::Empty
        }
    }
}
fn caps() -> Capabilities
{
    Capabilities::empty().with(Capability::ClipboardByDemand)
                         .with(Capability::ClipboardSelection)
                         .with(Capability::ClipboardGrabSerial)
}
fn limits() -> Limits
{
    Limits::new(4096).unwrap()
}
fn config() -> Config
{
    Config::new(limits(), 1024).unwrap()
}
fn encode(message: OwnedMessage) -> Vec<u8>
{
    message.chunks(1024)
           .unwrap()
           .flat_map(|chunk| {
               chunk.segments().into_iter().flat_map(|bytes| bytes.to_vec()).collect::<Vec<_>>()
           })
           .collect()
}
fn frame<R>(message: OwnedMessage, run: impl FnOnce(&Frame<'_>) -> R) -> R
{
    let wire = encode(message);
    let mut decoder = Decoder::new(limits());
    // Exercise arbitrary transport fragments with the production decoder.
    let mut offset = 0;
    while offset < wire.len()
    {
        let end = (offset + 7).min(wire.len());
        let feed = decoder.feed(&wire[offset..end]).unwrap();
        offset += feed.consumed;
        if let Some(value) = feed.frame
        {
            assert_eq!(offset, wire.len());
            return run(&value);
        }
        assert_eq!(offset, end);
    }
    panic!("complete encoded message did not yield a frame")
}
fn deliver(engine: &mut Engine<Pin>, epoch: Epoch, message: OwnedMessage) -> Result<(), Error>
{
    frame(message, |frame| {
        assert!(!matches!(engine.receive(epoch, frame)?, Effect::Import(_)),
                "test must explicitly commit imports");
        Ok(())
    })
}
fn drain(engine: &mut Engine<Pin>) -> (Vec<u8>, Vec<Sent>)
{
    let mut bytes = Vec::new();
    let mut sent = Vec::new();
    while let Some(output) = engine.output()
    {
        let token = output.token;
        let count = output.remaining_bytes();
        for segment in output.segments
        {
            bytes.extend_from_slice(segment);
        }
        sent.push(engine.accepted(token, count).unwrap());
    }
    (bytes, sent)
}
fn ready() -> (Engine<Pin>, Epoch)
{
    let mut engine = Engine::new(config());
    let epoch = engine.open_after_transport_reset(Pin::text(1, "local")).unwrap();
    drain(&mut engine);
    deliver(&mut engine, epoch, OwnedMessage::announce(caps(), false, limits()).unwrap()).unwrap();
    assert_eq!(engine.phase(), Phase::Ready);
    (engine, epoch)
}
fn grab(serial: u32, formats: &[Format]) -> OwnedMessage
{
    OwnedMessage::grab(Selection::Clipboard, Some(serial), formats, caps(), limits()).unwrap()
}
fn offer(engine: &mut Engine<Pin>, epoch: Epoch, serial: u32) -> mac_host_clipboard::FetchToken
{
    deliver(engine, epoch, grab(serial, &[Format::UTF8_TEXT])).unwrap();
    let (_, sent) = drain(engine);
    sent.into_iter()
        .find_map(|sent| if let Sent::Fetch(token) = sent { Some(token) } else { None })
        .unwrap()
}
fn data(text: &str) -> OwnedMessage
{
    OwnedMessage::text(Selection::Clipboard, text, caps(), limits()).unwrap()
}

#[test]
fn capability_handshake_is_explicit_and_connection_labels_never_accept_old_work()
{
    let mut engine = Engine::new(config());
    let epoch = engine.open_after_transport_reset(Pin::empty(1)).unwrap();
    let (wire, _) = drain(&mut engine);
    let mut decoder = Decoder::new(limits());
    let feed = decoder.feed(&wire).unwrap();
    let Message::Capabilities(peer) = feed.frame.unwrap().message(Capabilities::empty()).unwrap()
    else
    {
        panic!()
    };
    assert!(peer.requests_reply());
    assert!(peer.supports(Capability::ClipboardByDemand));
    assert!(!peer.supports(Capability::GuestLineEndCrLf));
    assert!(!peer.supports(Capability::MaxClipboard));
    assert_eq!(deliver(&mut engine, epoch, grab(0, &[Format::UTF8_TEXT])),
               Err(Error::HandshakeRequired));
    let unsupported = OwnedMessage::announce(Capabilities::empty(), false, limits()).unwrap();
    assert_eq!(deliver(&mut engine, epoch, unsupported), Err(Error::UnsupportedPeer));
    assert_eq!(engine.phase(), Phase::ResetRequired);
    let next = engine.open_after_transport_reset(Pin::empty(1)).unwrap();
    assert!(next.get() > epoch.get());
    assert_eq!(deliver(&mut engine, epoch, data("old")), Err(Error::StaleEpoch));
    assert_eq!(engine.local_revision(), Some(1));
    engine.close(next).unwrap();
    assert!(engine.output().is_none());
    assert_eq!(deliver(&mut engine, next, data("late")), Err(Error::Closed));
}

#[test]
fn qemu_crossed_initial_request_is_answered_by_the_single_queued_guest_advertisement()
{
    let mut engine = Engine::new(config());
    let epoch = engine.open_after_transport_reset(Pin::text(1, "local")).unwrap();
    // QEMU emits this request as soon as the frontend opens, independently of
    // the guest's request. It can arrive before any guest bytes were written.
    frame(OwnedMessage::announce(caps(), true, limits()).unwrap(), |frame| {
        assert!(matches!(engine.receive(epoch, frame), Ok(Effect::Idle)));
    });
    assert_eq!(engine.phase(), Phase::Negotiating);
    assert_eq!(engine.queued_messages(), 1);
    let (guest_wire, _) = drain(&mut engine);
    assert_eq!(guest_wire,
               encode(OwnedMessage::announce(caps(), true, limits()).unwrap()),
               "a second guest announcement resets QEMU's registered clipboard peer");
    assert_eq!(engine.phase(), Phase::Negotiating, "crossed host request is not the reply to ours");
    frame(OwnedMessage::announce(caps(), false, limits()).unwrap(), |frame| {
        assert!(matches!(engine.receive(epoch, frame), Ok(Effect::Negotiated)));
    });
    assert_eq!(engine.phase(), Phase::Ready);
    assert_eq!(engine.queued_messages(), 0);
}

#[test]
fn coalesced_qemu_initial_request_and_reply_finish_once_without_a_second_guest_caps_frame()
{
    let mut engine = Engine::new(config());
    let epoch = engine.open_after_transport_reset(Pin::empty(1)).unwrap();
    let (guest_wire, _) = drain(&mut engine);
    let mut host_wire = encode(OwnedMessage::announce(caps(), true, limits()).unwrap());
    host_wire.extend_from_slice(&encode(OwnedMessage::announce(caps(), false, limits()).unwrap()));
    let mut decoder = Decoder::new(limits());
    let mut offset = 0;
    let mut transitions = 0;
    while offset < host_wire.len()
    {
        let feed = decoder.feed(&host_wire[offset..]).unwrap();
        offset += feed.consumed;
        let frame = feed.frame.unwrap();
        if matches!(engine.receive(epoch, &frame).unwrap(), Effect::Negotiated)
        {
            transitions += 1;
        }
    }
    assert_eq!(transitions, 1);
    assert_eq!(engine.phase(), Phase::Ready);
    assert_eq!(guest_wire, encode(OwnedMessage::announce(caps(), true, limits()).unwrap()));
    assert_eq!(engine.queued_messages(), 0);
}

#[test]
fn early_peer_reply_does_not_publish_ready_until_the_owned_advertisement_is_fully_accepted()
{
    let mut engine = Engine::new(config());
    let epoch = engine.open_after_transport_reset(Pin::text(1, "local")).unwrap();
    let token = engine.output().unwrap().token;
    engine.accepted(token, 8).unwrap();
    frame(OwnedMessage::announce(caps(), true, limits()).unwrap(), |frame| {
        assert!(matches!(engine.receive(epoch, frame), Ok(Effect::Idle)));
    });
    frame(OwnedMessage::announce(caps(), false, limits()).unwrap(), |frame| {
        assert!(matches!(engine.receive(epoch, frame), Ok(Effect::Idle)));
    });
    assert_eq!(engine.phase(), Phase::Negotiating);
    assert_eq!(engine.queued_messages(), 1);
    let output = engine.output().unwrap();
    let token = output.token;
    let count = output.remaining_bytes();
    assert_eq!(engine.accepted(token, count).unwrap(), Sent::Negotiated);
    assert_eq!(engine.phase(), Phase::Ready);
    assert_eq!(engine.queued_messages(), 0);
}

#[test]
fn capabilities_cannot_change_between_the_crossed_request_and_its_reply()
{
    let mut engine = Engine::new(config());
    let epoch = engine.open_after_transport_reset(Pin::text(1, "preserved")).unwrap();
    frame(OwnedMessage::announce(caps(), true, limits()).unwrap(), |frame| {
        engine.receive(epoch, frame).unwrap();
    });
    let changed = Capabilities::empty().with(Capability::ClipboardByDemand);
    let error = deliver(&mut engine,
                        epoch,
                        OwnedMessage::announce(changed, false, limits()).unwrap()).unwrap_err();
    assert_eq!(error, Error::CapabilitiesChanged);
    assert!(error.to_string().contains("reset and negotiate"));
    assert_eq!(engine.phase(), Phase::ResetRequired);
    assert!(engine.output().is_none());
    assert_eq!(engine.local_revision(), Some(1));
}

#[test]
fn guest_copies_offer_on_change_and_answer_demand_with_exact_unicode_and_newlines()
{
    let (mut engine, epoch) = ready();
    let text = "café 日本 🦀\r\nLF\nCR\rtail";
    let snapshot = Pin::text(2, text);
    let pin = snapshot.text.as_ref().unwrap().clone();
    engine.observe(snapshot).unwrap();
    assert_eq!(Arc::strong_count(&pin),
               2,
               "engine retains the supplied pin without another text copy");
    let (wire, _) = drain(&mut engine);
    assert_eq!(wire, encode(grab(1, &[Format::UTF8_TEXT])));
    engine.observe(Pin::text(2, text)).unwrap();
    assert_eq!(engine.queued_messages(), 0);
    deliver(&mut engine,epoch,OwnedMessage::request(Selection::Clipboard,Format::UTF8_TEXT,caps(),limits()).unwrap()).unwrap();
    let (wire, _) = drain(&mut engine);
    assert_eq!(wire, encode(data(text)));
}

#[test]
fn stale_data_is_drained_before_the_latest_offer_can_issue_another_fetch()
{
    let (mut engine, epoch) = ready();
    let original = offer(&mut engine, epoch, 4);
    engine.observe(Pin::text(2, "user's newer copy")).unwrap();
    assert!(engine.fetch_is_quarantined());
    deliver(&mut engine, epoch, grab(6, &[Format::UTF8_TEXT])).unwrap();
    assert_eq!(engine.pending_fetch(), Some(original));
    drain(&mut engine);
    frame(data("old host bytes"), |frame| {
        assert!(matches!(engine.receive(epoch, frame), Ok(Effect::Discarded)))
    });
    assert_eq!(engine.local_revision(), Some(2));
    assert!(engine.pending_fetch().is_none());
    engine.poll(epoch).unwrap();
    let (_, sent) = drain(&mut engine);
    assert!(sent.iter().any(|sent| matches!(sent,Sent::Fetch(token) if *token!=original)));
    frame(data("current host bytes"), |frame| {
        let Effect::Import(import) = engine.receive(epoch, frame).unwrap()
        else
        {
            panic!()
        };
        assert_eq!(import.token().expected_revision(), 2);
        assert_eq!(import.text(), "current host bytes");
        engine.complete_import(import, Ok::<_, &str>(Pin::text(3, import.text()))).unwrap();
    });
    assert_eq!(engine.local_revision(), Some(3));
    engine.poll(epoch).unwrap();
    assert_eq!(engine.queued_messages(), 0);
    engine.observe(Pin::text(3, "current host bytes")).unwrap();
    assert_eq!(engine.queued_messages(), 0, "successful import must not echo");
}

#[test]
fn timed_out_tombstones_require_real_channel_reset_before_reuse()
{
    let (mut engine, epoch) = ready();
    let fetch = offer(&mut engine, epoch, 1);
    engine.observe(Pin::text(2, "local wins")).unwrap();
    let error = engine.expire_fetch(fetch).unwrap_err();
    assert_eq!(error, Error::Timeout);
    for word in ["timed out", "unlabelled", "reset", "retain Scrap"]
    {
        assert!(error.to_string().contains(word));
    }
    assert_eq!(engine.phase(), Phase::ResetRequired);
    assert!(engine.output().is_none());
    assert_eq!(deliver(&mut engine, epoch, data("late")), Err(Error::ResetRequired));
    let next = engine.open_after_transport_reset(Pin::text(2, "local wins")).unwrap();
    assert_eq!(engine.expire_fetch(fetch), Err(Error::StaleEpoch));
    assert_ne!(next, epoch);
}

#[test]
fn empty_utf8_is_committed_but_none_and_unsupported_offers_preserve_local_scrap()
{
    let (mut engine, epoch) = ready();
    offer(&mut engine, epoch, 1);
    frame(OwnedMessage::unavailable(Selection::Clipboard, caps(), limits()).unwrap(), |frame| {
        assert!(matches!(engine.receive(epoch, frame), Ok(Effect::Unavailable)));
    });
    assert_eq!(engine.local_revision(), Some(1));
    deliver(&mut engine, epoch, grab(2, &[Format::from_raw(8)])).unwrap();
    assert!(engine.pending_fetch().is_none());
    assert_eq!(engine.local_revision(), Some(1));
    offer(&mut engine, epoch, 3);
    frame(data(""), |frame| {
        let Effect::Import(import) = engine.receive(epoch, frame).unwrap()
        else
        {
            panic!()
        };
        assert_eq!(import.text(), "");
        engine.complete_import(import, Ok::<_, &str>(Pin::text(2, ""))).unwrap();
    });
    engine.observe(Pin::text(3, "")).unwrap();
    drain(&mut engine);
    deliver(&mut engine,epoch,OwnedMessage::request(Selection::Clipboard,Format::UTF8_TEXT,caps(),limits()).unwrap()).unwrap();
    assert_eq!(drain(&mut engine).0, encode(data("")));
    engine.observe(Pin::empty(4)).unwrap();
    assert_eq!(drain(&mut engine).0,
               encode(OwnedMessage::release(Selection::Clipboard, caps(), limits()).unwrap()));
}

#[test]
fn size_nul_and_invalid_unicode_refusals_preserve_local_content_and_explain_recovery()
{
    let (mut engine, epoch) = ready();
    let error = engine.observe(Pin::text(2, "left\0right")).unwrap_err();
    assert_eq!(error, Error::EmbeddedNul { offset: 4 });
    assert!(error.to_string().contains("may truncate"));
    assert_eq!(engine.local_revision(), Some(2));
    assert_eq!(engine.queued_messages(), 0);
    let error = engine.observe(Pin::text(3, &"x".repeat(1025))).unwrap_err();
    assert_eq!(error,
               Error::TextLimit { bytes:   1025,
                                  maximum: 1024 });
    assert!(error.to_string().contains("smaller selection"));
    offer(&mut engine, epoch, 4);
    assert_eq!(deliver(&mut engine, epoch, data("host\0tail")),
               Err(Error::EmbeddedNul { offset: 4 }));
    assert_eq!(engine.local_revision(), Some(3));
    assert!(engine.pending_fetch().is_none());
    offer(&mut engine, epoch, 5);
    let mut invalid = Vec::new();
    invalid.extend_from_slice(&[0; 4]);
    invalid.extend_from_slice(&1u32.to_le_bytes());
    invalid.push(0xff);
    let header = Header::new(MessageType::CLIPBOARD, invalid.len(), limits()).unwrap();
    let mut wire = mac_vdagent_protocol::ChunkHeader::new(mac_vdagent_protocol::Port::Client,
                                                          20 + invalid.len()).unwrap()
                                                                             .encode()
                                                                             .to_vec();
    wire.extend_from_slice(&header.encode());
    wire.extend_from_slice(&invalid);
    let mut decoder = Decoder::new(limits());
    let feed = decoder.feed(&wire).unwrap();
    assert_eq!(engine.receive(epoch, &feed.frame.unwrap()).unwrap_err(),
               Error::Protocol(mac_vdagent_protocol::Error::Utf8));
    assert_eq!(engine.phase(), Phase::ResetRequired);
    assert_eq!(engine.local_revision(), Some(3));
}

#[test]
fn output_owns_retry_bytes_and_validates_each_exact_acceptance_position()
{
    let (mut engine, epoch) = ready();
    let text = "Ω".repeat(500);
    engine.observe(Pin::text(2, &text)).unwrap();
    drain(&mut engine);
    deliver(&mut engine,epoch,OwnedMessage::request(Selection::Clipboard,Format::UTF8_TEXT,caps(),limits()).unwrap()).unwrap();
    let expected = encode(data(&text));
    drop(text);
    let initial = engine.output().unwrap().token;
    assert_eq!(engine.accepted(initial, 0).unwrap(), Sent::Progress);
    assert_eq!(engine.accepted(initial, usize::MAX), Err(Error::InvalidProgress));
    let mut bytes = Vec::new();
    while let Some(output) = engine.output()
    {
        let token = output.token;
        let count = 3.min(output.remaining_bytes());
        bytes.extend(output.segments.into_iter().flatten().take(count).copied());
        engine.accepted(token, count).unwrap();
        if bytes.len() == 3
        {
            assert_eq!(engine.accepted(initial, 1), Err(Error::InvalidProgress));
        }
    }
    assert_eq!(bytes, expected);
}

#[test]
fn partially_sent_text_is_not_reused_after_a_new_local_copy()
{
    let (mut engine, epoch) = ready();
    engine.observe(Pin::text(2, "old")).unwrap();
    drain(&mut engine);
    deliver(&mut engine,epoch,OwnedMessage::request(Selection::Clipboard,Format::UTF8_TEXT,caps(),limits()).unwrap()).unwrap();
    let token = engine.output().unwrap().token;
    engine.accepted(token, 9).unwrap();
    assert_eq!(engine.observe(Pin::text(3, "new")), Err(Error::ResetRequired));
    assert_eq!(engine.local_revision(), Some(3));
    assert!(engine.output().is_none());
    assert_eq!(engine.phase(), Phase::ResetRequired);
}

#[test]
fn unchanged_capability_requests_do_not_emit_another_advertisement_or_reset_live_state()
{
    let (mut engine, epoch) = ready();
    engine.observe(Pin::text(2, "first guest copy")).unwrap();
    drain(&mut engine);
    for _ in 0..16
    {
        frame(OwnedMessage::announce(caps(), true, limits()).unwrap(), |frame| {
            assert!(matches!(engine.receive(epoch, frame), Ok(Effect::Ignored)));
        });
    }
    assert_eq!(engine.queued_messages(), 0);
    engine.observe(Pin::text(3, "second guest copy")).unwrap();
    assert_eq!(drain(&mut engine).0, encode(grab(2, &[Format::UTF8_TEXT])));
    let pending = offer(&mut engine, epoch, 3);
    frame(OwnedMessage::announce(caps(), false, limits()).unwrap(), |frame| {
        assert!(matches!(engine.receive(epoch, frame), Ok(Effect::Ignored)));
    });
    assert_eq!(engine.pending_fetch(), Some(pending));
    assert!(!engine.fetch_is_quarantined());
    assert_eq!(engine.queued_messages(), 0);
}

#[test]
fn atomic_import_causes_are_preserved_and_late_completion_cannot_suppress_a_local_copy()
{
    let (mut engine, epoch) = ready();
    offer(&mut engine, epoch, 1);
    frame(data("host"), |frame| {
        let Effect::Import(import) = engine.receive(epoch, frame).unwrap()
        else
        {
            panic!()
        };
        let error =
            engine.complete_import(import, Err::<Pin, _>("Scrap allocation refused")).unwrap_err();
        assert!(matches!(error, CompletionError::Import("Scrap allocation refused")));
        assert!(error.to_string().contains("Scrap allocation refused"));
        assert!(matches!(engine.complete_import(import, Ok::<_, &str>(Pin::text(2, "host"))),
                         Err(CompletionError::Engine(Error::StaleCompletion))));
    });
    offer(&mut engine, epoch, 2);
    frame(data("pending"), |frame| {
        let Effect::Import(import) = engine.receive(epoch, frame).unwrap()
        else
        {
            panic!()
        };
        assert_eq!(engine.receive(epoch, frame).unwrap_err(), Error::ImportPending);
        engine.observe(Pin::text(2, "new local")).unwrap();
        assert!(matches!(engine.complete_import(import, Err::<Pin, _>("CAS conflict")),
                         Err(CompletionError::Engine(Error::StaleCompletion))));
        assert!(engine.pending_fetch().is_none());
    });
    assert_eq!(engine.local_revision(), Some(2));
    assert!(engine.queued_messages() > 0);
}

#[test]
fn host_serial_ties_win_and_guest_serial_exhaustion_requires_reset()
{
    let (mut engine, epoch) = ready();
    engine.observe(Pin::text(2, "guest")).unwrap();
    drain(&mut engine);
    deliver(&mut engine, epoch, grab(0, &[Format::UTF8_TEXT])).unwrap();
    assert!(engine.pending_fetch().is_none());
    offer(&mut engine, epoch, 1);
    assert!(engine.pending_fetch().is_some(), "equal serial host offer wins");
    frame(data("host"), |frame| {
        let Effect::Import(import) = engine.receive(epoch, frame).unwrap()
        else
        {
            panic!()
        };
        engine.complete_import(import, Ok::<_, &str>(Pin::text(3, "host"))).unwrap();
    });
    offer(&mut engine, epoch, u32::MAX);
    assert_eq!(engine.observe(Pin::text(4, "guest after max")), Err(Error::Exhausted));
    assert_eq!(engine.phase(), Phase::ResetRequired);
    assert_eq!(engine.local_revision(), Some(4));
}

#[test]
fn unsolicited_or_premature_data_cannot_become_an_import()
{
    let (mut engine, epoch) = ready();
    assert_eq!(deliver(&mut engine, epoch, data("unasked")), Err(Error::UnexpectedData));
    let (mut engine, epoch) = ready();
    deliver(&mut engine, epoch, grab(1, &[Format::UTF8_TEXT])).unwrap();
    assert_eq!(deliver(&mut engine, epoch, data("before request was sent")),
               Err(Error::UnexpectedData));
    assert_eq!(engine.phase(), Phase::ResetRequired);
}

#[test]
fn capability_changes_and_transport_framing_failures_revoke_pending_protocol_work()
{
    let (mut engine, epoch) = ready();
    offer(&mut engine, epoch, 1);
    let fewer = Capabilities::empty().with(Capability::ClipboardByDemand);
    assert_eq!(deliver(&mut engine,
                       epoch,
                       OwnedMessage::announce(fewer, false, limits()).unwrap()),
               Err(Error::CapabilitiesChanged));
    assert!(engine.pending_fetch().is_none());
    assert_eq!(engine.local_revision(), Some(1));
    let (mut engine, epoch) = ready();
    let cause = mac_vdagent_protocol::Error::Length;
    assert_eq!(engine.malformed(epoch, cause), Err(Error::Protocol(cause)));
    assert_eq!(engine.phase(), Phase::ResetRequired);
}

#[test]
fn partial_guest_ownership_is_reset_when_the_peer_has_no_grab_serial_support()
{
    let mut engine = Engine::new(config());
    let epoch = engine.open_after_transport_reset(Pin::text(1, "local")).unwrap();
    drain(&mut engine);
    let minimal = Capabilities::empty().with(Capability::ClipboardByDemand);
    deliver(&mut engine, epoch, OwnedMessage::announce(minimal, false, limits()).unwrap()).unwrap();
    engine.observe(Pin::text(2, "guest")).unwrap();
    let token = engine.output().unwrap().token;
    engine.accepted(token, 1).unwrap();
    let incoming = OwnedMessage::grab(Selection::Clipboard,
                                      None,
                                      &[Format::UTF8_TEXT],
                                      minimal,
                                      limits()).unwrap();
    assert_eq!(deliver(&mut engine, epoch, incoming), Err(Error::ResetRequired));
    assert_eq!(engine.phase(), Phase::ResetRequired);
    assert!(engine.output().is_none());
    assert_eq!(engine.local_revision(), Some(2));
}

#[test]
fn close_revokes_borrowed_import_completion_and_primary_selection_does_not_replace_clipboard()
{
    let (mut engine, epoch) = ready();
    let fetch = offer(&mut engine, epoch, 1);
    deliver(&mut engine,
            epoch,
            OwnedMessage::grab(Selection::Primary,
                               Some(99),
                               &[Format::UTF8_TEXT],
                               caps(),
                               limits()).unwrap()).unwrap();
    assert_eq!(engine.pending_fetch(), Some(fetch));
    assert_eq!(engine.queued_messages(), 0);
    deliver(&mut engine,
            epoch,
            OwnedMessage::text(Selection::Primary, "selected", caps(), limits()).unwrap()).unwrap();
    assert_eq!(engine.pending_fetch(), Some(fetch));
    frame(data("clipboard"), |frame| {
        let Effect::Import(import) = engine.receive(epoch, frame).unwrap()
        else
        {
            panic!()
        };
        engine.close(epoch).unwrap();
        assert!(matches!(engine.complete_import(import, Ok::<_, &str>(Pin::text(2, "clipboard"))),
                         Err(CompletionError::Engine(Error::StaleCompletion))));
    });
    assert_eq!(engine.local_revision(), Some(1));
    assert!(engine.pending_fetch().is_none());
    assert!(engine.output().is_none());
}

#[test]
fn host_release_quarantines_fetch_and_invalid_commit_reports_do_not_create_an_echo_baseline()
{
    let (mut engine, epoch) = ready();
    offer(&mut engine, epoch, 1);
    deliver(&mut engine,
            epoch,
            OwnedMessage::release(Selection::Clipboard, caps(), limits()).unwrap()).unwrap();
    assert!(engine.fetch_is_quarantined());
    frame(data("released"), |frame| {
        assert!(matches!(engine.receive(epoch, frame), Ok(Effect::Discarded)))
    });
    engine.poll(epoch).unwrap();
    assert!(engine.output().is_none());
    offer(&mut engine, epoch, 2);
    frame(data("actual"), |frame| {
        let Effect::Import(import) = engine.receive(epoch, frame).unwrap()
        else
        {
            panic!()
        };
        let result = engine.complete_import(import, Ok::<_, &str>(Pin::text(2, "different")));
        assert!(matches!(result, Err(CompletionError::Engine(Error::InvalidCompletion))));
    });
    assert_eq!(engine.local_revision(), Some(1));
    assert_eq!(engine.phase(), Phase::ResetRequired);
    assert_eq!(engine.observe(Pin::text(0, "decreasing")), Err(Error::RevisionOrder));
}
