//! Public inspection SDK against the production dispatcher and owner directory.
#![cfg(not(target_arch = "riscv64"))]
use mac_system::testing::{files::with_owner, inspection as fixture, with_services};
use mac_toolbox::{Error, System, inspection::Scope};

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
            assert_eq!(snapshot.at(0).unwrap().ledger_revision,
                       old_revision,
                       "reads must not mutate ledger revisions");
            let cleanup = fixture::begin_cleanup(own);
            let pending = snapshot.at(0).unwrap();
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
