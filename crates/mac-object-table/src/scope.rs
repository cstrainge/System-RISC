//! Authority for children of an already-admitted resource. A scope never reopens
//! ordinary owner admission and its closure never acknowledges lower I/O.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::{fmt,
           sync::atomic::{AtomicBool, AtomicU64, Ordering}};

use spinning_top::Spinlock;

use crate::{domain::{DomainId, KindCode, OwnerIndex, PoolError, ResourceKey},
            shared::{InvocationGate, OwnerRef}};

// Every published scope consumes a boot-unique native identity and closes
// once. Unpublished reservations do not advance this counter, so the native
// identity space is exhausted before this change clock could wrap.
static CLOSED_REVISION: AtomicU64 = AtomicU64::new(0);
pub fn closed_revision() -> u64
{
    CLOSED_REVISION.load(Ordering::Acquire)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure
{
    Unpublished,
    Closed,
    RootGone
    {
        root: ResourceKey
    },
    ForeignOwner
    {
        expected: DomainId,
        actual:   DomainId
    },
    WrongKind
    {
        expected: KindCode,
        actual:   KindCode
    }
}
impl fmt::Display for Failure
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Unpublished =>
            {
                f.write_str("root resource is not published; no child operation was admitted")
            }
            Self::Closed =>
            {
                f.write_str("resource scope is closed; no new child operation was admitted")
            }
            Self::RootGone { root } =>
            {
                write!(f,
                       "root {} slot {} generation {} is no longer live or pending; no child \
                        operation was admitted",
                       root.kind(),
                       root.slot(),
                       root.generation())
            }
            Self::ForeignOwner { expected,
                                 actual } =>
            {
                write!(f,
                       "resource scope belongs to {actual}, not {expected}; foreign child \
                        admission rejected")
            }
            Self::WrongKind { expected,
                              actual } =>
            {
                write!(f,
                       "resource scope was reserved for '{expected}', not '{actual}'; publish \
                        through the matching root store")
            }
        }
    }
}
impl core::error::Error for Failure
{
}

pub(crate) struct State
{
    owner:      DomainId,
    kind:       KindCode,
    root:       Spinlock<Option<ResourceKey>>,
    open:       AtomicBool,
    allocation: Spinlock<()>
}
impl State
{
    /// Invoked only inside this owner's metadata gate. Close takes that same
    /// gate, so stopped-owner authorization cannot race root-scope closure.
    pub(crate) fn authorize(&self, index: &OwnerIndex) -> Result<ResourceKey, Failure>
    {
        if index.owner() != self.owner
        {
            return Err(Failure::ForeignOwner { expected: index.owner(),
                                               actual:   self.owner });
        }
        if !self.open.load(Ordering::Acquire)
        {
            return Err(Failure::Closed);
        }
        let root = (*self.root.lock()).ok_or(Failure::Unpublished)?;
        if !index.contains(root) && !index.pending(root)
        {
            return Err(Failure::RootGone { root });
        }
        Ok(root)
    }
}

/// An unpublished root and its unique binding reservation. Dropping it closes
/// the scope; a lease obtained during construction cannot admit any child.
/// Publication consumes the reservation through the matching native store.
pub struct Reservation<G: InvocationGate>
{
    owner:   OwnerRef<G>,
    kind:    KindCode,
    scope:   Owner<G>,
    binding: Binding
}
impl<G: InvocationGate> Reservation<G>
{
    pub fn try_new(owner: &OwnerRef<G>, kind: KindCode) -> Result<Self, PoolError>
    {
        if !owner.accepting()
        {
            return Err(PoolError::Stopping { owner: owner.owner() });
        }
        let state = Arc::try_new(State { owner: owner.owner(),
                                         kind,
                                         root: Spinlock::new(None),
                                         open: AtomicBool::new(true),
                                         allocation: Spinlock::new(()) }).map_err(|_| {
                        PoolError::Storage { owner: owner.owner(),
                                             kind,
                                             used: 0,
                                             requested_slots: 1,
                                             limit: None }
                    })?;
        Ok(Self { owner: owner.clone(),
                  kind,
                  scope: Owner { owner: owner.clone(),
                                 state: state.clone() },
                  binding: Binding { state } })
    }

    pub fn lease(&self) -> Lease<G>
    {
        self.scope.lease()
    }

    pub(crate) fn into_parts(self, kind: KindCode)
                             -> Result<(OwnerRef<G>, Owner<G>, Binding), PoolError>
    {
        if kind != self.kind
        {
            return Err(PoolError::Scope { owner: self.owner.owner(),
                                          kind,
                                          cause: Failure::WrongKind { expected: self.kind,
                                                                      actual:   kind } });
        }
        Ok((self.owner, self.scope, self.binding))
    }
}

/// Exactly one owner can close admission. Leases are read-only authority and
/// cannot reopen or publish a root, or be upgraded into a general OwnerRef.
pub(crate) struct Owner<G: InvocationGate>
{
    owner: OwnerRef<G>,
    state: Arc<State>
}
impl<G: InvocationGate> Owner<G>
{
    pub fn lease(&self) -> Lease<G>
    {
        Lease { owner: self.owner.clone(),
                state: self.state.clone() }
    }

    fn close(&self)
    {
        let closed = self.owner.access(|index| {
                                   // Allocator admission never acquires the owner index. Close
                                   // serializes with its bounded budget reservation here,
                                   // before Store can acknowledge this root's release ticket.
                                   let _allocation = self.state.allocation.lock();
                                   if self.state.open.swap(false, Ordering::AcqRel)
                                      && self.state.root.lock().is_some()
                                   {
                                       index.note_scope_change();
                                       CLOSED_REVISION.fetch_add(1, Ordering::Release);
                                       true
                                   }
                                   else
                                   {
                                       false
                                   }
                               });
        if closed
        {
            G::scope_closed(self.owner.owner());
        }
    }
}
impl<G: InvocationGate> Drop for Owner<G>
{
    fn drop(&mut self)
    {
        self.close();
    }
}

pub struct Lease<G: InvocationGate>
{
    owner: OwnerRef<G>,
    state: Arc<State>
}
impl<G: InvocationGate> Clone for Lease<G>
{
    fn clone(&self) -> Self
    {
        Self { owner: self.owner.clone(),
               state: self.state.clone() }
    }
}
impl<G: InvocationGate> Lease<G>
{
    pub fn kind(&self) -> KindCode
    {
        self.state.kind
    }

    pub fn subject(&self) -> DomainId
    {
        self.owner.owner()
    }

    /// Positive evidence that the unique root owner closed this scope. A stale
    /// metadata lookup or stopped subject alone must not manufacture closure.
    pub fn closed(&self) -> bool
    {
        !self.state.open.load(Ordering::Acquire)
    }

    pub fn subject_accepting(&self) -> bool
    {
        self.owner.accepting()
    }

    /// Identity metadata grants no publication or completion authority.
    pub fn root(&self) -> Option<ResourceKey>
    {
        G::run(|| *self.state.root.lock())
    }

    pub fn admitting(&self) -> Result<(), Failure>
    {
        self.owner.access(|index| self.state.authorize(index).map(|_| ()))
    }

    /// Derive allocator admission only from a real published live/pending root.
    /// Its separate close gate avoids recursively locking this owner index when
    /// allocator-backed pool metadata grows while the index is already held.
    pub fn allocation_admission(&self) -> Result<AllocationAdmission<G>, Failure>
    {
        self.owner.access(|index| {
                      self.state.authorize(index)?;
                      Ok(AllocationAdmission { owner: self.owner.clone(),
                                               state: self.state.clone() })
                  })
    }

    /// Distinguishes two operations of the same process without a numeric tag.
    pub fn same_scope(&self, other: &Self) -> bool
    {
        self.subject() == other.subject() && Arc::ptr_eq(&self.state, &other.state)
    }

    pub(crate) fn owner(&self) -> &OwnerRef<G>
    {
        &self.owner
    }

    pub(crate) fn state(&self) -> &State
    {
        &self.state
    }
}

/// Sealed admission for one published resource root. It retains the exact
/// canonical scope state and owner, never authority reconstructed from a key.
/// Keeping this capability cannot hold a root open: its unique close owner
/// flips the same guarded state before the root release can be acknowledged.
pub struct AllocationAdmission<G: InvocationGate>
{
    owner: OwnerRef<G>,
    state: Arc<State>
}
impl<G: InvocationGate> Clone for AllocationAdmission<G>
{
    fn clone(&self) -> Self
    {
        Self { owner: self.owner.clone(),
               state: self.state.clone() }
    }
}
impl<G: InvocationGate> AllocationAdmission<G>
{
    pub fn subject(&self) -> DomainId
    {
        self.state.owner
    }

    pub fn same_scope(&self, scope: &Lease<G>) -> bool
    {
        self.subject() == scope.subject() && Arc::ptr_eq(&self.state, &scope.state)
    }

    /// Run only bounded, allocation-free budget admission. The callback must
    /// not allocate backing, drop refund receipts, acquire an owner ledger or
    /// invoke external code. Every such action follows release of this guard.
    /// Root closure and this check share one gate; a naked closed() observation
    /// is never sufficient authority to admit a reservation.
    pub fn with_admission<R>(&self, operation: impl FnOnce() -> R) -> Result<R, Failure>
    {
        G::run(|| {
            let _allocation = self.state.allocation.lock();
            if !self.state.open.load(Ordering::Acquire)
            {
                return Err(Failure::Closed);
            }
            Ok(operation())
        })
    }
}

/// Only Store's successful-publication arm receives this unique binding.
/// Its owner reference and payload factory come from the same reservation;
/// callers cannot assemble a binding for a different process or root.
pub(crate) struct Binding
{
    state: Arc<State>
}
impl Binding
{
    pub(crate) fn publish(self, key: ResourceKey)
    {
        *self.state.root.lock() = Some(key);
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    struct Gate;
    impl InvocationGate for Gate
    {
        fn run<R>(f: impl FnOnce() -> R) -> R
        {
            f()
        }
    }
    #[test]
    fn scope_close_notification_runs_once_after_the_metadata_gate_is_released()
    {
        use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        static INSIDE: AtomicBool = AtomicBool::new(false);
        static CLOSED: AtomicUsize = AtomicUsize::new(0);
        struct Notify;
        impl InvocationGate for Notify
        {
            fn run<R>(operation: impl FnOnce() -> R) -> R
            {
                let outer = INSIDE.swap(true, Ordering::SeqCst);
                let result = operation();
                INSIDE.store(outer, Ordering::SeqCst);
                result
            }

            fn scope_closed(_: DomainId)
            {
                assert!(!INSIDE.load(Ordering::SeqCst), "scope callback ran under metadata gate");
                CLOSED.fetch_add(1, Ordering::SeqCst);
            }
        }
        let owner = OwnerRef::<Notify>::try_new(OwnerIndex::issue(crate::domain::DomainKind::Module).unwrap()).unwrap();
        let kind = KindCode(*b"NCLO");
        let unpublished = Reservation::try_new(&owner, kind).unwrap();
        drop(unpublished);
        assert_eq!(CLOSED.load(Ordering::SeqCst), 0);
        let reservation = Reservation::try_new(&owner, kind).unwrap();
        let lease = reservation.lease();
        let mut roots =
            crate::store::Store::new(kind, crate::domain::CapacityPolicy::GrowableUntilMemory);
        let id = roots.insert_root(reservation, ()).unwrap();
        assert!(!lease.closed());
        drop(roots.remove(id).unwrap());
        assert!(lease.closed());
        assert_eq!(CLOSED.load(Ordering::SeqCst), 1);
        drop(lease);
        drop(roots);
        assert_eq!(CLOSED.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_foreign_index_cannot_authorize_a_genuine_scope_even_with_matching_slot_numbers()
    {
        let owner=OwnerRef::<Gate>::try_new(OwnerIndex::issue(crate::domain::DomainKind::Process).unwrap()).unwrap();
        let other = OwnerIndex::issue(crate::domain::DomainKind::Process).unwrap();
        let kind = KindCode(*b"ROOT");
        let reservation = Reservation::try_new(&owner, kind).unwrap();
        let lease = reservation.lease();
        let mut roots =
            crate::store::Store::new(kind, crate::domain::CapacityPolicy::GrowableUntilMemory);
        roots.insert_root(reservation, 1).unwrap();
        assert_eq!(lease.state.authorize(&other),
                   Err(Failure::ForeignOwner { expected: other.owner(),
                                               actual:   owner.owner() }));
        let text = alloc::format!("{}", lease.state.authorize(&other).unwrap_err());
        assert!(text.contains("foreign child admission rejected"));
        assert_eq!(other.summary().live, 0);
    }
}
