//! Trusted operation-lifetime transport for public-SDK acceptance. Publication,
//! invalidation, deferred child access and the owner walk are real services.
use std::sync::{Arc,
                atomic::{AtomicBool, Ordering}};

use mac_object_table::{domain::{CapacityPolicy, KindCode, PoolError, ReleasedResource},
                       registry::{Release, Source, Specification},
                       scope, store};

use super::super::inspection::Gate;
use crate::toolbox::handles::{OwnerRef, cleanup::Failure};
const ROOT: KindCode = KindCode(*b"SCIO");
const BACKING: KindCode = KindCode(*b"SCST");
struct Marker
{
    id:   u64,
    held: Arc<AtomicBool>
}
pub(in crate::testing) struct Scopes
{
    roots:    store::Store<Marker, Gate>,
    retiring: std::vec::Vec<store::Removed<Marker, Gate>>
}
#[derive(Clone, Copy)]
struct Cleanup(bool);
impl Release<super::super::Session, Failure> for Cleanup
{
    fn release(&self, session: &mut super::super::Session, ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        if self.0
        {
            return session.scopes.roots.release_storage(ticket).map_err(Into::into);
        }
        let hold = session.scopes
                          .roots
                          .releasing(&ticket)
                          .is_some_and(|root| root.held.load(Ordering::Acquire));
        if hold && session.scopes.retiring.len() == session.scopes.retiring.capacity()
        {
            return Err(Failure::file_retirement_storage(ticket,
                                                        session.scopes.retiring.len(),
                                                        session.scopes.retiring.capacity()));
        }
        let root = session.scopes.roots.release(ticket)?;
        if hold
        {
            session.scopes.retiring.push(root);
        }
        else
        {
            drop(root);
        }
        Ok(())
    }
}
impl Scopes
{
    pub(in crate::testing) fn new(registry:&mut crate::toolbox::handles::Registry<super::super::Session>,
                                  registrar: &OwnerRef)
                                  -> Self
    {
        let spec = |kind, label| {
            Specification { kind,
                            label,
                            provider: "Conformance operation transport",
                            capacity: CapacityPolicy::GrowableUntilMemory,
                            source: Source::Resident }
        };
        let root =
            registry.register(registrar, spec(ROOT, "Scoped I/O fixture roots"), Cleanup(false))
                    .unwrap();
        let backing = registry.register(registrar,
                                        spec(BACKING, "Scoped I/O fixture backing"),
                                        Cleanup(true))
                              .unwrap();
        Self { roots:    store::Store::owned(root, backing).unwrap(),
               retiring: std::vec::Vec::new() }
    }

    fn finish(&mut self, id: u64)
    {
        if let Some(root) = self.roots.remove(id)
        {
            drop(root);
            return;
        }
        if let Some(at) = self.retiring.iter().position(|root| root.id == id)
        {
            drop(self.retiring.swap_remove(at));
        }
    }
}

/// A real registered root held until the transport reports its completion.
/// It owns no file payload, ledger ticket, scope-close primitive or fake ACK.
pub struct Operation
{
    id:     u64,
    lease:  scope::Lease<Gate>,
    held:   Arc<AtomicBool>,
    active: bool
}
impl Operation
{
    pub fn open(&self, volume: u32, file: u32, fork: u32) -> Result<u32, mac_abi::Status>
    {
        super::super::SESSION.with(|slot| {
                                 slot.borrow_mut()
                                     .as_mut()
                                     .unwrap()
                                     .files
                                     .fixture_scoped_open(&self.lease, volume, file, fork)
                                     .map(|(handle, _)| handle)
                             })
    }

    pub fn read(&self, handle: u32, bytes: &mut [u8]) -> Result<usize, mac_abi::Status>
    {
        let mut call = mac_abi::files::FileCall { operation: mac_abi::files::READ,
                                                  handle,
                                                  ..Default::default() };
        super::super::SESSION.with(|slot| {
                                 slot.borrow_mut()
                                     .as_mut()
                                     .unwrap()
                                     .files
                                     .fixture_scoped_request(&self.lease, &mut call, bytes)
                             })?;
        Ok(call.transferred)
    }

    pub fn validate(&self) -> Result<(), scope::Failure>
    {
        self.lease.admitting()
    }

    pub fn finish(&mut self)
    {
        if !self.active
        {
            return;
        }
        self.held.store(false, Ordering::Release);
        super::super::SESSION.with(|slot| {
                                 let mut slot = slot.borrow_mut();
                                 let session = slot.as_mut().unwrap();
                                 session.scopes.finish(self.id);
                                 session.files.fixture_refresh_scopes();
                             });
        self.active = false;
    }
}
impl Drop for Operation
{
    fn drop(&mut self)
    {
        self.finish();
    }
}

#[derive(Debug)]
pub enum Error
{
    Context(mac_abi::Status),
    Pool(PoolError)
}
impl From<PoolError> for Error
{
    fn from(error: PoolError) -> Self
    {
        Self::Pool(error)
    }
}
impl core::fmt::Display for Error
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Context(status) =>
            {
                write!(f,
                       "scoped operation fixture could not resolve its trusted caller: status \
                        {status}")
            }
            Self::Pool(error) => error.fmt(f)
        }
    }
}
impl core::error::Error for Error
{
}

/// The closure cannot move the owned operation outside its service session.
/// Forgetting the borrowed reference cannot skip the RAII completion path.
pub fn with_operation<R>(run: impl FnOnce(&mut Operation) -> R) -> Result<R, Error>
{
    let mut operation = begin_operation()?;
    Ok(run(&mut operation))
}

fn begin_operation() -> Result<Operation, Error>
{
    let owner =
        super::super::inspection::caller_ledger(super::current_owner()).map_err(Error::Context)?;
    let reservation = scope::Reservation::try_new(&owner, ROOT)?;
    let lease = reservation.lease();
    let held = Arc::new(AtomicBool::new(true));
    let id =
        super::super::SESSION.with(|slot| {
                                 let mut slot = slot.borrow_mut();
                                 let session = slot.as_mut().unwrap();
                                 session.scopes
                                        .retiring
                                        .try_reserve(session.scopes.roots.iter().count() + 1)
                                        .map_err(|_| {
                                            PoolError::Storage { owner:           owner.owner(),
                                                                 kind:            ROOT,
                                                                 used:            session.scopes
                                                                                         .retiring
                                                                                         .len(),
                                                                 requested_slots: session.scopes
                                                                                         .retiring
                                                                                         .len()
                                                                                  + 1,
                                                                 limit:           None }
                                        })?;
                                 session.scopes.roots.insert_root_with(reservation, |id| {
                                                         Marker { id,
                                                                  held: held.clone() }
                                                     })
                             })?;
    Ok(Operation { id,
                   lease,
                   held,
                   active: true })
}
impl Drop for Scopes
{
    fn drop(&mut self)
    {
        // Ending the fixture world is not a transport ACK. Retain unique
        // guards for any promise the transport deliberately left outstanding.
        self.roots
            .remove_where_with(|_, root| root.held.load(Ordering::Acquire), core::mem::forget);
        if !self.retiring.is_empty()
        {
            core::mem::forget(core::mem::take(&mut self.retiring));
        }
    }
}
