//! Public inspection SDK against the production dispatcher and owner directory.
#![cfg(not(target_arch = "riscv64"))]
use mac_system::testing::{files::with_owner, inspection as fixture, with_services};
use mac_toolbox::{Error, System, inspection::Scope};

#[test]
fn pool_capacity_policies_preserve_limits_and_the_reason_for_a_hard_bound()
{
    use mac_toolbox::inspection::CapacityPolicy;
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let owner = fixture::add("Policy owner", 0);
        fixture::policy_pools(owner);
        with_owner(owner, || {
            let snapshot = system.inspection().snapshot(Scope::Own).unwrap().ledger(0).unwrap();
            assert_eq!(snapshot.len(), 3);
            assert_eq!(snapshot.at(0).unwrap().capacity, CapacityPolicy::GrowableUntilMemory);
            assert_eq!(snapshot.at(1).unwrap().capacity,
                       CapacityPolicy::PerDomainQuota { slots: 2 });
            let bound = snapshot.at(2).unwrap();
            match bound.capacity
            {
                CapacityPolicy::HardBound { slots,
                                            reason } =>
                {
                    assert_eq!(slots, 1);
                    assert_eq!(reason.as_str(), "one hardware channel");
                }
                other => panic!("wrong bound policy: {other:?}")
            }
            assert_eq!((bound.live, bound.slots, bound.births, bound.deaths), (1, 1, 1, 0));
        });
    });
}

#[test]
fn pool_snapshots_track_real_controls_reject_mutation_and_do_not_alias_another_caller()
{
    use mac_toolbox::{Rect, WindowId, controls::TextAlignment, inspection::CapacityPolicy};
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let owner = fixture::add("Pool owner", 0);
        let foreign = fixture::add("Other caller", 0);
        with_owner(owner, || {
            let members = system.inspection().snapshot(Scope::Own).unwrap();
            let empty = members.ledger(0).unwrap();
            assert!(empty.is_empty());
            let first = system.controls()
                              .create_static_text(WindowId(0),
                                                  Rect::new(0, 0, 80, 20),
                                                  "First",
                                                  Default::default(),
                                                  TextAlignment::Left)
                              .unwrap();
            let error = empty.at(0).unwrap_err();
            assert_eq!(error.cause, Error::ScanStale);
            assert!(error.to_string().contains("obtain a new ledger snapshot and retry"));
            let current = members.ledger(0).unwrap();
            assert_eq!(current.domain().incarnation, owner);
            assert_eq!(current.len(), 1);
            let row = current.at(0).unwrap();
            assert_eq!(row.kind, *b"CTRL");
            assert_eq!(row.capacity, CapacityPolicy::GrowableUntilMemory);
            assert_eq!((row.live, row.retired, row.pending, row.slots, row.births, row.deaths),
                       (1, 0, 0, 1, 1, 0));
            assert_eq!(row.generation_headroom, u64::MAX - 1);
            assert!(!row.counters_saturated);
            assert_eq!(current.at(0).unwrap(), row, "reading a snapshot must not mutate it");
            let error = current.at(1).unwrap_err();
            assert_eq!(error.cause, Error::NotFound);
            assert!(error.to_string().contains("outside this ledger snapshot"));
            with_owner(foreign, || {
                let error = current.at(0).unwrap_err();
                assert_eq!(error.cause, Error::AccessDenied);
                assert!(error.to_string().contains("different lifetime domain"));
            });
            let second = system.controls()
                               .create_static_text(WindowId(0),
                                                   Rect::new(0, 22, 80, 42),
                                                   "Second",
                                                   Default::default(),
                                                   TextAlignment::Left)
                               .unwrap();
            let error = current.at(0).unwrap_err();
            assert_eq!(error.cause, Error::ScanStale);
            drop(second);
            let next = members.ledger(0).unwrap().at(0).unwrap();
            assert_eq!((next.live, next.slots, next.births, next.deaths), (1, 2, 2, 1));
            assert_eq!(next.generation_headroom, u64::MAX - 2);
            drop(first);
            let final_row = members.ledger(0).unwrap().at(0).unwrap();
            assert_eq!((final_row.live, final_row.pending, final_row.births, final_row.deaths),
                       (0, 0, 2, 2));
            assert_eq!(final_row.slots, 2, "empty pools retain their generation history");
        });
    });
}

#[test]
fn self_scope_and_policy_granted_cross_scope_reject_foreign_and_stale_reads()
{
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let own = fixture::add("Ordinary app", 0);
        let inspector = fixture::add("Inspector", mac_abi::inspection::INSPECT_DOMAINS);
        with_owner(own, || {
            let snapshot = system.inspection().snapshot(Scope::Own).unwrap();
            assert_eq!(snapshot.count, 1);
            assert!(!snapshot.complete_ledgers());
            assert!(snapshot.process_domains_only());
            let row = snapshot.at(0).unwrap();
            assert_eq!(row.incarnation, own);
            assert_eq!(row.name.as_str(), "Ordinary app");
            let error = snapshot.at(1).unwrap_err();
            assert_eq!(error.cause, Error::NotFound);
            assert!(error.to_string().contains("outside this inspection snapshot"));
            let error = system.inspection().snapshot(Scope::All).err().unwrap();
            assert_eq!(error.cause, Error::AccessDenied);
            assert!(error.to_string().contains("System policy grant"));
        });
        let snapshot = with_owner(inspector, || system.inspection().snapshot(Scope::All).unwrap());
        assert_eq!(snapshot.count, 2);
        // Snapshot values confer no authority when passed to another caller.
        with_owner(own, || {
            let error = snapshot.at(1).unwrap_err();
            assert_eq!(error.cause, Error::AccessDenied);
            assert!(error.to_string().contains("own-domain inspection remains available"));
        });
        fixture::exercise_ledger(own);
        with_owner(inspector, || {
            let row = snapshot.at(0).unwrap();
            assert_eq!((row.live, row.births, row.deaths, row.slots, row.kinds), (1, 2, 1, 64, 1));
            let old_revision = row.ledger_revision;
            let pools = snapshot.ledger(0).unwrap();
            let pool = pools.at(0).unwrap();
            assert_eq!(pool.kind, *b"TEST");
            assert_eq!((pool.live, pool.births, pool.deaths), (1, 2, 1));
            assert_eq!(snapshot.at(0).unwrap().ledger_revision,
                       old_revision,
                       "reads must not mutate ledger revisions");
            let cleanup = fixture::begin_cleanup(own);
            let pending = snapshot.at(0).unwrap();
            assert_eq!(pools.at(0).unwrap_err().cause, Error::ScanStale);
            let pending_pool = snapshot.ledger(0).unwrap().at(0).unwrap();
            assert_eq!((pending_pool.live, pending_pool.pending), (0, 1));
            assert_eq!((pending.live, pending.pending), (0, 1));
            assert!(pending.ledger_revision > old_revision);
            drop(cleanup);
            let completed = snapshot.at(0).unwrap();
            assert_eq!((completed.live, completed.pending), (0, 0));
            assert!(completed.ledger_revision > pending.ledger_revision);
            fixture::add("New process", 0);
            let error = snapshot.at(0).unwrap_err();
            assert_eq!(error.cause, Error::ScanStale);
            assert!(error.to_string().contains("obtain a new snapshot and retry"));
            assert_eq!(system.inspection().snapshot(Scope::All).unwrap().count, 3);
        });
        with_owner(u64::MAX, || {
            let error = system.inspection().snapshot(Scope::Own).err().unwrap();
            assert_eq!(error.cause, Error::AccessDenied);
            assert!(error.to_string().contains("no live process domain"));
        });
    });
}

#[test]
fn native_theme_is_a_stable_resident_role_snapshot()
{
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let first = mac_toolbox::quickdraw::theme::current(system.client()).unwrap();
        assert_ne!(first.revision, 0);
        assert_ne!(first.text, first.content);
        assert_ne!(first.selected_text, first.selection);
        assert_eq!(first, mac_toolbox::quickdraw::theme::current(system.client()).unwrap());
    });
}
