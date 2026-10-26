//! Trusted host scheduler fixture around the production inspection dispatcher.
use std::string::String;

use mac_object_table::domain::{CapacityPolicy, Directory, DomainKind, KindCode, OwnerIndex};

pub(crate) use crate::toolbox::handles::InvocationGate as Gate;
pub(crate) struct Metadata
{
    label:           String,
    scheduler_alias: Option<u64>,
    authority:       u64
}
impl crate::toolbox::inspection::Subject for Metadata
{
    fn label(&self) -> &str
    {
        &self.label
    }

    fn foreground(&self) -> bool
    {
        false
    }

    fn authority(&self) -> u64
    {
        self.authority
    }
}
pub(crate) type Domains = Directory<Metadata, Gate>;
pub use mac_object_table::{domain::DirectoryError as RetirementError,
                           registry::Error as ProviderError};
pub fn add(label: &str, authority: u64) -> u64
{
    super::SESSION.with(|session| {
                      let mut session = session.borrow_mut();
                      let directory = &mut session.as_mut().unwrap().inspection;
                      let ledger = OwnerIndex::issue(DomainKind::Process).unwrap();
                      directory.insert(ledger,
                                       Metadata { scheduler_alias: None,
                                                  label: label.into(),
                                                  authority })
                               .unwrap()
                               .incarnation()
                  })
}

/// Attempt actual directory retirement; a refusal must leave the domain and
/// its retryable manager resources inspectable through the public SDK.
pub fn try_retire(owner: u64) -> Result<(), RetirementError>
{
    super::SESSION.with(|slot| {
                      slot.borrow_mut().as_mut().unwrap().inspection.remove(owner).map(|_| ())
                  })
}

pub struct RegisteredFixtureKind
{
    registrar: crate::toolbox::handles::OwnerRef,
    kind:      KindCode,
    id:        u64
}
impl RegisteredFixtureKind
{
    pub fn id(&self) -> u64
    {
        self.id
    }
}
impl Drop for RegisteredFixtureKind
{
    fn drop(&mut self)
    {
        // Keep provider destruction outside the registry/session borrow.
        let cleanup = super::SESSION.with(|slot| {
                                        let mut slot = slot.borrow_mut();
                                        slot.as_mut()
                                            .unwrap()
                                            .registry
                                            .unregister(&self.registrar, self.kind)
                                            .unwrap()
                                    });
        drop(cleanup);
    }
}
pub fn register_kind(namespace: [u8; 4], provider: &str) -> RegisteredFixtureKind
{
    let registrar = crate::toolbox::handles::OwnerRef::try_new(OwnerIndex::issue(DomainKind::Module).unwrap()).unwrap();
    let kind = KindCode(namespace);
    let id = super::SESSION.with(|slot| {
        let mut slot = slot.borrow_mut();
        slot.as_mut().unwrap().registry.register(&registrar,
            mac_object_table::registry::Specification { kind, label: "Fixture object kind", provider,
                capacity: CapacityPolicy::PerDomainQuota { slots: 16 },
                source: mac_object_table::registry::Source::BundleResource {
                    bundle: *b"FixtureKindData!", kind: KindCode(*b"KIND"), id: 7 } },
            crate::toolbox::handles::cleanup::Complete(crate::toolbox::controls::objects::ReleaseControls)).unwrap().id()
    });
    RegisteredFixtureKind { registrar,
                            kind,
                            id }
}

#[derive(Debug)]
pub struct WalkOutcome
{
    pub report:   mac_object_table::walk::Report,
    pub failures: std::vec::Vec<String>
}
/// Fault injection is only in provider resolution; publication, invalidation,
/// quarantine, retry and destruction all use the real shared facility.
pub fn walk_registered(owner: u64, provider_unavailable: bool, retry: bool) -> WalkOutcome
{
    super::SESSION.with(|slot| {
        let mut slot = slot.borrow_mut();
        let session = slot.as_mut().unwrap();
        let ledger = session.inspection.get(owner).unwrap().ledger().clone();
        let resolve = |session: &super::Session, key: mac_object_table::domain::ResourceKey| {
            if provider_unavailable { Err(mac_object_table::registry::Error::Missing { kind: key.kind() }) }
            else { session.registry.dispatch(key.kind()) }
        };
        let mut failures = std::vec::Vec::new();
        let mut failed = |_: mac_object_table::domain::ResourceKey, error: &mac_object_table::walk::Failure<mac_object_table::registry::Error, crate::toolbox::handles::cleanup::Cause>| {
            failures.push(std::format!("{error}"));
        };
        let report = if retry { mac_object_table::walk::retry(session, &ledger, resolve, &mut failed) }
            else { mac_object_table::walk::drain(session, &ledger, resolve, &mut failed) }.unwrap();
        WalkOutcome { report, failures }
    })
}
/// Real ledger publication/invalidation, not canned inspector results.
pub fn exercise_ledger(owner: u64)
{
    super::SESSION.with(|session| {
                      let session = session.borrow();
                      let ledger =
                          session.as_ref().unwrap().inspection.get(owner).unwrap().ledger();
                      let kind = KindCode(*b"TEST");
                      ledger.add_pool(kind, CapacityPolicy::GrowableUntilMemory).unwrap();
                      ledger.grow(kind, 64).unwrap();
                      let key = ledger.allocate(kind).unwrap();
                      let released = ledger.invalidate(key).unwrap();
                      ledger.complete_release(released).unwrap();
                      ledger.allocate(kind).unwrap();
                  });
}

/// Prepare real metadata policies; observations still go through the public
/// dispatcher/SDK. No policy values are fabricated by the inspection service.
pub fn policy_pools(owner: u64)
{
    super::SESSION.with(|session| {
                      let session = session.borrow();
                      let ledger =
                          session.as_ref().unwrap().inspection.get(owner).unwrap().ledger();
                      for (code, policy) in
                          [(*b"GROW", CapacityPolicy::GrowableUntilMemory),
                           (*b"QUOT", CapacityPolicy::PerDomainQuota { slots: 2 }),
                           (*b"BNDR",
                            CapacityPolicy::HardBound { slots:  1,
                                                        reason: "one hardware channel" })]
                      {
                          let kind = KindCode(code);
                          ledger.add_pool(kind, policy).unwrap();
                          ledger.grow(kind, policy.limit().unwrap_or(4)).unwrap();
                          ledger.allocate(kind).unwrap();
                      }
                  });
}

pub struct Cleanup
{
    ledger: mac_object_table::shared::OwnerRef<Gate>,
    ticket: Option<mac_object_table::domain::ReleasedResource>
}
impl Drop for Cleanup
{
    fn drop(&mut self)
    {
        if let Some(ticket) = self.ticket.take()
        {
            self.ledger.complete_release(ticket).unwrap();
        }
    }
}
/// Hold the real cleanup barrier until this trusted fixture's guard is dropped.
pub fn begin_cleanup(owner: u64) -> Cleanup
{
    super::SESSION.with(|session| {
                      let session = session.borrow();
                      let ledger =
                          session.as_ref().unwrap().inspection.get(owner).unwrap().ledger().clone();
                      let ticket = ledger.invalidate_next().expect("fixture has a live resource");
                      Cleanup { ledger,
                                ticket: Some(ticket) }
                  })
}

/// Resolve only an existing trusted fixture context; inspection never enrolls
/// a caller or fabricates authority for an unknown scheduler label.
pub(super) fn caller_identity(directory: &Domains, alias: u64) -> Option<u64>
{
    directory.get(alias)
             .or_else(|| {
                 directory.iter().find(|entry| entry.value().scheduler_alias == Some(alias))
             })
             .map(|entry| entry.ledger().owner().incarnation())
}

// Host tests may use arbitrary trusted scheduler labels (7, 8, …). Resolve them
// to real facility-issued domains, never fabricate a DomainId from that label.
pub(crate) fn caller_ledger(alias: u64)
                            -> Result<crate::toolbox::handles::OwnerRef, mac_abi::Status>
{
    super::SESSION.with(|session| {
                      let mut session = session.borrow_mut();
                      let directory =
                          &mut session.as_mut().ok_or(mac_abi::NOT_SUPPORTED)?.inspection;
                      if let Some(entry) = directory.get(alias)
                      {
                          return Ok(entry.ledger().clone());
                      }
                      if let Some(entry) =
                          directory.iter()
                                   .find(|entry| entry.value().scheduler_alias == Some(alias))
                      {
                          return Ok(entry.ledger().clone());
                      }
                      let ledger =
                          OwnerIndex::issue(DomainKind::Process).map_err(|_| mac_abi::NO_MEMORY)?;
                      let id = directory.insert(ledger,
                                                Metadata { scheduler_alias: Some(alias),
                                                           label:           "Fixture app".into(),
                                                           authority:       0 })
                                        .map_err(|_| mac_abi::NO_MEMORY)?;
                      Ok(directory.get(id.incarnation()).unwrap().ledger().clone())
                  })
}

/// Host-unwind fault injection only. The production store owns publication,
/// invalidation and the retained capability; no native unwind claim is made.
pub struct InterruptedCleanup
{
    registrar:        crate::toolbox::handles::OwnerRef,
    kind:             KindCode,
    pub registration: u64
}
impl InterruptedCleanup
{
    pub fn try_unregister(&self) -> Result<(), mac_object_table::registry::Error>
    {
        let removed = super::SESSION.with(|slot| {
                                        slot.borrow_mut()
                                            .as_mut()
                                            .unwrap()
                                            .registry
                                            .unregister(&self.registrar, self.kind)
                                    })?;
        drop(removed);
        Ok(())
    }
}
pub fn interrupt_cleanup(owner: u64, backing: bool) -> InterruptedCleanup
{
    struct Panics;
    impl Drop for Panics
    {
        fn drop(&mut self)
        {
            panic!("fixture destructor did not return");
        }
    }
    struct Cleanup;
    impl mac_object_table::registry::Release<super::Session> for Cleanup
    {
        fn release(&self, _: &mut super::Session,
                   ticket: mac_object_table::domain::ReleasedResource)
                   -> Result<(), mac_object_table::domain::CompletionError>
        {
            use mac_object_table::registry::CleanupFailure;
            Err(mac_object_table::domain::CompletionError::registration_mismatch(ticket))
        }
    }
    let registrar=crate::toolbox::handles::OwnerRef::try_new(OwnerIndex::issue(DomainKind::Module).unwrap()).unwrap();
    let (ledger,lease)=super::SESSION.with(|slot| {
        let mut slot=slot.borrow_mut();let session=slot.as_mut().unwrap();
        let ledger=session.inspection.get(owner).unwrap().ledger().clone();
        let lease=session.registry.register(&registrar,mac_object_table::registry::Specification {
            kind:KindCode(*b"FALT"),label:"Interrupted destructor",provider:"Inspection fault fixture",
            capacity:CapacityPolicy::PerDomainQuota {slots:1},source:mac_object_table::registry::Source::Resident
        },crate::toolbox::handles::cleanup::Complete(Cleanup)).unwrap();
        (ledger,lease)
    });
    let (registration, kind) = if backing
    {
        let storage=super::SESSION.with(|slot|slot.borrow_mut().as_mut().unwrap().registry.register(&registrar,
            mac_object_table::registry::Specification {kind:KindCode(*b"FABS"),label:"Interrupted backing",
                provider:"Inspection backing fixture",capacity:CapacityPolicy::GrowableUntilMemory,
                source:mac_object_table::registry::Source::Resident},crate::toolbox::handles::cleanup::Complete(Cleanup)).unwrap());
        let registration = storage.id();
        let mut store =
            mac_object_table::store::Store::<u8, _, Panics>::owned_state(lease, storage, |_| {
                Ok(Panics)
            }).unwrap();
        store.prepare(&ledger).unwrap();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(store))).is_err());
        (registration, KindCode(*b"FABS"))
    }
    else
    {
        let registration = lease.id();
        let mut store = mac_object_table::store::Store::registered(lease);
        let id = store.insert(&ledger, Panics).unwrap();
        let guard = store.remove(id).unwrap();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(guard))).is_err());
        drop(store);
        (registration, KindCode(*b"FALT"))
    };
    InterruptedCleanup { registrar,
                         kind,
                         registration }
}


/// Stage real module metadata and its shared-facility domain. Host tests do
/// not execute a guest driver; native campaigns exercise the entry separately.
pub fn stage_module(identity: [u8; 16], name: &str) -> u64
{
    stage_module_with_offers(identity, name, &[])
}
fn stage_module_with_offers(identity: [u8; 16], name: &str, offers: &[mac_module::Offer]) -> u64
{
    stage_module_with_requirements(identity, name, offers, &[])
}
pub fn stage_dependent_module(identity: [u8; 16], name: &str, provider: [u8; 16]) -> u64
{
    stage_module_with_requirements(identity,
                                   name,
                                   &[],
                                   &[mac_module::Dependency { module:   provider,
                                                              class:
                                                                  mac_module::ClassId(*b"TEST"),
                                                              version:  1,
                                                              optional: false }])
}
pub fn resolve_module_dependency(module: u64) -> Result<u64, crate::drivers::Error>
{
    super::SESSION.with(|slot| {
                      slot.borrow()
                          .as_ref()
                          .unwrap()
                          .modules
                          .resolve(module, mac_module::ClassId(*b"TEST"), 1)
                  })
}
fn stage_module_with_requirements(identity: [u8; 16], name: &str, offers: &[mac_module::Offer],
                                  dependencies: &[mac_module::Dependency])
                                  -> u64
{
    let spec = mac_module::Spec { kind: mac_module::PackageKind::Driver,
                                  kernel_abi: mac_abi::ABI_VERSION,
                                  target: mac_module::Target::RVA23U64,
                                  identity,
                                  code_id: 0,
                                  code_digest: [0; 32],
                                  heap_bytes: 0,
                                  stack_bytes: 4096,
                                  name,
                                  version: "0.1.0",
                                  provenance: "inspection fixture",
                                  offers,
                                  dependencies,
                                  services: &[],
                                  devices: &[] };
    let mut bytes = std::vec![0;spec.encoded_len().unwrap()];
    spec.encode(&mut bytes).unwrap();
    let contract = mac_module::Contract::decode(&bytes).unwrap();
    super::SESSION.with(|slot| {
                      slot.borrow_mut()
                          .as_mut()
                          .unwrap()
                          .modules
                          .stage(contract, mac_module::PackageKind::Driver, *b"DRVR")
                          .unwrap()
                  })
}
pub fn retire_module(module: u64) -> Result<(), crate::drivers::Error>
{
    finish_with_module_walk(module, false)
}

/// Production registry publication, with only the absent hardware callback
/// replaced by an explicit test completion. Inspection still uses public traps.
pub fn publish_module_interface(identity: [u8; 16], name: &str) -> (u64, u64)
{
    let offer = mac_module::Offer { class:          mac_module::ClassId(*b"TEST"),
                                    version:        1,
                                    request_schema: 1,
                                    reply_schema:   1,
                                    max_request:    16,
                                    max_reply:      16,
                                    flags:          mac_module::Offer::KERNEL_ONLY,
                                    capabilities:   0 };
    let module = stage_module_with_offers(identity, name, &[offer]);
    super::SESSION.with(|slot| {
                      let mut slot = slot.borrow_mut();
                      let registry = &mut slot.as_mut().unwrap().modules;
                      registry.commit(module).unwrap();
                      let provider = registry.find(offer.class, 1)
                                             .find(|(_, i)| i.module == module)
                                             .unwrap()
                                             .0;
                      (module, provider)
                  })
}
pub fn module_request(provider: u64) -> Result<u32, crate::drivers::Error>
{
    super::SESSION.with(|slot| {
                      slot.borrow_mut().as_mut().unwrap().modules.begin(0, provider, 1, 0, 16)
                  })
}
pub fn complete_module_request(ticket: u32) -> Result<(), crate::drivers::Error>
{
    super::SESSION.with(|slot| {
                      slot.borrow_mut().as_mut().unwrap().modules.complete(0, ticket, 0).map(|_| ())
                  })
}
pub fn quiesce_module(module: u64) -> Result<(), crate::drivers::Error>
{
    super::SESSION.with(|slot| slot.borrow_mut().as_mut().unwrap().modules.quiesce(module))
}
pub fn finish_module(module: u64) -> Result<(), crate::drivers::Error>
{
    finish_with_module_walk(module, true)
}
fn finish_with_module_walk(module: u64, published: bool) -> Result<(), crate::drivers::Error>
{
    let finish = || {
        super::SESSION.with(|slot| {
                          let mut slot = slot.borrow_mut();
                          let registry = &mut slot.as_mut().unwrap().modules;
                          if published
                          {
                              registry.finish_quiesce(module)
                          }
                          else
                          {
                              registry.rollback(module)
                          }
                      })
    };
    let result = finish();
    use mac_object_table::domain::DirectoryError;

    use crate::drivers::Error;
    if matches!(result,
                Err(Error::Domain(DirectoryError::LiveResources { .. }
                                  | DirectoryError::PendingResources { .. }
                                  | DirectoryError::CompletionFaults { .. })))
    {
        // The transport's one real catalog needs the Session context, not a
        // fabricated Registry-only context. Release its RefCell borrow first.
        walk_module(module, false);
        walk_module(module, true);
        finish()
    }
    else
    {
        result
    }
}
pub fn walk_module(module: u64, retry: bool) -> WalkOutcome
{
    super::SESSION.with(|slot| {
        let mut slot = slot.borrow_mut();
        let session = slot.as_mut().unwrap();
        let owner = session.modules.module_ledger(module).unwrap();
        let resolve = |session: &super::Session, key: mac_object_table::domain::ResourceKey| session.registry.dispatch(key.kind());
        let mut failures = std::vec::Vec::new();
        let mut failed = |_: mac_object_table::domain::ResourceKey, error: &mac_object_table::walk::Failure<mac_object_table::registry::Error, crate::toolbox::handles::cleanup::Cause>| failures.push(std::format!("{error}"));
        let report = if retry { mac_object_table::walk::retry(session, &owner, resolve, &mut failed) }
            else { mac_object_table::walk::drain(session, &owner, resolve, &mut failed) }.unwrap();
        WalkOutcome { report, failures }
    })
}
