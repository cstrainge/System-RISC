//! Registered kind policy and cleanup dispatch. The host supplies concrete
//! adapters; this facility knows no manager semantics and calls no adapter
//! while holding owner metadata or registry locks.
use alloc::{sync::Arc, vec::Vec};
use core::{fmt,
           sync::atomic::{AtomicU64, Ordering}};

use crate::{domain::{CapacityPolicy, CompletionError, DomainId, KindCode, KindRegistration,
                     PoolError, ReleaseDispatch, ReleasedResource, ResourceKey},
            shared::{InvocationGate, OwnerRef},
            store::Store};

/// The single bootstrap namespace. Its values are registrations themselves;
/// creating this root cannot recursively require an already registered kind.
pub const REGISTRATION_KIND: KindCode = KindCode(*b"KREG");
const CLOSED: u64 = 1 << 63;
const COUNT: u64 = CLOSED - 1;
pub const LABEL_BYTES: usize = mac_abi::INLINE_STRING_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Label
{
    bytes:  [u8; LABEL_BYTES],
    length: usize
}
impl Label
{
    fn checked(text: &str, field: &'static str) -> Result<Self, Error>
    {
        if text.trim().is_empty() || text.len() > LABEL_BYTES || text.chars().any(char::is_control)
        {
            return Err(Error::Label { field,
                                      limit: LABEL_BYTES });
        }
        let mut value = Self { bytes:  [0; LABEL_BYTES],
                               length: text.len() };
        value.bytes[..text.len()].copy_from_slice(text.as_bytes());
        Ok(value)
    }

    pub fn as_str(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.length]).expect("validated registration label")
    }
}
impl fmt::Display for Label
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.write_str(self.as_str())
    }
}

/// Resource claims retain stable bundle/resource evidence as well as the
/// registering domain incarnation. No source identity is inferred from a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source
{
    Resident,
    BundleResource
    {
        bundle: [u8; 16],
        kind:   KindCode,
        id:     i16
    }
}
pub struct Specification<'a>
{
    pub kind:     KindCode,
    pub label:    &'a str,
    pub provider: &'a str,
    pub capacity: CapacityPolicy,
    pub source:   Source
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    RegistrationNotFound
    {
        id: u64
    },
    Label
    {
        field: &'static str,
        limit: usize
    },
    Duplicate
    {
        kind:     KindCode,
        provider: Label
    },
    Missing
    {
        kind: KindCode
    },
    Foreign
    {
        kind:     KindCode,
        expected: DomainId,
        actual:   DomainId
    },
    Stale
    {
        registration: ResourceKey
    },
    Busy
    {
        kind:      KindCode,
        provider:  Label,
        remaining: u64
    },
    SnapshotStale
    {
        expected: u64,
        actual:   u64
    },
    RevisionExhausted,
    IndexNotFound
    {
        index: usize
    },
    Pool(PoolError)
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::RegistrationNotFound { id } =>
            {
                write!(f,
                       "registration identity {id} is not live; resolve the current provider \
                        before retrying")
            }
            Self::Label { field,
                          limit } =>
            {
                write!(f,
                       "registration {field} must be nonempty UTF-8, contain no control \
                        characters and fit {limit} bytes")
            }
            Self::Duplicate { kind,
                              provider } =>
            {
                write!(f,
                       "kind '{kind}' is already registered by {provider}; remove that \
                        registration before replacing it")
            }
            Self::Missing { kind } => write!(f, "kind '{kind}' has no registered cleanup provider"),
            Self::Foreign { kind,
                            expected,
                            actual } =>
            {
                write!(f, "cannot unregister '{kind}' owned by {expected} as {actual}")
            }
            Self::Stale { registration } =>
            {
                write!(f,
                       "registration in {} is stale; resolve its current provider before retrying",
                       registration.owner())
            }
            Self::Busy { kind,
                         provider,
                         remaining } =>
            {
                write!(f,
                       "{provider} is stopping kind '{kind}'; {remaining} resources or \
                        invocations still require cleanup; finish cleanup and retry")
            }
            Self::SnapshotStale { expected,
                                  actual } =>
            {
                write!(f,
                       "kind registrations changed from revision {expected} to {actual}; obtain a \
                        new registry snapshot and retry")
            }
            Self::RevisionExhausted =>
            {
                f.write_str("kind registry revision exhausted; restart System to obtain new \
                             snapshots")
            }
            Self::IndexNotFound { index } =>
            {
                write!(f, "kind index {index} is outside this registry snapshot")
            }
            Self::Pool(error) => error.fmt(f)
        }
    }
}
impl core::error::Error for Error
{
}
impl From<PoolError> for Error
{
    fn from(error: PoolError) -> Self
    {
        Self::Pool(error)
    }
}

struct Metadata
{
    kind:     KindCode,
    label:    Label,
    provider: Label,
    capacity: CapacityPolicy,
    source:   Source,
    // Admission and use count change in one CAS: closing cannot race an
    // insertion into admitting a resource after a zero-use unregister check.
    uses:     AtomicU64
}
pub(crate) struct Use
{
    metadata: Arc<Metadata>
}
impl Drop for Use
{
    fn drop(&mut self)
    {
        self.metadata.uses.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Validated registration capability. Its fields are private; observing a key
/// does not let a caller fabricate this binding or substitute another policy.
pub struct KindLease<G: InvocationGate>
{
    id:       u64,
    key:      ResourceKey,
    owner:    OwnerRef<G>,
    metadata: Arc<Metadata>
}
impl<G: InvocationGate> Clone for KindLease<G>
{
    fn clone(&self) -> Self
    {
        Self { id:       self.id,
               key:      self.key,
               owner:    self.owner.clone(),
               metadata: self.metadata.clone() }
    }
}
impl<G: InvocationGate> KindLease<G>
{
    pub fn id(&self) -> u64
    {
        self.id
    }

    pub fn registration(&self) -> KindRegistration
    {
        KindRegistration { code:     self.metadata.kind,
                           capacity: self.metadata.capacity,
                           release:  ReleaseDispatch { registration: self.key } }
    }

    pub(crate) fn live(&self) -> bool
    {
        self.owner.contains(self.key)
    }

    pub(crate) fn acquire(&self, admitting: bool) -> Result<Use, PoolError>
    {
        let unavailable = PoolError::RegistrationUnavailable { kind:         self.metadata.kind,
                                                               registration: self.key };
        if !self.live() || admitting && !self.owner.accepting()
        {
            return Err(unavailable);
        }
        self.metadata
            .uses
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                if admitting && state & CLOSED != 0 || state & COUNT == COUNT
                {
                    None
                }
                else
                {
                    Some(state + 1)
                }
            })
            .map_err(|state| {
                if state & COUNT == COUNT
                {
                    PoolError::RegistrationUsesExhausted { kind:         self.metadata.kind,
                                                           registration: self.key }
                }
                else
                {
                    unavailable
                }
            })?;
        // A registrar can be invalidated independently by its owner walk.
        // The lease's native capability still cannot revive a dead generation.
        let usage = Use { metadata: self.metadata.clone() };
        if !self.live()
        {
            return Err(unavailable);
        }
        Ok(usage)
    }
}

/// Concrete resident adapters implement this at cleanup granularity. A loaded
/// module adapter must own its code/resource pin; no raw vtable crosses an ABI.
pub trait CleanupFailure
{
    type Cause;
    fn into_parts(self) -> (ReleasedResource, Self::Cause);
    fn registration_mismatch(resource: ReleasedResource) -> Self;
}
impl CleanupFailure for CompletionError
{
    type Cause = PoolError;

    fn into_parts(self) -> (ReleasedResource, PoolError)
    {
        let cause = self.cause();
        (self.into_ticket(), cause)
    }

    fn registration_mismatch(resource: ReleasedResource) -> Self
    {
        CompletionError::stale(resource)
    }
}

/// The host chooses its typed failure carrier. File I/O, driver or other
/// manager failures retain their identity; the facility never guesses meaning
/// from a message or squeezes every cleanup failure into a slot error.
pub trait Release<Context, F: CleanupFailure = CompletionError>: Send + Sync
{
    fn release(&self, context: &mut Context, resource: ReleasedResource) -> Result<(), F>;
}
struct Record<Context, G: InvocationGate, F: CleanupFailure>
{
    owner:    OwnerRef<G>,
    metadata: Arc<Metadata>,
    release:  Arc<dyn Release<Context, F>>
}
#[derive(Clone, Copy, Debug)]
pub struct Information
{
    pub id:           u64,
    pub registration: KindRegistration,
    pub label:        Label,
    pub provider:     Label,
    pub source:       Source,
    pub admitting:    bool,
    pub uses:         u64
}
pub struct Registry<Context, G: InvocationGate, F: CleanupFailure = CompletionError>
{
    records:  Store<Record<Context, G, F>, G>,
    kinds:    Vec<(KindCode, u64)>,
    revision: u64
}
impl<Context: 'static, G: InvocationGate, F: CleanupFailure + 'static> Registry<Context, G, F>
{
    pub fn new() -> Self
    {
        Self { records:  Store::new(REGISTRATION_KIND, CapacityPolicy::GrowableUntilMemory),
               kinds:    Vec::new(),
               revision: 1 }
    }

    fn position(&self, kind: KindCode) -> Result<usize, usize>
    {
        self.kinds.binary_search_by_key(&kind.0, |(code, _)| code.0)
    }

    pub fn register<R: Release<Context, F> + 'static>(&mut self, owner: &OwnerRef<G>,
                                                      spec: Specification<'_>, release: R)
                                                      -> Result<KindLease<G>, Error>
    {
        let label = Label::checked(spec.label, "label")?;
        let provider = Label::checked(spec.provider, "provider name")?;
        let at = match self.position(spec.kind)
        {
            Ok(at) =>
            {
                return Err(Error::Duplicate { kind:     spec.kind,
                                              provider:
                                                  self.records
                                                      .get(self.kinds[at].1)
                                                      .ok_or(Error::Missing { kind: spec.kind })?
                                                      .metadata
                                                      .provider });
            }
            Err(at) => at
        };
        if let CapacityPolicy::HardBound { reason,
                                           .. } = spec.capacity
        {
            if reason.trim().is_empty()
            {
                return Err(PoolError::InvalidBoundReason.into());
            }
            if reason.len() > mac_abi::inspection::DETAIL_BYTES
               || reason.chars().any(char::is_control)
            {
                return Err(Error::Label { field: "capacity reason",
                                          limit: mac_abi::inspection::DETAIL_BYTES });
            }
        }
        let allocation = PoolError::Storage { owner:           owner.owner(),
                                              kind:            REGISTRATION_KIND,
                                              used:            self.kinds.len(),
                                              requested_slots: self.kinds.len().saturating_add(1),
                                              limit:           None };
        self.kinds.try_reserve(1).map_err(|_| allocation)?;
        let metadata = Arc::try_new(Metadata { kind: spec.kind,
                                               label,
                                               provider,
                                               capacity: spec.capacity,
                                               source: spec.source,
                                               uses: AtomicU64::new(0) }).map_err(|_| allocation)?;
        let release: Arc<dyn Release<Context, F>> = Arc::try_new(release).map_err(|_| allocation)?;
        let id = self.records.insert(owner,
                                      Record { owner: owner.clone(),
                                               metadata: metadata.clone(),
                                               release })?;
        let key = self.records.key(id).ok_or(Error::Missing { kind: spec.kind })?;
        self.kinds.insert(at, (spec.kind, id));
        self.revision = self.revision.saturating_add(1);
        Ok(KindLease { id,
                       key,
                       owner: owner.clone(),
                       metadata })
    }

    pub fn get(&self, kind: KindCode) -> Result<KindLease<G>, Error>
    {
        let at = self.position(kind).map_err(|_| Error::Missing { kind })?;
        let id = self.kinds[at].1;
        let record = self.records.get(id).ok_or(Error::Missing { kind })?;
        let key = self.records.key(id).ok_or(Error::Missing { kind })?;
        Ok(KindLease { id,
                       key,
                       owner: record.owner.clone(),
                       metadata: record.metadata.clone() })
    }

    pub fn snapshot(&self) -> Result<(u64, usize), Error>
    {
        if self.revision == u64::MAX
        {
            return Err(Error::RevisionExhausted);
        }
        Ok((self.revision, self.kinds.len()))
    }

    pub fn at(&self, revision: u64, index: usize) -> Result<Information, Error>
    {
        if self.revision == u64::MAX
        {
            return Err(Error::RevisionExhausted);
        }
        if revision != self.revision
        {
            return Err(Error::SnapshotStale { expected: revision,
                                              actual:   self.revision });
        }
        let &(_, id) = self.kinds.get(index).ok_or(Error::IndexNotFound { index })?;
        self.by_id(revision, id)
    }

    pub fn by_id(&self, revision: u64, id: u64) -> Result<Information, Error>
    {
        if self.revision == u64::MAX
        {
            return Err(Error::RevisionExhausted);
        }
        if revision != self.revision
        {
            return Err(Error::SnapshotStale { expected: revision,
                                              actual:   self.revision });
        }
        let record = self.records.get(id).ok_or(Error::RegistrationNotFound { id })?;
        let key = self.records.key(id).ok_or(Error::RegistrationNotFound { id })?;
        let state = record.metadata.uses.load(Ordering::Acquire);
        Ok(Information { id,
                         registration: KindRegistration { code:     record.metadata.kind,
                                                          capacity: record.metadata.capacity,
                                                          release:
                                                              ReleaseDispatch { registration: key } },
                         label: record.metadata.label,
                         provider: record.metadata.provider,
                         source: record.metadata.source,
                         admitting: state & CLOSED == 0 && record.owner.accepting(),
                         uses: state & COUNT })
    }

    /// Stop admission first. A busy registration remains owned/enumerable and
    /// callable for cleanup; its key/code backing is not freed prematurely.
    pub fn unregister(&mut self, owner: &OwnerRef<G>, kind: KindCode)
                      -> Result<Unregistered<Context, G, F>, Error>
    {
        let lease = self.get(kind)?;
        if lease.key.owner() != owner.owner()
        {
            return Err(Error::Foreign { kind,
                                        expected: lease.key.owner(),
                                        actual: owner.owner() });
        }
        let state = lease.metadata.uses.fetch_or(CLOSED, Ordering::AcqRel);
        if state & COUNT != 0
        {
            return Err(Error::Busy { kind,
                                     provider: lease.metadata.provider,
                                     remaining: state & COUNT });
        }
        let at = self.position(kind).map_err(|_| Error::Missing { kind })?;
        let id = self.kinds[at].1;
        let record = self.records.remove(id).ok_or(Error::Stale { registration: lease.key })?;
        self.kinds.remove(at);
        self.revision = self.revision.saturating_add(1);
        Ok(Unregistered { _record: record })
    }

    /// Clone/pin under the host registry boundary, then invoke after releasing
    /// that boundary. No callback is run by this method.
    pub fn dispatch(&self, kind: KindCode) -> Result<Invocation<Context, G, F>, Error>
    {
        let lease = self.get(kind)?;
        let usage = lease.acquire(false)?;
        let at = self.position(kind).map_err(|_| Error::Missing { kind })?;
        let record =
            self.records.get(self.kinds[at].1).ok_or(Error::Stale { registration: lease.key })?;
        Ok(Invocation { lease,
                        _usage: usage,
                        release: record.release.clone() })
    }
}

/// Revocation is already published. Retains the provider/code pin and root
/// pending ticket until this guard drops outside the host registry boundary.
#[must_use = "drop the registration cleanup guard after releasing the host registry lock"]
pub struct Unregistered<Context, G: InvocationGate, F: CleanupFailure = CompletionError>
{
    _record: crate::store::Removed<Record<Context, G, F>, G>
}
impl<C, G: InvocationGate, F: CleanupFailure> fmt::Debug for Unregistered<C, G, F>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.write_str("Unregistered(pending provider release)")
    }
}
impl<Context: 'static, G: InvocationGate, F: CleanupFailure + 'static> Default
    for Registry<Context, G, F>
{
    fn default() -> Self
    {
        Self::new()
    }
}

impl<Context, G: InvocationGate, F: CleanupFailure> Drop for Registry<Context, G, F>
{
    fn drop(&mut self)
    {
        for (_, id) in self.kinds.drain(..)
        {
            if let Some(record) = self.records.remove(id)
            {
                let state = record.metadata.uses.fetch_or(CLOSED, Ordering::AcqRel);
                if state & COUNT != 0
                {
                    // Abandoning a registry is not proof that its dependents
                    // were cleaned up. Keep its provider/code pin and root
                    // cleanup ticket quarantined: the authoritative pending
                    // barrier prevents domain retirement. Normal teardown uses
                    // unregister/retry while retaining this registry.
                    core::mem::forget(record);
                }
            }
        }
    }
}

pub struct Invocation<Context, G: InvocationGate, F: CleanupFailure = CompletionError>
{
    lease:   KindLease<G>,
    _usage:  Use,
    release: Arc<dyn Release<Context, F>>
}
impl<Context, G: InvocationGate, F: CleanupFailure> Invocation<Context, G, F>
{
    pub fn invoke(self, context: &mut Context, resource: ReleasedResource) -> Result<(), F>
    {
        if resource.key().kind() != self.lease.metadata.kind
           || resource.key().registration() != Some(self.lease.id)
        {
            return Err(F::registration_mismatch(resource));
        }
        self.release.release(context, resource)
    }
}
