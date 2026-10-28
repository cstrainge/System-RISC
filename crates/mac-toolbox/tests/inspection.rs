//! Public inspection SDK against the production dispatcher and owner directory.
#![cfg(not(target_arch = "riscv64"))]
use mac_system::testing::{files::with_owner, inspection as fixture, with_services};
use mac_toolbox::{Error, System, inspection::Scope};

#[test]
fn object_kind_registrations_are_public_named_and_generation_scoped()
{
    use mac_toolbox::inspection::{CapacityPolicy, RegistrationSource};
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let ordinary = fixture::add("Ordinary reader", 0);
        with_owner(ordinary, || {
            assert_eq!(system.inspection().snapshot(Scope::All).err().unwrap().cause,
                       Error::AccessDenied);
            let snapshot = system.inspection().object_kinds().unwrap();
            let baseline = snapshot.count;
            assert!(baseline >= 1);
            let control = (0..snapshot.count).map(|index| snapshot.at(index).unwrap())
                                             .find(|row| row.namespace == *b"CTRL")
                                             .unwrap();
            assert_eq!(control.namespace, *b"CTRL");
            assert_eq!(control.label.as_str(), "Managed controls");
            assert_eq!(control.provider.as_str(), "System Control Manager");
            assert_eq!(control.capacity, CapacityPolicy::GrowableUntilMemory);
            assert_eq!(control.source, RegistrationSource::Resident);
            assert_eq!(snapshot.get(control.id).unwrap(), control);
            assert!(control.admitting);
            assert_eq!(control.uses, 0);
            let error = snapshot.get(u64::MAX).unwrap_err();
            assert_eq!(error.cause, Error::NotFound);
            assert!(error.to_string().contains("resolve the current provider"));
            let error = snapshot.at(baseline).unwrap_err();
            assert_eq!(error.cause, Error::NotFound);
            assert!(error.to_string().contains("outside this registry snapshot"));
            let other = fixture::add("Another ordinary reader", 0);
            with_owner(other, || assert_eq!(snapshot.get(control.id).unwrap(), control));
            let extra = fixture::register_kind(*b"DEMO", "Example Extension");
            let error = snapshot.at(0).unwrap_err();
            assert_eq!(error.cause, Error::ScanStale);
            assert!(error.to_string().contains("obtain a new registry snapshot and retry"));
            let next = system.inspection().object_kinds().unwrap();
            assert_eq!(next.count, baseline + 1);
            let row = next.get(extra.id()).unwrap();
            assert_eq!(row.provider.as_str(), "Example Extension");
            assert_eq!(row.capacity, CapacityPolicy::PerDomainQuota { slots: 16 });
            assert_eq!(row.source, RegistrationSource::BundleResource { bundle: *b"FixtureKindData!",
                resource: mac_toolbox::ResourceKey::from_bytes(*b"KIND", 7) });
            let stale_id = extra.id();
            drop(extra);
            let current = system.inspection().object_kinds().unwrap();
            assert_eq!(current.count, baseline);
            assert_eq!(current.get(stale_id).unwrap_err().cause, Error::NotFound);
        });
        with_owner(u64::MAX, || {
            let error = system.inspection().object_kinds().err().unwrap();
            assert_eq!(error.cause, Error::AccessDenied);
            assert!(error.to_string().contains("live application context"));
        });
    });
}


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


#[test]
fn actual_module_domains_are_full_width_public_sdk_rows_with_exact_membership_epochs()
{
    use mac_toolbox::inspection::OwnerKind;
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let reader = fixture::add("Module inspector", mac_abi::inspection::INSPECT_DOMAINS);
        with_owner(reader, || {
            let own = system.inspection().snapshot(Scope::Own).unwrap();
            let before = system.inspection().snapshot(Scope::All).unwrap();
            let mut ids = Vec::new();
            for number in 1u128..=40
            {
                let name = format!("Loaded module {number}");
                let id = fixture::stage_module(number.to_le_bytes(), &name);
                assert!(id > u64::from(u32::MAX),
                        "module incarnation was truncated to a slot projection");
                ids.push(id);
            }
            assert_eq!(own.at(0).unwrap().incarnation,
                       reader,
                       "unrelated module changes invalidated own-process membership");
            let stale = before.at(0).unwrap_err();
            assert_eq!(stale.cause, Error::ScanStale);
            assert!(stale.to_string().contains("membership changed"));
            let current = system.inspection().snapshot(Scope::All).unwrap();
            assert!(current.includes_module_domains());
            assert!(!current.process_domains_only());
            assert_ne!(before.revision, current.revision,
                       "module epoch was discarded from the membership token");
            assert_eq!(current.count, 41);
            for (index, id) in ids.iter().copied().enumerate()
            {
                let ledger = current.ledger(index as u64 + 1).unwrap();
                let row = ledger.domain();
                assert_eq!((row.kind, row.incarnation), (OwnerKind::Module, id));
                assert_eq!(row.name.as_str(), format!("Loaded module {}", index + 1));
                assert!(row.accepting);
                assert_eq!((row.live, row.pending, ledger.len()),
                           (0, 0, 0),
                           "unmigrated driver objects must not be invented as ledger resources");
                let error = ledger.at(0).unwrap_err();
                assert_eq!(error.cause, Error::NotFound);
                assert!(error.to_string().contains("outside this ledger snapshot"));
            }
            fixture::retire_module(ids[0]).unwrap();
            assert_eq!(current.at(1).unwrap_err().cause, Error::ScanStale);
            let missing = fixture::retire_module(ids[0]).unwrap_err();
            assert!(missing.to_string().contains("absent")
                    || missing.to_string().contains("not found"));
            let next = system.inspection().snapshot(Scope::All).unwrap();
            assert_eq!(next.count, 40);
            for i in 0..next.count
            {
                assert_ne!(next.at(i).unwrap().incarnation, ids[0]);
            }
        });
    });
}

#[test]
fn provider_interfaces_are_real_module_resources_and_pending_calls_hold_cleanup_open()
{
    use mac_toolbox::inspection::OwnerKind;
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let reader =
            fixture::add("Driver lifetime inspector", mac_abi::inspection::INSPECT_DOMAINS);
        with_owner(reader, || {
            let (module, provider) =
                fixture::publish_module_interface(*b"interface-test-1", "Pending provider");
            assert!(provider > u64::from(u32::MAX));
            let snapshot = system.inspection().snapshot(Scope::All).unwrap();
            let index =
                (0..snapshot.count).find(|&i| snapshot.at(i).unwrap().incarnation == module)
                                   .unwrap();
            let ledger = snapshot.ledger(index).unwrap();
            assert_eq!(ledger.domain().kind, OwnerKind::Module);
            assert_eq!((ledger.domain().live, ledger.domain().pending), (2, 0));
            assert_eq!(ledger.len(), 2);
            let pools: Vec<_> = (0..ledger.len()).map(|i| ledger.at(i).unwrap()).collect();
            for code in [*b"DIFC", *b"DIFS"]
            {
                let pool = pools.iter().find(|p| p.kind == code).unwrap();
                assert_eq!((pool.live, pool.pending), (1, 0));
                let registration = system.inspection()
                                         .object_kinds()
                                         .unwrap()
                                         .get(pool.registration.unwrap())
                                         .unwrap();
                assert_eq!(registration.namespace, code);
                assert_eq!(registration.provider.as_str(), "System Module Manager");
            }
            let request = fixture::module_request(provider).unwrap();
            fixture::quiesce_module(module).unwrap();
            let refused = fixture::module_request(provider).unwrap_err();
            assert!(refused.to_string().contains("lifecycle state"));
            let walk = fixture::walk_module(module, false);
            assert_eq!(walk.report.quarantined, 1);
            assert_eq!(walk.failures.len(), 1);
            assert!(walk.failures[0].contains("1 requests")
                    && walk.failures[0].contains("acknowledged cancellation"),
                    "{:?}",
                    walk.failures);
            let stale = fixture::module_request(provider).unwrap_err();
            assert!(stale.to_string().contains("retired"));
            let pending = snapshot.ledger(index).unwrap().domain();
            assert_eq!((pending.live, pending.pending, pending.quarantined), (0, 2, 1));
            let busy = fixture::finish_module(module).unwrap_err();
            assert!(busy.to_string().contains("actual completion"));
            fixture::complete_module_request(request).unwrap();
            fixture::finish_module(module).unwrap();
            let done = snapshot.ledger(index).unwrap().domain();
            assert_eq!((done.live, done.pending, done.quarantined), (0, 0, 0));
            let (fresh_module, fresh_provider) =
                fixture::publish_module_interface(*b"interface-test-2", "Replacement provider");
            assert_ne!(fresh_module, module);
            assert_ne!(fresh_provider, provider);
            assert!(fixture::module_request(provider).unwrap_err().to_string().contains("retired"));
            let fresh_request = fixture::module_request(fresh_provider).unwrap();
            fixture::complete_module_request(fresh_request).unwrap();
        });
    });
}

#[test]
fn dependency_claims_are_enumerable_and_pin_exact_providers_until_the_owner_walk()
{
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let reader = fixture::add("Dependency inspector", mac_abi::inspection::INSPECT_DOMAINS);
        with_owner(reader, || {
            let (first, interface) =
                fixture::publish_module_interface(*b"dependency-one!!", "First provider");
            let (second, alternate) =
                fixture::publish_module_interface(*b"dependency-two!!", "Second provider");
            let consumer =
                fixture::stage_dependent_module(*b"dependency-user!", "Dependent", [0; 16]);
            assert_eq!(fixture::resolve_module_dependency(consumer).unwrap(), interface);
            let snapshot = system.inspection().snapshot(Scope::All).unwrap();
            let index =
                (0..snapshot.count).find(|&i| snapshot.at(i).unwrap().incarnation == consumer)
                                   .unwrap();
            let ledger = snapshot.ledger(index).unwrap();
            assert_eq!((ledger.domain().live, ledger.domain().pending), (2, 0));
            for code in [*b"DDEP", *b"DDST"]
            {
                let pool = (0..ledger.len()).map(|i| ledger.at(i).unwrap())
                                            .find(|p| p.kind == code)
                                            .unwrap();
                assert_eq!((pool.live, pool.pending), (1, 0));
                let kind = system.inspection()
                                 .object_kinds()
                                 .unwrap()
                                 .get(pool.registration.unwrap())
                                 .unwrap();
                assert_eq!(kind.namespace, code);
                assert!(kind.label.as_str().contains("dependency"));
                assert_eq!(kind.provider.as_str(), "System Module Manager");
            }
            fixture::quiesce_module(first).unwrap();
            let error = fixture::finish_module(first).unwrap_err();
            assert!(matches!(error, mac_system::drivers::Error::DependencyInUse {
                provider, dependent } if provider == first && dependent == consumer));
            assert!(error.to_string().contains("unload the dependent module before retrying"),
                    "{error}");
            let error = fixture::resolve_module_dependency(consumer).unwrap_err();
            assert!(error.to_string().contains("lifecycle state"), "{error}");
            assert_ne!(interface, alternate);
            fixture::retire_module(consumer).unwrap();
            fixture::finish_module(first).unwrap();
            let fresh = fixture::stage_dependent_module(*b"dependency-next!", "Next dependent",
                                                        *b"dependency-two!!");
            assert_ne!(fresh, consumer);
            assert_eq!(fixture::resolve_module_dependency(fresh).unwrap(), alternate);
            let request = fixture::module_request(alternate).unwrap();
            fixture::complete_module_request(request).unwrap();
            fixture::retire_module(fresh).unwrap();
            fixture::quiesce_module(second).unwrap();
            fixture::finish_module(second).unwrap();
        });
    });
}

#[test]
fn memory_addresses_require_policy_and_host_fixture_refuses_invented_native_mappings()
{
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let reader = fixture::add("Ordinary reader", 0);
        let inspector = fixture::add("Memory inspector", mac_abi::inspection::INSPECT_DOMAINS);
        with_owner(reader, || {
            let error = system.inspection().system_memory().err().unwrap();
            assert_eq!(error.cause, Error::AccessDenied);
            assert!(error.to_string().contains("System mapping details require a System \
                                                inspection policy grant"));
            assert!(system.inspection().object_kinds().is_ok(),
                    "public registration existence is not an inspector privilege");
        });
        with_owner(inspector, || {
            let error = system.inspection().system_memory().err().unwrap();
            assert_eq!(error.cause, Error::NotSupported);
            assert!(error.to_string().contains("native supervisor mappings are unavailable in \
                                                the host service fixture"));
            assert!(system.inspection().snapshot(Scope::All).is_ok(),
                    "refusal must leave other inspection services usable");
        });
        with_owner(u64::MAX, || {
            let error = system.inspection().system_memory().err().unwrap();
            assert_eq!(error.cause, Error::AccessDenied);
            assert!(error.to_string().contains("no live process domain"));
        });
    });
}

#[test]
fn own_execution_account_is_real_partial_coverage_with_refund_and_close()
{
    use mac_toolbox::inspection::MemoryAccounting;
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let owner = fixture::add("Execution account", 0);
        fixture::install_account(owner, 16_384, 65_536);
        with_owner(owner, || {
            let snapshot = system.inspection().snapshot(Scope::Own).unwrap();
            let charges = fixture::execution_charges(owner, 4096, 8192);
            let read = || snapshot.at(0).unwrap().accounting;
            match read()
            {
                MemoryAccounting::StackAndCpuOnly { kernel,
                                                    application,
                                                    closing } =>
                {
                    assert_eq!((kernel.limit, kernel.reserved, kernel.committed),
                               (16_384, 0, 4096));
                    assert_eq!((application.limit, application.reserved, application.committed),
                               (65_536, 0, 8192));
                    assert!(!closing);
                }
                _ => panic!("real account omitted")
            }
            let denied = system.inspection().snapshot(Scope::All).err().unwrap();
            assert_eq!(denied.cause, Error::AccessDenied);
            assert!(!denied.to_string().is_empty());
            assert!(!fixture::close_account(owner));
            assert!(matches!(read(),
                             MemoryAccounting::StackAndCpuOnly { closing: true,
                                                                 .. }));
            drop(charges);
            match read()
            {
                MemoryAccounting::StackAndCpuOnly { kernel,
                                                    application,
                                                    closing } =>
                {
                    assert_eq!((kernel.reserved,
                                kernel.committed,
                                application.reserved,
                                application.committed),
                               (0, 0, 0, 0));
                    assert!(closing);
                }
                _ => panic!("closed account omitted")
            }
            assert!(fixture::close_account(owner));
            // Host fixtures prove projection/refund; actual backing retirement
            // is covered by the native Process Manager acceptance fixture.
            fixture::try_retire(owner).unwrap();
        });
    });
}
