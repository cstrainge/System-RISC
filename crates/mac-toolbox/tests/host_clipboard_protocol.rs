//! Deterministic SPICE wire exchange with the real authenticated SCEX store and
//! public Scrap SDK. No scheduled worker, guest activation or host clipboard is
//! supplied by this fixture; protocol labels never replace resident authority.
#![cfg(not(target_arch = "riscv64"))]

use std::sync::Arc;

use mac_host_clipboard::{CompletionError, Config, Content, Direction, Effect, Engine, Epoch,
                         Error as BridgeError, Settings, Snapshot};
use mac_system::testing::{scrap::ChangeError,
                          scrap_extension::{self as fixture, Content as ResidentContent,
                                            Error as ServiceError, Module, Pin, ProbeError,
                                            Session}};
use mac_toolbox::{System, TypeCode};
use mac_vdagent_protocol::{Capabilities, Capability, ClipboardData, Decoder, Format, Frame,
                           Limits, Message, OwnedMessage, Selection};

/// The test transport reads a genuine resident snapshot through its bounded
/// facade. Engine ownership retains this immutable adapter, never Manager or an
/// owner/process ID. The original native pin remains the real CAS operand.
#[derive(Clone)]
struct Captured
{
    pin:      Pin,
    revision: u64,
    kind:     ResidentContent,
    text:     Arc<String>
}
impl Snapshot for Captured
{
    fn revision(&self) -> u64
    {
        self.revision
    }

    fn content(&self) -> Content<'_>
    {
        match self.kind
        {
            ResidentContent::Empty => Content::Empty,
            ResidentContent::Text => Content::Text(&self.text),
            ResidentContent::Unsupported => Content::Unsupported
        }
    }
}
fn capture(module: &Module, session: Session) -> Captured
{
    let pin = module.snapshot(session).unwrap();
    let info = module.info(session, pin).unwrap();
    let mut bytes = vec![0; info.total];
    let mut offset = 0;
    while offset < bytes.len()
    {
        let end = (offset + 7).min(bytes.len());
        let read = module.read(session, pin, offset, &mut bytes[offset..end]).unwrap();
        assert_eq!((read.revision, read.content, read.total),
                   (info.revision, info.content, info.total));
        assert_ne!(read.copied, 0);
        offset += read.copied;
    }
    Captured { pin,
               revision: info.revision.get(),
               kind: info.content,
               text: Arc::new(String::from_utf8(bytes).unwrap()) }
}
fn caps() -> Capabilities
{
    Capabilities::empty().with(Capability::ClipboardByDemand)
                         .with(Capability::ClipboardSelection)
                         .with(Capability::ClipboardGrabSerial)
}
fn wire_bytes(message: OwnedMessage) -> Vec<u8>
{
    let mut wire = Vec::new();
    for chunk in message.chunks(1024).unwrap()
    {
        for part in chunk.segments()
        {
            wire.extend_from_slice(part);
        }
    }
    wire
}
fn decoded<R>(message: OwnedMessage, run: impl FnOnce(&Frame<'_>) -> R) -> R
{
    let bytes = wire_bytes(message);
    let mut decoder = Decoder::new(Limits::qemu());
    let mut offset = 0;
    while offset < bytes.len()
    {
        let end = (offset + 5).min(bytes.len());
        let feed = decoder.feed(&bytes[offset..end]).unwrap();
        offset += feed.consumed;
        if let Some(frame) = feed.frame
        {
            assert_eq!(offset, bytes.len());
            return run(&frame);
        }
        assert_eq!(offset, end);
    }
    panic!("complete fixture message did not decode")
}
fn drain(engine: &mut Engine<Captured>) -> Vec<u8>
{
    let mut wire = Vec::new();
    while let Some(output) = engine.output()
    {
        let token = output.token;
        let accepted = output.remaining_bytes();
        for part in output.segments
        {
            wire.extend_from_slice(part);
        }
        engine.accepted(token, accepted).unwrap();
    }
    wire
}
fn ready(snapshot: Captured) -> (Engine<Captured>, Epoch)
{
    ready_with_settings(snapshot, Settings::default())
}
fn ready_with_settings(snapshot: Captured, settings: Settings) -> (Engine<Captured>, Epoch)
{
    let mut engine = Engine::new(Config::qemu().with_settings(settings));
    let epoch = engine.open_after_transport_reset(snapshot).unwrap();
    // QEMU queues its own initial request before receiving our one guest
    // advertisement. This crossed request must not produce a second caps frame.
    decoded(OwnedMessage::announce(caps(), true, Limits::qemu()).unwrap(), |frame| {
        assert!(matches!(engine.receive(epoch, frame), Ok(Effect::Idle)));
    });
    assert_eq!(engine.queued_messages(), 1);
    drain(&mut engine);
    decoded(OwnedMessage::announce(caps(), false, Limits::qemu()).unwrap(), |frame| {
        assert!(matches!(engine.receive(epoch, frame), Ok(Effect::Negotiated)));
    });
    assert_eq!(engine.queued_messages(), 0);
    (engine, epoch)
}

#[test]
fn guest_to_host_json_policy_exports_public_copies_with_an_observation_only_real_session()
{
    fixture::with_services(|services, fixture| {
        // SAFETY: the actual resident service table is pinned for this scope.
        let system = unsafe { System::from_raw(services) }.unwrap();
        let module = fixture.extension(true, true);
        let session = module.open(false).unwrap();
        system.scrap().put_text("retained guest text").unwrap();
        let baseline = capture(&module, session);
        let before = system.scrap().info().unwrap();
        let settings = Settings::decode(br#"{"version":1,"direction":"guest-to-host"}"#).unwrap();
        let (mut engine, epoch) = ready_with_settings(baseline.clone(), settings);
        let offered = OwnedMessage::grab(Selection::Clipboard,
                                         Some(40),
                                         &[Format::UTF8_TEXT],
                                         caps(),
                                         Limits::qemu()).unwrap();
        decoded(offered, |frame| {
            let error = engine.receive(epoch, frame).unwrap_err();
            assert!(matches!(error, BridgeError::DirectionDenied { .. }));
            cause(&error, &["guest-to-host", "retain local Scrap", "changing direction"]);
        });
        assert!(engine.pending_fetch().is_none());
        assert_eq!(engine.queued_messages(), 0);
        let refused =
            module.import(session, baseline.pin, "direction is not a write grant").unwrap_err();
        assert!(matches!(refused, ProbeError::Service(ServiceError::ReadOnly)));
        cause(&refused, &["observation only", "writable"]);
        assert_eq!(system.scrap().info().unwrap(), before);
        assert_eq!(public_text(&system), "retained guest text");
        system.scrap().clear().unwrap();
        system.scrap().put_text("public guest Ω copy").unwrap();
        engine.observe(capture(&module, session)).unwrap();
        let bytes = drain(&mut engine);
        let mut decoder = Decoder::new(Limits::qemu());
        let feed = decoder.feed(&bytes).unwrap();
        let Message::Grab(grab) = feed.frame.unwrap().message(caps()).unwrap()
        else
        {
            panic!()
        };
        assert_eq!(grab.serial(), Some(41));
        assert!(grab.formats().contains(Format::UTF8_TEXT));
        decoded(OwnedMessage::request(Selection::Clipboard,
                                      Format::UTF8_TEXT,
                                      caps(),
                                      Limits::qemu()).unwrap(),
                |frame| {
                    engine.receive(epoch, frame).unwrap();
                });
        let bytes = drain(&mut engine);
        let mut decoder = Decoder::new(Limits::qemu());
        let feed = decoder.feed(&bytes).unwrap();
        let Message::Data(ClipboardData::Text { text,
                                                .. }) =
            feed.frame.unwrap().message(caps()).unwrap()
        else
        {
            panic!()
        };
        assert_eq!(text, "public guest Ω copy");
        assert_eq!(public_text(&system), text);
    });
}

#[test]
fn host_to_guest_json_policy_prevents_export_and_policy_replacement_revokes_the_old_session()
{
    fixture::with_services(|services, fixture| {
        // SAFETY: the fixture owns this real service dispatch throughout policy replacement.
        let system = unsafe { System::from_raw(services) }.unwrap();
        let module = fixture.extension(true, true);
        let session = module.open(true).unwrap();
        system.scrap().put_text("private local text").unwrap();
        let initial = capture(&module, session);
        let settings = Settings::decode(br#"{"version":1,"direction":"host-to-guest"}"#).unwrap();
        let (mut engine, epoch) = ready_with_settings(initial, settings);
        system.scrap().clear().unwrap();
        system.scrap().put_text("newer private Ω text").unwrap();
        let local = capture(&module, session);
        engine.observe(local.clone()).unwrap();
        assert_eq!(engine.queued_messages(), 0);
        decoded(OwnedMessage::request(Selection::Clipboard,
                                      Format::UTF8_TEXT,
                                      caps(),
                                      Limits::qemu()).unwrap(),
                |frame| {
                    engine.receive(epoch, frame).unwrap();
                });
        let bytes = drain(&mut engine);
        let mut decoder = Decoder::new(Limits::qemu());
        let feed = decoder.feed(&bytes).unwrap();
        assert!(matches!(feed.frame.unwrap().message(caps()).unwrap(),
                         Message::Data(ClipboardData::Unavailable { .. })));
        let mut committed_pin = None;
        host_offer(&mut engine, epoch, 1, &[Format::UTF8_TEXT]);
        decoded(host_text("allowed host Ω text"), |frame| {
            let Effect::Import(import) = engine.receive(epoch, frame).unwrap()
            else
            {
                panic!()
            };
            assert_eq!(import.token().expected_revision(), local.revision);
            let committed = module.import(session, local.pin, import.text()).unwrap();
            let snapshot = capture(&module, session);
            assert_eq!(snapshot.revision, committed.get());
            committed_pin = Some(snapshot.pin);
            engine.complete_import(import, Ok::<_, ProbeError>(snapshot)).unwrap();
        });
        assert_eq!(public_text(&system), "allowed host Ω text");
        let replacement_policy = Settings::new(true, Direction::GuestToHost);
        assert_eq!(engine.settings().direction(),
                   Direction::HostToGuest,
                   "creating settings cannot mutate an active epoch or its resident authority");
        host_offer(&mut engine, epoch, 2, &[Format::UTF8_TEXT]);
        decoded(host_text("old-policy delayed text"), |frame| {
            let Effect::Import(import) = engine.receive(epoch, frame).unwrap()
            else
            {
                panic!()
            };
            module.disconnect(session).unwrap();
            engine.close(epoch).unwrap();
            let next = module.open(false).unwrap();
            assert_ne!(next, session);
            let snapshot = capture(&module, next);
            let (mut replacement, next_epoch) =
                ready_with_settings(snapshot.clone(), replacement_policy);
            assert_eq!(replacement.settings().direction(), Direction::GuestToHost);
            let error = module.import(session, committed_pin.unwrap(), import.text()).unwrap_err();
            assert!(matches!(error, ProbeError::Service(ServiceError::SessionMissing)));
            cause(&error, &["disconnected", "fresh authenticated session"]);
            assert!(matches!(engine.complete_import(import, Err::<Captured, _>(error)),
                             Err(CompletionError::Engine(BridgeError::StaleCompletion))));
            let error = module.import(next, snapshot.pin,
                                      "new policy cannot upgrade observation authority")
                              .unwrap_err();
            assert!(matches!(error, ProbeError::Service(ServiceError::ReadOnly)));
            cause(&error, &["observation only", "writable"]);
            assert_eq!(public_text(&system), "allowed host Ω text");
            replacement.close(next_epoch).unwrap();
            module.disconnect(next).unwrap();
        });
    });
}
fn host_offer(engine: &mut Engine<Captured>, epoch: Epoch, serial: u32, formats: &[Format])
{
    let grab = OwnedMessage::grab(Selection::Clipboard,
                                  Some(serial),
                                  formats,
                                  caps(),
                                  Limits::qemu()).unwrap();
    decoded(grab, |frame| {
        engine.receive(epoch, frame).unwrap();
    });
    drain(engine); // The exact request is now accepted; the response may arrive.
}
fn host_text(text: &str) -> OwnedMessage
{
    OwnedMessage::text(Selection::Clipboard, text, caps(), Limits::qemu()).unwrap()
}
fn cause(error: &impl core::fmt::Display, fragments: &[&str])
{
    let text = error.to_string();
    for fragment in fragments
    {
        assert!(text.contains(fragment), "{text}");
    }
}
fn public_text(system: &System<'_>) -> String
{
    let mut bytes = [0; 512];
    system.scrap().text(&mut bytes).unwrap().into()
}

#[test]
fn real_host_import_compares_the_engine_pin_and_suppresses_only_its_authenticated_origin()
{
    fixture::with_services(|services, fixture| {
        // SAFETY: the fixture pins this actual public service table for the scope.
        let system = unsafe { System::from_raw(services) }.unwrap();
        let module = fixture.extension(true, true);
        let origin = module.open(true).unwrap();
        let sibling = module.open(false).unwrap();
        let other = fixture.extension(true, true);
        let observer = other.open(false).unwrap();
        system.scrap().clear().unwrap();
        system.scrap().put_text("before import").unwrap();
        system.scrap().put(TypeCode::new(*b"BINA"), b"native flavor").unwrap();
        let baseline = capture(&module, origin);
        let sibling_baseline = capture(&module, sibling);
        let observer_baseline = capture(&other, observer);
        let info = system.scrap().info().unwrap();
        let (mut engine, epoch) = ready(baseline.clone());
        host_offer(&mut engine, epoch, 1, &[Format::UTF8_TEXT]);
        let incoming = "host café 日本 🦀\r\nline two\n";
        decoded(host_text(incoming), |frame| {
            let Effect::Import(import) = engine.receive(epoch, frame).unwrap()
            else
            {
                panic!()
            };
            assert_eq!(import.token().expected_revision(), baseline.revision);
            assert_eq!(module.info(origin, baseline.pin).unwrap().revision.get(),
                       import.token().expected_revision());
            let readonly = module.import(sibling, sibling_baseline.pin, import.text()).unwrap_err();
            assert!(matches!(readonly, ProbeError::Service(ServiceError::ReadOnly)));
            cause(&readonly, &["observation only", "writable"]);
            assert_eq!(system.scrap().info().unwrap(), info);
            // Each Module call authenticates the actual ready INIT afresh. The
            // exact resident pin, rather than a wire revision, drives the CAS.
            let committed = module.import(origin, baseline.pin, import.text()).unwrap();
            assert_eq!(module.notice(origin).unwrap(), None);
            assert_eq!(module.notice(sibling).unwrap(), Some(committed));
            assert_eq!(other.notice(observer).unwrap(), Some(committed));
            let imported = capture(&module, origin);
            assert_eq!(imported.revision, committed.get());
            engine.complete_import(import, Ok::<_, ProbeError>(imported.clone())).unwrap();
            engine.observe(imported).unwrap();
        });
        assert_eq!(public_text(&system), incoming);
        let after = system.scrap().info().unwrap();
        assert_eq!((after.flavors, after.count), (1, info.count.wrapping_add(1)));
        assert_eq!(engine.queued_messages(), 0, "origin engine re-exported its own import");
        assert_eq!(module.notice(sibling).unwrap().unwrap().get(),
                   engine.local_revision().unwrap());
        assert_eq!(other.notice(observer).unwrap().unwrap().get(),
                   engine.local_revision().unwrap());
        assert_eq!(module.info(sibling, sibling_baseline.pin).unwrap().revision.get(),
                   baseline.revision);
        assert_eq!(other.info(observer, observer_baseline.pin).unwrap().revision.get(),
                   baseline.revision);
    });
}

#[test]
fn delayed_host_data_is_drained_after_real_local_copy_and_the_new_native_text_exports()
{
    fixture::with_services(|services, fixture| {
        // SAFETY: fixture ownership encloses the real SDK and authenticated calls.
        let system = unsafe { System::from_raw(services) }.unwrap();
        let module = fixture.extension(true, true);
        let session = module.open(true).unwrap();
        system.scrap().put_text("old local").unwrap();
        let baseline = capture(&module, session);
        let (mut engine, epoch) = ready(baseline.clone());
        host_offer(&mut engine, epoch, 1, &[Format::UTF8_TEXT]);
        system.scrap().clear().unwrap();
        system.scrap().put_text("user's newer Ω copy").unwrap();
        system.scrap().put(TypeCode::new(*b"BINA"), b"preserve this too").unwrap();
        let expected = system.scrap().info().unwrap();
        let newer = capture(&module, session);
        assert!(newer.revision > baseline.revision);
        engine.observe(newer).unwrap();
        assert!(engine.fetch_is_quarantined());
        drain(&mut engine);
        decoded(host_text("delayed host replacement"), |frame| {
            assert!(matches!(engine.receive(epoch, frame), Ok(Effect::Discarded)),
                    "stale DATA must never become an import effect");
        });
        assert_eq!(system.scrap().info().unwrap(), expected);
        assert_eq!(public_text(&system), "user's newer Ω copy");
        let mut binary = [0; 32];
        let count = system.scrap().read(TypeCode::new(*b"BINA"), 0, &mut binary).unwrap();
        assert_eq!(&binary[..count], b"preserve this too");
        let request = OwnedMessage::request(Selection::Clipboard,
                                            Format::UTF8_TEXT,
                                            caps(),
                                            Limits::qemu()).unwrap();
        decoded(request, |frame| {
            engine.receive(epoch, frame).unwrap();
        });
        let bytes = drain(&mut engine);
        let mut decoder = Decoder::new(Limits::qemu());
        let result = decoder.feed(&bytes).unwrap();
        let Message::Data(ClipboardData::Text { text,
                                                .. }) =
            result.frame.unwrap().message(caps()).unwrap()
        else
        {
            panic!()
        };
        assert_eq!(text, "user's newer Ω copy");
        assert_eq!(module.info(session, baseline.pin).unwrap().revision.get(), baseline.revision);
    });
}

#[test]
fn missed_change_notification_still_hits_real_cas_and_preserves_the_informative_cause()
{
    fixture::with_services(|services, fixture| {
        // SAFETY: this host fixture keeps the actual service table pinned.
        let system = unsafe { System::from_raw(services) }.unwrap();
        let module = fixture.extension(true, true);
        let session = module.open(true).unwrap();
        system.scrap().put_text("original").unwrap();
        let baseline = capture(&module, session);
        let (mut engine, epoch) = ready(baseline.clone());
        host_offer(&mut engine, epoch, 1, &[Format::UTF8_TEXT]);
        // The real store changes before the worker consumes its notification.
        system.scrap().clear().unwrap();
        system.scrap().put_text("real newer content").unwrap();
        let actual = module.notice(session).unwrap().unwrap().get();
        let expected_info = system.scrap().info().unwrap();
        decoded(host_text("late result"), |frame| {
            let Effect::Import(import) = engine.receive(epoch, frame).unwrap()
            else
            {
                panic!()
            };
            assert_eq!(import.token().expected_revision(), baseline.revision);
            let refusal = module.import(session, baseline.pin, import.text()).unwrap_err();
            let ProbeError::Service(ServiceError::Change(ChangeError::Conflict { expected,
                                                                                 actual:
                                                                                     resident_actual })) =
                &refusal
            else
            {
                panic!("{refusal}")
            };
            assert_eq!(expected.get(), import.token().expected_revision());
            assert_eq!(resident_actual.get(), actual);
            cause(&refusal, &["changed", "keep the current Scrap", "retry"]);
            let refusal = engine.complete_import(import, Err::<Captured, _>(refusal)).unwrap_err();
            assert!(matches!(refusal, CompletionError::Import(_)));
            cause(&refusal, &["could not commit Scrap", "changed", "retry"]);
        });
        assert_eq!(system.scrap().info().unwrap(), expected_info);
        assert_eq!(public_text(&system), "real newer content");
        engine.observe(capture(&module, session)).unwrap();
        assert_eq!(engine.local_revision(), Some(actual));
    });
}

#[test]
fn session_disconnect_and_real_owner_quiesce_reject_delayed_imports_before_effects()
{
    fixture::with_services(|services, fixture| {
        // SAFETY: the fixture owns real service routing throughout revocation.
        let system = unsafe { System::from_raw(services) }.unwrap();
        let module = fixture.extension(true, true);
        let session = module.open(true).unwrap();
        system.scrap().put_text("retained local").unwrap();
        let baseline = capture(&module, session);
        let expected_info = system.scrap().info().unwrap();
        let (mut engine, epoch) = ready(baseline.clone());
        host_offer(&mut engine, epoch, 1, &[Format::UTF8_TEXT]);
        decoded(host_text("after disconnect"), |frame| {
            let Effect::Import(import) = engine.receive(epoch, frame).unwrap()
            else
            {
                panic!()
            };
            module.disconnect(session).unwrap();
            engine.close(epoch).unwrap();
            let refusal = module.import(session, baseline.pin, import.text()).unwrap_err();
            assert!(matches!(refusal, ProbeError::Service(ServiceError::SessionMissing)));
            cause(&refusal, &["disconnected", "discard delayed", "fresh"]);
            let refusal = engine.complete_import(import, Err::<Captured, _>(refusal)).unwrap_err();
            assert!(matches!(refusal, CompletionError::Engine(BridgeError::StaleCompletion)));
            cause(&refusal, &["stale", "newer Scrap"]);
        });
        let retired_pin = baseline.pin;
        let next = module.open(true).unwrap();
        assert_ne!(next, session);
        let baseline = capture(&module, next);
        let (mut replacement, next_epoch) = ready(baseline.clone());
        assert_eq!(next_epoch, epoch,
                   "independent engine labels are deliberately not resident grants");
        let old = module.import(session, retired_pin, "numeric engine epoch is no authority")
                        .unwrap_err();
        assert!(matches!(old, ProbeError::Service(ServiceError::SessionMissing)));
        cause(&old, &["disconnected", "authenticated"]);
        let old =
            module.import(next, retired_pin, "fresh session cannot revive an old pin").unwrap_err();
        assert!(matches!(old, ProbeError::Service(ServiceError::PinMissing)));
        cause(&old, &["retired", "fresh"]);
        host_offer(&mut replacement, next_epoch, 1, &[Format::UTF8_TEXT]);
        decoded(host_text("after owner stopped"), |frame| {
            let Effect::Import(import) = replacement.receive(next_epoch, frame).unwrap()
            else
            {
                panic!()
            };
            let cached = module.stop_and_probe_cached(next).unwrap_err();
            assert!(matches!(cached, ProbeError::Service(ServiceError::Stopped)));
            cause(&cached, &["stopping", "revoked", "discarded"]);
            let refusal = module.import(next, baseline.pin, import.text()).unwrap_err();
            assert!(matches!(refusal, ProbeError::Service(ServiceError::NotReady)));
            cause(&refusal, &["initialization", "real activation"]);
            replacement.close(next_epoch).unwrap();
            assert!(matches!(replacement.complete_import(import, Err::<Captured, _>(refusal)),
                             Err(CompletionError::Engine(BridgeError::StaleCompletion))));
        });
        let cleaned = module.cleanup(false, false);
        assert_eq!((cleaned.summary.live,
                    cleaned.summary.pending,
                    cleaned.summary.quarantined,
                    cleaned.summary.completion_faults),
                   (0, 0, 0, 0));
        assert!(cleaned.failed.is_empty());
        assert_eq!(system.scrap().info().unwrap(), expected_info);
        assert_eq!(public_text(&system), "retained local");
        system.scrap().clear().unwrap();
        system.scrap().put_text("local retry still works").unwrap();
        assert_eq!(public_text(&system), "local retry still works");
    });
}

#[test]
fn none_and_unsupported_offers_preserve_native_flavors_but_empty_utf8_is_real_text()
{
    fixture::with_services(|services, fixture| {
        // SAFETY: this fixture pins the real SDK and admitted module route.
        let system = unsafe { System::from_raw(services) }.unwrap();
        let module = fixture.extension(true, true);
        let session = module.open(true).unwrap();
        system.scrap().put(TypeCode::new(*b"PNGf"), b"native image").unwrap();
        system.scrap().put(TypeCode::new(*b"BINA"), b"native binary").unwrap();
        let baseline = capture(&module, session);
        assert_eq!(baseline.kind, ResidentContent::Unsupported);
        let before = system.scrap().info().unwrap();
        let (mut engine, epoch) = ready(baseline.clone());
        host_offer(&mut engine, epoch, 1, &[Format::from_raw(2)]);
        assert!(engine.pending_fetch().is_none());
        host_offer(&mut engine, epoch, 2, &[Format::UTF8_TEXT]);
        decoded(OwnedMessage::unavailable(Selection::Clipboard, caps(), Limits::qemu()).unwrap(),
                |frame| {
                    assert!(matches!(engine.receive(epoch, frame), Ok(Effect::Unavailable)));
                });
        assert_eq!(system.scrap().info().unwrap(), before);
        for (kind, expected) in
            [(*b"PNGf", &b"native image"[..]), (*b"BINA", &b"native binary"[..])]
        {
            let mut bytes = [0; 32];
            let count = system.scrap().read(TypeCode::new(kind), 0, &mut bytes).unwrap();
            assert_eq!(&bytes[..count], expected);
        }
        host_offer(&mut engine, epoch, 3, &[Format::UTF8_TEXT]);
        decoded(host_text(""), |frame| {
            let Effect::Import(import) = engine.receive(epoch, frame).unwrap()
            else
            {
                panic!()
            };
            assert_eq!(import.text(), "");
            assert_eq!(import.token().expected_revision(), baseline.revision);
            let committed = module.import(session, baseline.pin, import.text()).unwrap();
            let empty_text = capture(&module, session);
            assert_eq!(empty_text.kind, ResidentContent::Text);
            assert_eq!(empty_text.revision, committed.get());
            engine.complete_import(import, Ok::<_, ProbeError>(empty_text)).unwrap();
        });
        let after = system.scrap().info().unwrap();
        assert_eq!((after.flavors, after.size, after.count), (1, 8, before.count.wrapping_add(1)));
        assert_eq!(public_text(&system), "");
        system.scrap().clear().unwrap();
        let no_records = capture(&module, session);
        assert_eq!(no_records.kind, ResidentContent::Empty);
        assert_ne!(no_records.revision, engine.local_revision().unwrap());
        assert_eq!(system.scrap().info().unwrap().flavors, 0);
    });
}
