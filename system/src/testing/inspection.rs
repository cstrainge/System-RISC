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

// Host tests may use arbitrary trusted scheduler labels (7, 8, …). Resolve them
// to real facility-issued domains, never fabricate a DomainId from that label.
pub(crate) fn control_owner(alias: u64)
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
