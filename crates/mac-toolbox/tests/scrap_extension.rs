//! Public Scrap calls and an authenticated consumer share the production store.
//! Real INIT registry admission is exercised; native worker activation is not.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/allocations.rs"]
mod allocations;
use allocations::Observe;
use mac_system::testing::scrap_extension::{self as fixture, Content, Error, Module, Pin,
                                           ProbeError, Session, wire};
use mac_toolbox::{System, TypeCode};

fn bytes(module: &Module, session: Session, pin: Pin) -> Vec<u8>
{
    let info = module.info(session, pin).unwrap();
    let mut bytes = vec![0; info.total];
    let mut offset = 0;
    while offset < bytes.len()
    {
        let end = (offset + 7).min(bytes.len());
        let read = module.read(session, pin, offset, &mut bytes[offset..end]).unwrap();
        assert_eq!(read.revision, info.revision);
        assert_eq!(read.content, info.content);
        assert_eq!(read.total, info.total);
        assert_ne!(read.copied, 0);
        offset += read.copied;
    }
    bytes
}
fn message(error: &ProbeError, fragments: &[&str])
{
    let cause = error.to_string();
    for fragment in fragments
    {
        assert!(cause.contains(fragment), "{cause}");
    }
}

#[test]
fn actual_ready_init_and_declared_resident_service_authenticate_every_session_entry()
{
    fixture::with_services(|services, fixture| {
        // SAFETY: the fixture pins this real service table throughout the call.
        let system = unsafe { System::from_raw(services) }.unwrap();
        let not_ready = fixture.extension(true, false).open(true).unwrap_err();
        assert!(matches!(not_ready, ProbeError::Service(Error::NotReady)));
        message(&not_ready, &["initialization", "real activation"]);
        let undeclared = fixture.extension(false, true).open(true).unwrap_err();
        assert_eq!(undeclared.status(), mac_abi::ACCESS_DENIED);
        message(&undeclared, &["SCEX", "declare", "caller"]);
        let driver = fixture.driver().open(true).unwrap_err();
        assert!(matches!(driver, ProbeError::Service(Error::NotExtension)));
        message(&driver, &["INIT", "driver"]);

        let a = fixture.extension(true, true);
        let b = fixture.extension(true, true);
        let sa = a.open(true).unwrap();
        let sb = b.open(true).unwrap();
        assert!(sa.raw() > u32::MAX as u64 && sb.raw() > u32::MAX as u64);
        assert_ne!(sa, sb);
        let unknown = a.notice(Session::from_raw(0)).unwrap_err();
        assert!(matches!(unknown, ProbeError::Service(Error::SessionMissing)));
        message(&unknown, &["disconnected", "fresh"]);
        let foreign = b.notice(sa).unwrap_err();
        assert!(matches!(foreign, ProbeError::Service(Error::ForeignSession)));
        message(&foreign, &["another module", "own"]);
        system.scrap().put_text("authenticated Ω").unwrap();
        let pin = a.snapshot(sa).unwrap();
        let foreign = b.info(sb, pin).unwrap_err();
        assert!(matches!(foreign, ProbeError::Service(Error::ForeignPin)));
        message(&foreign, &["another module", "caller"]);
        let another_connection = a.open(true).unwrap();
        let wrong_session = a.info(another_connection, pin).unwrap_err();
        assert!(matches!(wrong_session, ProbeError::Service(Error::PinSession)));
        message(&wrong_session, &["different connection", "discard"]);
        assert_eq!(bytes(&a, sa, pin), "authenticated Ω".as_bytes());
    });
}

#[test]
fn committed_changes_coalesce_without_allocation_and_retained_reads_never_mix_revisions()
{
    fixture::with_services(|services, fixture| {
        // SAFETY: this scope owns the pinned, real resident service dispatch.
        let system = unsafe { System::from_raw(services) }.unwrap();
        let module = fixture.extension(true, true);
        let session = module.open(false).unwrap();
        let empty = module.snapshot(session).unwrap();
        let empty_info = module.info(session, empty).unwrap();
        assert_eq!(empty_info.content, Content::Empty);
        assert_eq!(empty_info.total, 0);
        assert_eq!(module.notice(session).unwrap(), None);
        let observed = Observe::begin(true, None);
        let cleared = system.scrap().clear();
        let notice = module.notice(session);
        for _ in 0..20
        {
            assert_eq!(module.notice(session).unwrap(), notice.as_ref().unwrap().clone());
        }
        let allocated = observed.finish();
        cleared.unwrap();
        assert_eq!(allocated, 0, "clear/notification allocated after session admission");
        let cleared_revision = notice.unwrap().unwrap();
        system.scrap().put_text("first\n日本 🦀").unwrap();
        system.scrap().put(TypeCode::new(*b"BINA"), b"extra flavor").unwrap();
        let final_revision = module.notice(session).unwrap().unwrap();
        assert_eq!(final_revision.get(), cleared_revision.get() + 2);
        // Delivery sees the latest complete committed store, not a copied queue
        // of intermediate clear/put payloads. A scheduler batching delay remains
        // a separate worker integration requirement.
        let pin = module.snapshot(session).unwrap();
        assert_eq!(module.info(session, pin).unwrap().revision, final_revision);
        assert_eq!(module.notice(session).unwrap(), None);
        system.scrap().clear().unwrap();
        system.scrap().put_text("new local clipboard").unwrap();
        let observed = Observe::begin(true, None);
        let mut first = [0; 5];
        let read = module.read(session, pin, 0, &mut first);
        let allocated = observed.finish();
        assert_eq!(allocated, 0, "pinned reads copied or admitted unrelated storage");
        assert_eq!(read.unwrap().revision, final_revision);
        assert_eq!(&first, b"first");
        assert_eq!(bytes(&module, session, pin), "first\n日本 🦀".as_bytes());
        assert_eq!(module.info(session, empty).unwrap().content, Content::Empty);
        let bad_offset = module.read(session, pin, usize::MAX, &mut first).unwrap_err();
        message(&bad_offset, &["offset", "exceeds", "retry"]);
        let mut too_big = [0; 4097];
        let too_big = module.read(session, pin, 0, &mut too_big).unwrap_err();
        message(&too_big, &["4096", "bounded"]);
        system.scrap().clear().unwrap();
        system.scrap().put(TypeCode::new(*b"PNGf"), b"unsupported").unwrap();
        let unsupported = module.snapshot(session).unwrap();
        assert_eq!(module.info(session, unsupported).unwrap().content, Content::Unsupported);
    });
}

#[test]
fn snapshot_admission_refusal_preserves_the_pending_change_and_old_pinned_payload()
{
    fixture::with_services(|services, fixture| {
        // SAFETY: fixture service ownership encloses every public SDK call.
        let system = unsafe { System::from_raw(services) }.unwrap();
        let module = fixture.extension(true, true);
        let session = module.open(true).unwrap();
        system.scrap().put_text("retained export").unwrap();
        let old = module.snapshot(session).unwrap();
        system.scrap().clear().unwrap();
        system.scrap().put_text("new committed text").unwrap();
        let pending = module.notice(session).unwrap();
        let observed = Observe::begin(true, None);
        let refusal = module.snapshot(session);
        let attempted = observed.finish();
        assert!(attempted > 0);
        let refusal = refusal.unwrap_err();
        assert!(matches!(refusal, ProbeError::Service(Error::Storage(_))));
        message(&refusal, &["storage", "retain", "retry"]);
        assert_eq!(module.notice(session).unwrap(), pending);
        assert_eq!(bytes(&module, session, old), b"retained export");
        let new = module.snapshot(session).unwrap();
        assert_eq!(module.notice(session).unwrap(), None);
        assert_eq!(bytes(&module, session, new), b"new committed text");
        module.close_pin(session, old).unwrap();
        let stale = module.info(session, old).unwrap_err();
        assert!(matches!(stale, ProbeError::Service(Error::PinMissing)));
        message(&stale, &["retired", "fresh"]);
    });
}

#[test]
fn import_requires_writable_exact_session_pin_and_cas_then_suppresses_only_its_origin()
{
    fixture::with_services(|services, fixture| {
        // SAFETY: the same production Manager serves SDK and extension calls.
        let system = unsafe { System::from_raw(services) }.unwrap();
        let module = fixture.extension(true, true);
        let origin = module.open(true).unwrap();
        let reader = module.open(false).unwrap();
        system.scrap().put_text("old").unwrap();
        let old = module.snapshot(origin).unwrap();
        let read_pin = module.snapshot(reader).unwrap();
        let readonly = module.import(reader, read_pin, "forbidden").unwrap_err();
        assert!(matches!(readonly, ProbeError::Service(Error::ReadOnly)));
        message(&readonly, &["observation only", "writable"]);
        system.scrap().clear().unwrap();
        system.scrap().put_text("newer local copy").unwrap();
        let info = system.scrap().info().unwrap();
        let observed = Observe::begin(true, None);
        let stale = module.import(origin, old, "delayed host text");
        let allocated = observed.finish();
        assert_eq!(allocated, 0, "stale import reserved or changed payload storage");
        let stale = stale.unwrap_err();
        assert_eq!(stale.status(), mac_abi::SCAN_STALE);
        message(&stale, &["changed", "keep the current Scrap", "retry"]);
        assert_eq!(system.scrap().info().unwrap(), info);
        let current = module.snapshot(origin).unwrap();
        let observed = Observe::fail_nth(1);
        let refusal = module.import(origin, current, "incoming host Ω");
        let allocated = observed.finish();
        assert!(allocated > 0);
        let refusal = refusal.unwrap_err();
        message(&refusal, &["storage", "retain", "retry"]);
        assert_eq!(system.scrap().info().unwrap(), info);
        let committed = module.import(origin, current, "incoming host Ω\n").unwrap();
        assert_eq!(module.notice(origin).unwrap(), None, "host import echoed to its origin");
        assert_eq!(module.notice(reader).unwrap(), Some(committed));
        assert_eq!(bytes(&module, origin, current), b"newer local copy");
        let imported = module.snapshot(reader).unwrap();
        assert_eq!(bytes(&module, reader, imported), "incoming host Ω\n".as_bytes());
        module.disconnect(origin).unwrap();
        let fresh = module.open(true).unwrap();
        assert_ne!(fresh, origin);
        let delayed = module.import(origin, current, "must not replace").unwrap_err();
        assert!(matches!(delayed, ProbeError::Service(Error::SessionMissing)));
        let retired_pin = module.info(fresh, current).unwrap_err();
        assert!(matches!(retired_pin, ProbeError::Service(Error::PinMissing)));
        let live = module.snapshot(fresh).unwrap();
        assert_eq!(bytes(&module, fresh, live), "incoming host Ω\n".as_bytes());
        let empty_revision = module.import(fresh, live, "").unwrap();
        assert_eq!(module.notice(fresh).unwrap(), None);
        assert_eq!(module.notice(reader).unwrap(), Some(empty_revision));
        let empty_text = module.snapshot(fresh).unwrap();
        let info = module.info(fresh, empty_text).unwrap();
        assert_eq!(info.content, Content::Text, "empty imported text became an empty Scrap");
        assert_eq!(info.total, 0);
    });
}

#[test]
fn stopped_real_module_revokes_cached_authority_and_owner_walk_keeps_exact_refused_tickets()
{
    fixture::with_services(|services, fixture| {
        // SAFETY: the fixture pins all routes through the cleanup campaign.
        let system = unsafe { System::from_raw(services) }.unwrap();
        let module = fixture.extension(true, true);
        let session = module.open(true).unwrap();
        system.scrap().put_text("retained until actual cleanup").unwrap();
        let _first = module.snapshot(session).unwrap();
        let _second = module.snapshot(session).unwrap();
        let before = module.summary();
        assert!(before.live >= 5);
        let stopped = module.stop_and_probe_cached(session).unwrap_err();
        assert!(matches!(stopped, ProbeError::Service(Error::Stopped)));
        message(&stopped, &["stopping", "revoked", "discarded"]);
        let fresh_auth = module.notice(session).unwrap_err();
        assert!(matches!(fresh_auth, ProbeError::Service(Error::NotReady)));
        let first = module.cleanup(true, false);
        assert_eq!(first.summary.live, 0);
        assert!(first.summary.pending >= 5);
        assert_eq!(first.summary.pending, first.summary.quarantined);
        assert_eq!(first.report.quarantined, first.failed.len());
        for (_, _, _, cause) in &first.failed
        {
            assert!(cause.contains("context")
                    && cause.contains("ticket")
                    && cause.contains("retained"),
                    "{cause}");
        }
        let again = module.cleanup(true, true);
        assert_eq!(again.report.invalidated, 0, "retry minted replacement cleanup tickets");
        let mut first_keys: Vec<_> =
            first.failed
                 .iter()
                 .map(|(kind, slot, generation, _)| (*kind, *slot, *generation))
                 .collect();
        let mut retry_keys: Vec<_> =
            again.failed
                 .iter()
                 .map(|(kind, slot, generation, _)| (*kind, *slot, *generation))
                 .collect();
        first_keys.sort();
        retry_keys.sort();
        assert_eq!(first_keys, retry_keys, "wrong-context retry changed exact cleanup identities");
        assert_eq!(again.summary.pending, first.summary.pending);
        let released = module.cleanup(false, true);
        assert_eq!(released.report.invalidated, 0);
        assert_eq!((released.summary.live,
                    released.summary.pending,
                    released.summary.quarantined,
                    released.summary.completion_faults),
                   (0, 0, 0, 0));
        assert!(released.failed.is_empty());
        // Scrap lives independently of extension death, and a different admitted
        // module cannot accidentally regain the stopped session's capabilities.
        system.scrap().clear().unwrap();
        system.scrap().put_text("local service remains usable").unwrap();
        let replacement = fixture.extension(true, true);
        let next = replacement.open(true).unwrap();
        assert_ne!(next, session);
        assert!(replacement.notice(session).is_err());
        let pin = replacement.snapshot(next).unwrap();
        assert_eq!(bytes(&replacement, next, pin), b"local service remains usable");
    });
}

fn wire_failure(module: &Module, operation: u32, input: &[u8], expected: i32, text: &[&str])
{
    let mut output = [0; wire::MAX_REPLY];
    let count = module.wire_call(operation, input, &mut output).unwrap();
    let wire::Reply::Failure { status,
                               cause } = wire::Reply::decode(&output[..count]).unwrap()
    else
    {
        panic!("invalid request was accepted");
    };
    assert_eq!(status, expected);
    for fragment in text
    {
        assert!(cause.contains(fragment), "{cause}");
    }
}

#[test]
fn real_wire_adapter_checks_buffers_and_authentication_before_effects_and_preserves_typed_causes()
{
    fixture::with_services(|services, fixture| {
        // SAFETY: this pinned native dispatch reaches the shared real Manager.
        let system = unsafe { System::from_raw(services) }.unwrap();
        let module = fixture.extension(true, true);
        let before = module.summary();
        let mut short = [0x55; wire::MIN_REPLY - 1];
        assert_eq!(module.wire_call(wire::OPEN, &1u32.to_le_bytes(), &mut short)
                         .unwrap_err()
                         .status(),
                   mac_abi::BAD_ARGUMENT);
        assert_eq!(short, [0x55; wire::MIN_REPLY - 1]);
        assert_eq!(module.summary().live,
                   before.live,
                   "reply refusal leaked an undisclosed session");
        wire_failure(&module,
                     wire::OPEN,
                     &2u32.to_le_bytes(),
                     mac_abi::BAD_ARGUMENT,
                     &["OPEN", "writable", "rebuild"]);
        wire_failure(&module,
                     wire::NOTICE,
                     &[0; 7],
                     mac_abi::BAD_ARGUMENT,
                     &["eight bytes", "rebuild"]);
        wire_failure(&module, u32::MAX, &[], mac_abi::NOT_SUPPORTED, &["unsupported", "rebuild"]);
        let mut output = [0; wire::MAX_REPLY];
        wire_failure(&fixture.driver(),
                     wire::OPEN,
                     &[1, 0, 0, 0],
                     mac_abi::ACCESS_DENIED,
                     &["INIT", "driver identities"]);
        wire_failure(&fixture.extension(false, true),
                     wire::OPEN,
                     &[1, 0, 0, 0],
                     mac_abi::ACCESS_DENIED,
                     &["SCEX", "declare"]);

        let count = module.wire_call(wire::OPEN, &[1, 0, 0, 0], &mut output).unwrap();
        let wire::Reply::Success(body) = wire::Reply::decode(&output[..count]).unwrap()
        else
        {
            panic!("real module could not open session");
        };
        assert_eq!(body.len(), 8);
        let session = u64::from_le_bytes(body.try_into().unwrap());
        system.scrap().put_text("wire Ω").unwrap();
        let count = module.wire_call(wire::SNAPSHOT, &session.to_le_bytes(), &mut output).unwrap();
        let wire::Reply::Success(body) = wire::Reply::decode(&output[..count]).unwrap()
        else
        {
            panic!("could not pin snapshot");
        };
        let pin = u64::from_le_bytes(body.try_into().unwrap());
        let mut pair = [0; 16];
        pair[..8].copy_from_slice(&session.to_le_bytes());
        pair[8..].copy_from_slice(&pin.to_le_bytes());
        let count = module.wire_call(wire::INFO, &pair, &mut output).unwrap();
        let wire::Reply::Success(body) = wire::Reply::decode(&output[..count]).unwrap()
        else
        {
            panic!("could not read metadata");
        };
        let metadata = wire::Metadata::decode(body).unwrap();
        assert_eq!(metadata.content, wire::Content::Text);
        assert_eq!(metadata.bytes as usize, "wire Ω".len());
        let mut read = [0; 24];
        read[..16].copy_from_slice(&pair);
        let count = module.wire_call(wire::READ, &read, &mut output).unwrap();
        let wire::Reply::Success(body) = wire::Reply::decode(&output[..count]).unwrap()
        else
        {
            panic!("could not read pin");
        };
        assert_eq!(body, "wire Ω".as_bytes());
        let foreign = fixture.extension(true, true);
        wire_failure(&foreign,
                     wire::NOTICE,
                     &session.to_le_bytes(),
                     mac_abi::ACCESS_DENIED,
                     &["another module", "own"]);
        let mut malformed = pair.to_vec();
        malformed.push(0xff);
        let before = system.scrap().info().unwrap();
        wire_failure(&module,
                     wire::IMPORT,
                     &malformed,
                     mac_abi::BAD_ARGUMENT,
                     &["valid UTF-8", "retained"]);
        assert_eq!(system.scrap().info().unwrap(), before);
        system.scrap().clear().unwrap();
        system.scrap().put_text("newer").unwrap();
        let mut stale = pair.to_vec();
        stale.extend_from_slice(b"late");
        wire_failure(&module, wire::IMPORT, &stale, mac_abi::SCAN_STALE, &["changed", "retry"]);
        let count =
            module.wire_call(wire::DISCONNECT, &session.to_le_bytes(), &mut output).unwrap();
        assert_eq!(wire::Reply::decode(&output[..count]), Some(wire::Reply::Success(&[])));
        wire_failure(&module, wire::INFO, &pair, mac_abi::NOT_FOUND, &["disconnected", "fresh"]);
    });
}
