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
