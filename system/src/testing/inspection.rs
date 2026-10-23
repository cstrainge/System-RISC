//! Trusted host scheduler fixture around the production inspection dispatcher.
use std::string::String;

use mac_object_table::{domain::{CapacityPolicy, Directory, DomainKind, KindCode, OwnerIndex},
                       shared::InvocationGate};
pub(crate) struct Gate;
impl InvocationGate for Gate
{
    fn run<R>(operation: impl FnOnce() -> R) -> R
    {
        operation()
    }
}
pub(crate) struct Metadata
{
    label:     String,
    authority: u64
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
                                       Metadata { label: label.into(),
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
