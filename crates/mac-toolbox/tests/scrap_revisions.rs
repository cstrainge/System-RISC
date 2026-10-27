//! Real public Scrap mutations and the resident immutable-snapshot primitive.
//! Extension registration/worker and native QEMU sharing are separate gates.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/allocations.rs"]
mod allocations;

use allocations::Observe;
use mac_system::testing::scrap::{self, ChangeError, ProbeError, Snapshot, SnapshotContent};
use mac_toolbox::{Error, System, TypeCode};

fn text(snapshot: &Snapshot) -> Option<&str>
{
    match snapshot.content()
    {
        SnapshotContent::Text(text) => Some(text),
        _ => None
    }
}

#[test]
fn every_committed_native_change_advances_revision_while_classic_count_advances_only_on_clear()
{
    scrap::with_services(|services| {
        // SAFETY: fixture owns and pins the real service table for this scope.
        let system = unsafe { System::from_raw(services) }.unwrap();
        let initial = scrap::snapshot().unwrap();
        assert!(matches!(initial.content(), SnapshotContent::Empty));
        system.scrap().clear().unwrap();
        let cleared = scrap::snapshot().unwrap();
        let generation = system.scrap().info().unwrap().count;
        assert_eq!(cleared.revision().get(), initial.revision().get() + 1);
        system.scrap().put_text("first Ω 🦀").unwrap();
        let first = scrap::snapshot().unwrap();
        assert_eq!(first.revision().get(), cleared.revision().get() + 1);
        assert_eq!(system.scrap().info().unwrap().count, generation);
        system.scrap().put(TypeCode::new(*b"BINA"), b"binary").unwrap();
        let mixed = scrap::snapshot().unwrap();
        assert_eq!(mixed.revision().get(), first.revision().get() + 1);
        assert_eq!(text(&mixed), Some("first Ω 🦀"));
        // Preserve classic first-record semantics when a producer appends the
        // same flavor more than once; no merged or mixed export is invented.
        system.scrap().put_text("second").unwrap();
        assert_eq!(text(&scrap::snapshot().unwrap()), Some("first Ω 🦀"));
        assert_eq!(system.scrap().info().unwrap().count, generation);
        system.scrap().clear().unwrap();
        system.scrap().put(TypeCode::new(*b"PNGf"), b"not plain text").unwrap();
        assert!(matches!(scrap::snapshot().unwrap().content(), SnapshotContent::Unsupported));
    });
}

#[test]
fn immutable_text_snapshot_pins_one_revision_without_allocating_or_borrowing_the_manager()
{
    scrap::with_services(|services| {
        // SAFETY: fixture owns the pinned service table throughout this call.
        let system = unsafe { System::from_raw(services) }.unwrap();
        system.scrap().put_text("pinned\n日本").unwrap();
        let observed = Observe::begin(true, None);
        let snapshot = scrap::snapshot();
        let count = observed.finish();
        assert_eq!(count, 0, "taking an immutable text pin must not copy payload storage");
        let snapshot = snapshot.unwrap();
        let observed = Observe::begin(true, None);
        let retained = snapshot.clone();
        let count = observed.finish();
        assert_eq!(count, 0);
        system.scrap().clear().unwrap();
        system.scrap().put_text("new content").unwrap();
        assert_eq!(text(&snapshot), Some("pinned\n日本"));
        assert_eq!(text(&retained), Some("pinned\n日本"));
        assert_eq!(text(&scrap::snapshot().unwrap()), Some("new content"));
    });
}

#[test]
fn delayed_import_refuses_before_allocation_and_retry_compares_the_current_revision()
{
    scrap::with_services(|services| {
        // SAFETY: this fixture's live service descriptor is scoped and pinned.
        let system = unsafe { System::from_raw(services) }.unwrap();
        system.scrap().put_text("original").unwrap();
        let old = scrap::snapshot().unwrap();
        system.scrap().clear().unwrap();
        system.scrap().put_text("user's newer copy").unwrap();
        let current = scrap::snapshot().unwrap();
        let info = system.scrap().info().unwrap();
        let observed = Observe::begin(true, None);
        let refusal = scrap::compare_replace(old.revision(), "delayed host result");
        let count = observed.finish();
        assert_eq!(count, 0, "stale version refusal precedes every storage reservation");
        let error = refusal.unwrap_err();
        assert!(matches!(error, ProbeError::Change(ChangeError::Conflict { .. })));
        let cause = error.to_string();
        for expected in ["Scrap Manager", "changed", "retry"]
        {
            assert!(cause.contains(expected), "{cause}");
        }
        assert_eq!(system.scrap().info().unwrap(), info);
        assert_eq!(text(&scrap::snapshot().unwrap()), Some("user's newer copy"));
        let committed = scrap::compare_replace(current.revision(), "fresh Ω\n").unwrap();
        assert_eq!(committed.get(), current.revision().get() + 1);
        assert_eq!(text(&scrap::snapshot().unwrap()), Some("fresh Ω\n"));
        assert_eq!(text(&old), Some("original"));
    });
}

#[test]
fn replacement_allocation_refusal_preserves_revision_count_and_payload_then_recovers()
{
    scrap::with_services(|services| {
        // SAFETY: service borrows cannot outlive the owning fixture scope.
        let system = unsafe { System::from_raw(services) }.unwrap();
        system.scrap().put_text("retained").unwrap();
        let before = scrap::snapshot().unwrap();
        let info = system.scrap().info().unwrap();
        // Nonempty text refuses byte admission; empty text separately refuses
        // its immutable owner admission. Neither may publish partial state.
        for value in ["incoming host text", ""]
        {
            let observed = Observe::begin(true, None);
            let result = scrap::compare_replace(before.revision(), value);
            let attempts = observed.finish();
            assert!(attempts > 0);
            let error = result.unwrap_err();
            assert!(matches!(error, ProbeError::Change(ChangeError::Allocation)));
            let cause = error.to_string();
            for expected in ["Scrap Manager", "storage", "retry"]
            {
                assert!(cause.contains(expected), "{cause}");
            }
            assert_eq!(scrap::snapshot().unwrap().revision(), before.revision());
            assert_eq!(system.scrap().info().unwrap(), info);
            assert_eq!(text(&scrap::snapshot().unwrap()), Some("retained"));
        }
        scrap::compare_replace(before.revision(), "").unwrap();
        assert_eq!(text(&scrap::snapshot().unwrap()), Some(""));
        assert!(!matches!(scrap::snapshot().unwrap().content(), SnapshotContent::Empty));
    });
}

#[test]
fn rejected_native_put_does_not_publish_a_change_revision()
{
    scrap::with_services(|services| {
        // SAFETY: fixture keeps the table live for all SDK calls here.
        let system = unsafe { System::from_raw(services) }.unwrap();
        system.scrap().put_text("keep").unwrap();
        let before = scrap::snapshot().unwrap();
        let oversized = vec![0; mac_toolbox::scrap::MAX_BYTES];
        assert_eq!(system.scrap().put(TypeCode::new(*b"BINA"), &oversized), Err(Error::NoMemory));
        assert_eq!(scrap::snapshot().unwrap().revision(), before.revision());
        assert_eq!(text(&scrap::snapshot().unwrap()), Some("keep"));
    });
}

#[test]
fn every_replacement_admission_can_fail_without_losing_the_previous_clipboard()
{
    scrap::with_services(|services| {
        // SAFETY: the fixture owns this service table for the test scope.
        let system = unsafe { System::from_raw(services) }.unwrap();
        system.scrap().put_text("original").unwrap();
        let first = scrap::snapshot().unwrap();
        let observed = Observe::begin(false, None);
        let result = scrap::compare_replace(first.revision(), "admitted replacement");
        let admissions = observed.finish();
        result.unwrap();
        assert!(admissions > 0);
        let before = scrap::snapshot().unwrap();
        let info = system.scrap().info().unwrap();
        for ordinal in 1..=admissions
        {
            let observed = Observe::fail_at(ordinal);
            let result = scrap::compare_replace(before.revision(), "another replacement");
            let attempted = observed.finish();
            assert_eq!(attempted, ordinal);
            assert!(matches!(result, Err(ProbeError::Change(ChangeError::Allocation))));
            assert_eq!(scrap::snapshot().unwrap().revision(), before.revision());
            assert_eq!(text(&scrap::snapshot().unwrap()), Some("admitted replacement"));
            assert_eq!(system.scrap().info().unwrap(), info);
        }
        scrap::compare_replace(before.revision(), "retry succeeded").unwrap();
        assert_eq!(text(&scrap::snapshot().unwrap()), Some("retry succeeded"));
    });
}
