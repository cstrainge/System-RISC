//! Canonical definition records own source, placement and physical publication.
//! No decode, allocation, monitor operation or author call runs under metadata guards.
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use hashbrown::HashMap;
use mac_abi::{ACCESS_DENIED, BAD_ARGUMENT, DefinitionCall, NO_MEMORY, NOT_FOUND, SERVICE_BUSY,
              Status};
use mac_kernel_model::image::{ExecutionGate, ExecutionPin, Loading};
use mac_object_table::{domain::{DomainId, ReleasedResource},
                       registry::{Release, Source, Specification},
                       shared::InvocationGate as _,
                       store::{CompletionKey, PreparedError, PublicationPermit}};
use spinning_top::Spinlock;

use crate::{machine::paging::modules::{DefinitionTarget, Mapping},
            toolbox::handles::{CapacityPolicy, InvocationGate, KindCode, OwnerRef, Store,
                               cleanup::Failure}};

#[path = "callbacks.rs"]
pub(crate) mod callbacks;

#[derive(Debug)]
pub(crate) enum Error
{
    Facility(mac_object_table::domain::PoolError),
    Preparation(PreparedError),
    OwnerUnavailable(Status),
    Allocation,
    Placement,
    Source,
    Stopping,
    Busy,
    Missing,
    Mapping(crate::machine::paging::modules::Error),
    Retained(crate::machine::paging::modules::CleanupCause),
    Execution(mac_kernel_model::image::Error)
}
impl Error
{
    pub(crate) fn status(&self) -> Status
    {
        match self
        {
            Self::Facility(cause) | Self::Preparation(PreparedError::Pool(cause)) =>
            {
                crate::toolbox::handles::registry_status(&mac_object_table::registry::Error::Pool(*cause))
            }
            Self::Preparation(PreparedError::CapacityChanged) | Self::Busy | Self::Stopping => SERVICE_BUSY,
            Self::OwnerUnavailable(status) => *status,
            Self::Allocation | Self::Placement => NO_MEMORY,
            Self::Source => ACCESS_DENIED,
            Self::Missing => NOT_FOUND,
            Self::Mapping(cause) => cause.status(),
            Self::Retained(_) => mac_abi::NOT_SUPPORTED,
            Self::Execution(mac_kernel_model::image::Error::Allocation | mac_kernel_model::image::Error::PinLimit) => NO_MEMORY,
            Self::Execution(mac_kernel_model::image::Error::Busy) => SERVICE_BUSY,
            Self::Preparation(_) => mac_abi::BAD_ABI
        }
    }
}

impl core::fmt::Display for Error
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Facility(e) => write!(f, "definition facility refused: {e:?}"),
            Self::Preparation(e) => write!(f, "definition publication refused: {e}"),
            Self::OwnerUnavailable(s) => write!(f, "definition System authority unavailable: {s}"),
            Self::Allocation => f.write_str("definition metadata capacity refused"),
            Self::Placement => f.write_str("definition executable placement exhausted"),
            Self::Source => f.write_str("definition source/owner/account binding mismatch"),
            Self::Stopping => f.write_str("definition source stopped before publication"),
            Self::Busy => f.write_str("definition preparation is already active"),
            Self::Missing => f.write_str("definition manager is unavailable"),
            Self::Mapping(e) => e.fmt(f),
            Self::Retained(e) => write!(f, "definition physical publication retained: {e}"),
            Self::Execution(e) => e.fmt(f)
        }
    }
}
/// Private original key. Only Ready publication returns one to a manager.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DefinitionRef(CompletionKey);
impl DefinitionRef
{
    pub(crate) fn same(self, other: Self) -> bool
    {
        self.0 == other.0
    }
}

pub(crate) fn is_user(reference: DefinitionRef) -> Result<bool, Status>
{
    state(|state| {
        let definition = state.store.completing(&reference.0).ok_or(NOT_FOUND)?;
        if definition.owner.owner() != reference.0.owner()
        {
            return Err(ACCESS_DENIED);
        }
        Ok(definition.owner.owner().kind() == mac_object_table::domain::DomainKind::Process)
    }).map_err(|cause| cause.status())?
}

/// One original definition dependency owned by a canonical bound object. This
/// is not a new resource identity and cannot be copied into a temporary view.
/// Arc destruction precedes the actual per-change retirement checkpoint.
pub(crate) struct BoundDefinition
{
    _definition: Arc<Definition>,
    reference:   DefinitionRef,
    _released:   BindingReleased
}
impl BoundDefinition
{
    /// Retain only the already admitted original source. This grants no new
    /// callback admission, publication or storage allocation authority.
    pub(crate) fn retain_source(&self) -> RetainedDefinition
    {
        RetainedDefinition { definition: self._definition.clone(),
                             reference:  self.reference,
                             _released:  BindingReleased }
    }

    /// Move a genuine newly admitted binding into a retained source without a
    /// temporary reader-return notification or a second binding operation.
    pub(crate) fn into_retained_source(self) -> RetainedDefinition
    {
        let Self { _definition,
                   reference,
                   _released } = self;
        RetainedDefinition { definition: _definition,
                             reference,
                             _released }
    }

    /// Retain this original dependency for guard-free owner-death admission.
    /// This does not mint a new binding or bypass stopped-owner publication.
    pub(crate) fn quiescence_plan(&self) -> QuiescencePlan
    {
        QuiescencePlan { definition: self._definition.clone(),
                         reference:  self.reference,
                         _released:  BindingReleased }
    }

    pub(crate) fn reference(&self) -> DefinitionRef
    {
        self.reference
    }

    /// Original bound source classification; this never consults the current
    /// caller/resource selection or recreates a binding from a numeric owner.
    /// The authenticated callback execution is the canonical single callback
    /// role of its actual Process/image. No owner number supplied by user code
    /// can create this proof or select a different queue.
    pub(crate) fn callback_waits_on(&self,
                                    execution:&crate::machine::scheduler::callbacks::CallbackExecution)
                                    -> bool
    {
        match &self._definition.source
        {
            SourceBinding::Image(source) =>
            {
                source.owner().owner() == execution.owner().owner()
                && source.binding().id() == execution.image_id()
            }
            _ => false
        }
    }

    pub(crate) fn resource_id(&self) -> i16
    {
        self._definition.identity.id
    }

    pub(crate) fn is_process_source(&self) -> bool
    {
        self._definition.owner.owner().kind() == mac_object_table::domain::DomainKind::Process
    }

    pub(crate) fn matches(&self, reference: DefinitionRef) -> bool
    {
        self.reference.0 == reference.0
    }
}
/// An immutable reader of one genuine admitted definition. Retaining it after
/// stop does not permit another invocation; canonical author admission remains
/// responsible for that check. Field order drops the Arc before its checkpoint.
pub(crate) struct RetainedDefinition
{
    definition: Arc<Definition>,
    reference:  DefinitionRef,
    _released:  BindingReleased
}
impl Clone for RetainedDefinition
{
    fn clone(&self) -> Self
    {
        Self { definition: self.definition.clone(),
               reference:  self.reference,
               _released:  BindingReleased }
    }
}
impl RetainedDefinition
{
    pub(crate) fn reference(&self) -> DefinitionRef
    {
        self.reference
    }

    pub(crate) fn matches(&self, binding: &BoundDefinition) -> bool
    {
        self.reference.same(binding.reference)
        && Arc::ptr_eq(&self.definition, &binding._definition)
    }

    pub(crate) fn is_process_source(&self) -> bool
    {
        self.definition.owner.owner().kind() == mac_object_table::domain::DomainKind::Process
    }
}
pub(crate) struct QuiescencePlan
{
    definition: Arc<Definition>,
    reference:  DefinitionRef,
    _released:  BindingReleased
}
impl QuiescencePlan
{
    /// Compare the retained original dependency without creating another bind.
    pub(crate) fn matches(&self, binding: &BoundDefinition) -> bool
    {
        self.reference == binding.reference && Arc::ptr_eq(&self.definition, &binding._definition)
    }
}
/// Abandoned user author state may be torn down by the kernel only while this
/// original source/exclusion remains held. This is not a DISPOSE callback ACK.
/// Carry it out of Desktop before Drop and before the canonical DDEF walk.
pub(crate) struct QuiescedDefinition
{
    _loading:   Loading,
    definition: Arc<Definition>,
    reference:  DefinitionRef,
    _released:  BindingReleased
}
impl QuiescencePlan
{
    /// Must run outside Desktop/metadata gates. Refusal returns the SAME plan.
    pub(crate) fn acquire(self) -> Result<QuiescedDefinition, (Status, Self)>
    {
        let prepared = (|| {
            let SourceBinding::Image(source) = &self.definition.source
            else
            {
                return Err(ACCESS_DENIED);
            };
            if source.owner().owner().kind() != mac_object_table::domain::DomainKind::Process
            {
                return Err(ACCESS_DENIED);
            }
            if self.definition.owner.accepting()
            {
                return Err(SERVICE_BUSY);
            }
            let quiescence = crate::machine::scheduler::callbacks::source_quiescence(source)?;
            if quiescence.owner().owner() != self.definition.owner.owner()
               || quiescence.image() != source.binding()
            {
                return Err(ACCESS_DENIED);
            }
            // Register before testing exclusion, so a concurrent last reader or
            // other proof release cannot pass between refusal and the wake flag.
            WAITING_READERS.store(true, Ordering::Release);
            let loading = self.definition.execution.begin_load().map_err(|_| SERVICE_BUSY)?;
            // Owner closure and no canonical rows are monotonic; Loading now
            // excludes admitted calls. The original Definition Arc keeps Read.
            drop(quiescence);
            Ok(loading)
        })();
        match prepared
        {
            Ok(loading) =>
            {
                let Self { definition,
                           reference,
                           _released } = self;
                Ok(QuiescedDefinition { _loading: loading,
                                        definition,
                                        reference,
                                        _released })
            }
            Err(status) => Err((status, self))
        }
    }
}
impl QuiescedDefinition
{
    pub(crate) fn matches(&self, binding: &BoundDefinition) -> bool
    {
        self.reference.same(binding.reference)
        && Arc::ptr_eq(&self.definition, &binding._definition)
    }
}
struct BindingReleased;
impl Drop for BindingReleased
{
    fn drop(&mut self)
    {
        // Field order has already released the actual source Arc. Wake only
        // real cleanup/claim waiters, never ordinary UI create/dispose traffic.
        let readers = WAITING_READERS.swap(false, Ordering::AcqRel);
        let claims = WAITING_CLAIM_READERS.swap(false, Ordering::AcqRel);
        if readers || claims
        {
            crate::kernel::window_sources_changed();
            crate::machine::paging::modules::changed();
            crate::toolbox::processes::retirement::resources_released();
        }
    }
}
/// Only actual Ready canonical storage can mint a new dependency. The source
/// admission observation and pin publication share State with closing: a bind
/// already admitted before stop may settle, but cleanup sees its actual Arc.
pub(crate) fn bind(reference: DefinitionRef) -> Result<BoundDefinition, Status>
{
    state(|state| {
        let definition = state.store.completing(&reference.0).ok_or(NOT_FOUND)?;
        // Completing lookup is required for existing Dispose, but a new bind
        // must still have a live canonical DDEF entry, never only a pending key.
        if !state.store.get(reference.0.id()).is_some_and(|live| Arc::ptr_eq(live, definition))
        {
            return Err(NOT_FOUND);
        }
        if definition.owner.owner() != reference.0.owner()
        {
            return Err(ACCESS_DENIED);
        }
        let cell = definition.state.lock();
        if !definition.owner.accepting() || cell.closing || cell.entry.is_none()
        {
            return Err(SERVICE_BUSY);
        }
        Ok(BoundDefinition { _definition: definition.clone(),
                             reference,
                             _released: BindingReleased })
    }).map_err(|cause| cause.status())?
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Identity
{
    kind:   [u8; 4],
    id:     i16,
    origin: u64
}
pub(super) enum SourceBinding
{
    System(crate::kernel::SystemResources),
    Image(crate::images::Read)
}
impl SourceBinding
{
    pub(super) fn owner(&self) -> &OwnerRef
    {
        match self
        {
            Self::System(s) => s.owner(),
            Self::Image(s) => s.owner()
        }
    }

    fn origin(&self) -> u64
    {
        match self
        {
            Self::System(_) => 0,
            Self::Image(s) => s.binding().id()
        }
    }

    pub(super) fn allocation_target(&self) -> crate::allocator::Target
    {
        match self
        {
            Self::System(_) =>
            {
                crate::allocator::Target::System(crate::allocator::SystemClass::Ordinary)
            }
            Self::Image(s) =>
            {
                crate::allocator::Target::Owner { account: s.allocation().clone(),
                                                  class:
                                                      mac_kernel_model::policy::MemoryClass::Kernel }
            }
        }
    }

    fn mapping_target(&self) -> DefinitionTarget<'_>
    {
        match self
        {
            Self::System(s) => DefinitionTarget::System(*s),
            Self::Image(s)
                if s.owner().owner().kind() == mac_object_table::domain::DomainKind::Process =>
            {
                DefinitionTarget::Process(s)
            }
            Self::Image(s) => DefinitionTarget::Owner(s.allocation().clone())
        }
    }

    fn valid(&self) -> bool
    {
        match self
        {
            Self::System(s) =>
            {
                s.owner().owner().kind() == mac_object_table::domain::DomainKind::System
            }
            Self::Image(s) =>
            {
                matches!(s.owner().owner().kind(),
                         mac_object_table::domain::DomainKind::Process
                         | mac_object_table::domain::DomainKind::Module)
                && s.owner().owner() == s.allocation().owner()
            }
        }
    }
}
struct Cell
{
    busy:    bool,
    closing: bool,
    entry:   Option<Entry>,
    mapping: Option<Arc<Mapping>>
}
#[derive(Clone, Copy)]
enum Entry
{
    Trusted(mac_abi::DefinitionEntry),
    User(u64)
}
struct Definition
{
    identity:  Identity,
    owner:     OwnerRef,
    base:      AtomicU64,
    limit:     AtomicU64,
    state:     Spinlock<Cell>,
    execution: ExecutionGate,
    retiring:  AtomicBool,
    source:    SourceBinding
}
struct State
{
    store:   Store<Arc<Definition>>,
    index:   HashMap<Identity, CompletionKey>,
    credits: usize
}
static DEFINITIONS: spin::Once<Spinlock<State>> = spin::Once::new();
static WAITING_PREPARATIONS: AtomicBool = AtomicBool::new(false);
static WAITING_READERS: AtomicBool = AtomicBool::new(false);
static WAITING_MONITOR: AtomicBool = AtomicBool::new(false);
static WAITING_CLAIM_READERS: AtomicBool = AtomicBool::new(false);
fn state<R>(op: impl FnOnce(&mut State) -> R) -> Result<R, Error>
{
    let state = DEFINITIONS.get().ok_or(Error::Missing)?;
    Ok(InvocationGate::run(|| op(&mut state.lock())))
}
fn state_owned<A, R>(argument: A, op: impl FnOnce(A, &mut State) -> R) -> Result<R, (Error, A)>
{
    let Some(state) = DEFINITIONS.get()
    else
    {
        return Err((Error::Missing, argument));
    };
    Ok(InvocationGate::run(|| op(argument, &mut state.lock())))
}
pub(super) fn initialize() -> Result<(), Error>
{
    if !crate::context::current_thread_context().is_bootstrap() || DEFINITIONS.get().is_some()
    {
        return Err(Error::Source);
    }
    let (objects, storage) =
        crate::toolbox::handles::definition_kinds().map_err(Error::OwnerUnavailable)?;
    let prepared = crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                       Store::owned_prepared(objects, storage)
                   }).map_err(Error::Facility)?;
    DEFINITIONS.call_once(|| {
                   Spinlock::new(State { store:   prepared,
                                         index:   HashMap::new(),
                                         credits: 0 })
               });
    Ok(())
}
struct IndexCredit
{
    active: bool
}
fn credit_invariant_lost(reason: &str) -> !
{
    // A live credit's reset-lifetime registry cannot disappear, and each
    // reservation has exactly one refund. A violated counter is structural
    // System corruption, not an ordinary allocation or teardown refusal.
    crate::machine::emergency::serial(format_args!("SYSTEM_DEFINITION_INVARIANT_LOST: {reason}\n"));
    crate::machine::power::fatal_shutdown()
}
impl Drop for IndexCredit
{
    fn drop(&mut self)
    {
        if self.active
        {
            match state(|s| {
                      if let Some(next) = s.credits.checked_sub(1)
                      {
                          s.credits = next
                      }
                      else
                      {
                          credit_invariant_lost("definition index credit underflow")
                      }
                  })
            {
                Ok(()) => (),
                Err(_) => credit_invariant_lost("definition index incarnation disappeared")
            }
        }
    }
}

fn index_credit() -> Result<IndexCredit, Error>
{
    let mut prepared = HashMap::new();
    loop
    {
        let need = state(|s| {
            s.index
             .len()
             .checked_add(s.credits)
             .and_then(|n| n.checked_add(1))
             .ok_or(Error::Allocation)
        })??;
        let enough = state(|s| s.index.capacity() >= need)?;
        if !enough
        {
            crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                prepared.try_reserve(need)
            }).map_err(|_| Error::Allocation)?;
        }
        let (credit, retired) = state(|s| {
            let Some(need) = s.index.len().checked_add(s.credits).and_then(|n| n.checked_add(1))
            else
            {
                return (None, prepared);
            };
            if s.index.capacity() >= need
            {
                s.credits += 1;
                return (Some(IndexCredit { active: true }), prepared);
            }
            if prepared.capacity() < need
            {
                return (None, prepared);
            }
            let mut old = core::mem::replace(&mut s.index, prepared);
            for (key, value) in old.drain()
            {
                s.index.insert(key, value);
            }
            s.credits += 1;
            (Some(IndexCredit { active: true }), old)
        })?;
        drop(retired);
        if let Some(credit) = credit
        {
            return Ok(credit);
        }
        prepared = HashMap::new();
    }
}
fn publication(owner: &OwnerRef) -> Result<PublicationPermit<InvocationGate>, Error>
{
    loop
    {
        let plan = state(|s| s.store.plan_publication(owner))?.map_err(Error::Facility)?;
        let prepared = plan.prepare().map_err(Error::Facility)?;
        match state(|s| s.store.admit_prepared(prepared))?
        {
            Ok((permit, old)) =>
            {
                old.dispose();
                return Ok(permit);
            }
            Err(failure) =>
            {
                let cause = failure.cause;
                drop(failure.preparation);
                if cause != PreparedError::CapacityChanged
                {
                    return Err(Error::Preparation(cause));
                }
            }
        }
    }
}
pub(super) fn cached(kind: [u8; 4], id: i16, origin: u64) -> Result<Option<DefinitionRef>, Error>
{
    state(|s| {
        let Some(key) = s.index.get(&Identity { kind,
                                                id,
                                                origin })
        else
        {
            return Ok(None);
        };
        let d = s.store.completing(key).ok_or(Error::Source)?;
        if !d.owner.accepting()
        {
            return Err(Error::Stopping);
        }
        let cell = d.state.lock();
        Ok(cell.entry.is_some().then_some(DefinitionRef(*key)))
    })?
}
/// Only the loader's authenticated source query reaches this identity. No
/// source/owner is selected by wire input and no new incarnation substitutes.
pub(super) fn finish_existing(kind: [u8; 4], id: i16, origin: u64)
                              -> Result<Option<DefinitionRef>, Error>
{
    if let Some(reference) = cached(kind, id, origin)?
    {
        return Ok(Some(reference));
    }
    let prepared = state(|s| {
        let Some(key) = s.index.get(&Identity { kind,
                                                id,
                                                origin })
        else
        {
            return Ok(None);
        };
        let d = s.store.completing(key).ok_or(Error::Source)?;
        let preparation = Preparation::claim(d)?;
        Ok::<_, Error>(Some((*key, preparation)))
    })??;
    let Some((key, prepare)) = prepared
    else
    {
        return Ok(None);
    };
    let mapping =
        InvocationGate::run(|| prepare.definition.state.lock().mapping.clone()).ok_or(Error::Busy)?;
    if mapping.quarantined()
    {
        return Err(Error::Retained(mapping.refusal()));
    }
    finish(key, &prepare, &mapping).map(Some)
}

struct Preparation
{
    definition: Arc<Definition>,
    _released:  PreparationReleased
}
struct PreparationReleased;
impl Drop for PreparationReleased
{
    fn drop(&mut self)
    {
        let preparation = WAITING_PREPARATIONS.swap(false, Ordering::AcqRel);
        let readers = WAITING_READERS.swap(false, Ordering::AcqRel);
        if preparation || readers
        {
            crate::machine::paging::modules::changed();
            crate::toolbox::processes::retirement::resources_released();
        }
    }
}
impl Preparation
{
    fn claim(definition: &Arc<Definition>) -> Result<Self, Error>
    {
        InvocationGate::run(|| {
            let mut cell = definition.state.lock();
            if cell.closing || !definition.owner.accepting()
            {
                return Err(Error::Stopping);
            }
            if cell.busy
            {
                return Err(Error::Busy);
            }
            cell.busy = true;
            Ok(())
        })?;
        Ok(Self { definition: definition.clone(),
                  _released:  PreparationReleased })
    }
}
impl Drop for Preparation
{
    fn drop(&mut self)
    {
        InvocationGate::run(|| self.definition.state.lock().busy = false);
    }
}
fn placement(store: &Store<Arc<Definition>>, extent: u64) -> Result<u64, Error>
{
    let mut base =
        crate::machine::memory::DEFINITION_LIMIT.checked_sub(extent).ok_or(Error::Placement)?;
    loop
    {
        if base < crate::machine::memory::DEFINITION_BASE
        {
            return Err(Error::Placement);
        }
        match store.retained_values().find(|d| {
                                         base < d.limit.load(Ordering::Acquire)
                                         && d.base.load(Ordering::Acquire) < base + extent
                                     })
        {
            None => return Ok(base),
            Some(d) =>
            {
                base = d.base.load(Ordering::Acquire).checked_sub(extent).ok_or(Error::Placement)?
            }
        }
    }
}
pub(super) fn prepare(source: SourceBinding, kind: [u8; 4], id: i16,
                      code: mac_formats::code::NativeCode)
                      -> Result<DefinitionRef, Error>
{
    if !source.valid()
    {
        return Err(Error::Source);
    }
    if !source.owner().accepting()
    {
        return Err(Error::Stopping);
    }
    let identity = Identity { kind,
                              id,
                              origin: source.origin() };
    if let Some(ready) = finish_existing(kind, id, identity.origin)?
    {
        return Ok(ready);
    }
    let extent = code.descriptor
                     .load_limit
                     .checked_sub(code.descriptor.load_base)
                     .filter(|n| *n != 0 && n.is_multiple_of(crate::machine::memory::PAGE_BYTES))
                     .ok_or(Error::Placement)?;
    if extent > crate::machine::memory::DEFINITION_LIMIT - crate::machine::memory::DEFINITION_BASE
    {
        return Err(Error::Placement);
    }
    let target = source.allocation_target();
    crate::allocator::with_target(target, || {
        let mut credit = index_credit()?;
        let permit = publication(source.owner())?;
        let d = Arc::try_new(Definition { identity,
                                          owner: source.owner().clone(),
                                          base: AtomicU64::new(0),
                                          limit: AtomicU64::new(0),
                                          state: Spinlock::new(Cell { busy:    true,
                                                                      closing: false,
                                                                      entry:   None,
                                                                      mapping: None }),
                                          execution:
                                              ExecutionGate::try_new().map_err(Error::Execution)?,
                                          retiring: AtomicBool::new(false),
                                          source }).map_err(|_| Error::Allocation)?;
        let result = state(|s| {
            if s.index.contains_key(&identity)
            {
                return Err((Error::Busy, permit, None));
            }
            if !credit.active
               || s.credits == 0
               || s.index.capacity() < s.index.len().checked_add(s.credits).unwrap_or(usize::MAX)
            {
                return Err((Error::Allocation, permit, None));
            }
            let base = match placement(&s.store, extent)
            {
                Ok(base) => base,
                Err(e) => return Err((e, permit, None))
            };
            d.base.store(base, Ordering::Release);
            d.limit.store(base + extent, Ordering::Release);
            match s.store.publish_prepared(permit, d.clone())
            {
                Ok((publication, old)) =>
                {
                    let key = publication.into_parts().1;
                    s.index.insert(identity, key);
                    s.credits -= 1;
                    credit.active = false;
                    Ok((key, old))
                }
                Err(failure) =>
                {
                    Err((Error::Preparation(failure.cause), failure.permit, Some(failure.value)))
                }
            }
        })?;
        let (key, old) = match result
        {
            Ok(v) => v,
            Err((cause, permit, value)) =>
            {
                drop(value);
                drop(permit);
                return Err(cause);
            }
        };
        old.dispose();
        let preparation = Preparation { definition: d,
                                        _released:  PreparationReleased };
        let base = preparation.definition.base.load(Ordering::Acquire);
        let mapping =
            match Mapping::prepare_definition(&preparation.definition.owner,
                                              &preparation.definition.source.mapping_target(),
                                              &code.descriptor,
                                              &code.code,
                                              &code.relocations,
                                              base)
            {
                Ok(mapping) => mapping,
                Err(cause) =>
                {
                    discard(key, preparation)?;
                    return Err(Error::Mapping(cause));
                }
            };
        InvocationGate::run(|| preparation.definition.state.lock().mapping = Some(mapping.clone()));
        match finish(key, &preparation, &mapping)
        {
            Ok(reference) => Ok(reference),
            Err(cause) =>
            {
                if mapping.unpublished()
                {
                    mapping.retire().map_err(Error::Mapping)?;
                    drop(mapping);
                    discard(key, preparation)?;
                }
                Err(cause)
            }
        }
    })
}

fn finish(key: CompletionKey, preparation: &Preparation, mapping: &Arc<Mapping>)
          -> Result<DefinitionRef, Error>
{
    mapping.publish().map_err(Error::Mapping)?;
    if !preparation.definition.owner.accepting()
    {
        return Err(Error::Stopping);
    }
    let address = preparation.definition.base.load(Ordering::Acquire);
    // Entry is computed from the validated plan, not assumed to be the range base.
    let entry = mapping.entry().ok_or(Error::Source)?;
    if entry < address || entry >= preparation.definition.limit.load(Ordering::Acquire)
    {
        return Err(Error::Source);
    }
    let function = if preparation.definition.owner.owner().kind()
                      == mac_object_table::domain::DomainKind::Process
    {
        Entry::User(entry)
    }
    else
    {
        // Only authentic System/Module sources may become resident callables.
        Entry::Trusted(unsafe { core::mem::transmute(entry as usize) })
    };
    InvocationGate::run(|| preparation.definition.state.lock().entry = Some(function));
    Ok(DefinitionRef(key))
}
fn discard(key: CompletionKey, preparation: Preparation) -> Result<(), Error>
{
    let removed = state(|s| {
        if s.index.get(&preparation.definition.identity) != Some(&key)
        {
            return Err(Error::Source);
        }
        match s.store.remove_with_backing(key.id())
        {
            Some(removed) =>
            {
                s.index.remove(&preparation.definition.identity);
                Ok(Some(removed))
            }
            // A concurrent stopped-owner walk owns its unique pending ticket.
            // Preparation Drop releases busy/Arc then posts its actual wake.
            None if s.store.completing(&key).is_some() => Ok(None),
            None => Err(Error::Source)
        }
    })??;
    drop(preparation);
    if let Some(removed) = removed
    {
        removed.dispose()
    }
    Ok(())
}

struct Invocation
{
    execution:  Option<ExecutionPin>,
    definition: Option<Arc<Definition>>
}
impl Drop for Invocation
{
    fn drop(&mut self)
    {
        drop(self.execution.take());
        let retiring = self.definition.as_ref().is_some_and(|d| d.retiring.load(Ordering::Acquire));
        drop(self.definition.take());
        let readers = WAITING_READERS.swap(false, Ordering::AcqRel);
        if retiring || readers
        {
            crate::machine::paging::modules::changed();
            crate::toolbox::processes::retirement::resources_released();
        }
    }
}
fn pin(reference: DefinitionRef, kind: u32, id: i16, disposal: bool)
       -> Result<(Invocation, Entry), Status>
{
    state(|s| {
        let d = s.store.completing(&reference.0).ok_or(NOT_FOUND)?;
        if d.identity.kind != kind.to_be_bytes() || d.identity.id != id
        {
            return Err(BAD_ARGUMENT);
        }
        if d.owner.owner() != reference.0.owner()
        {
            return Err(ACCESS_DENIED);
        }
        let cell = d.state.lock();
        if cell.closing || (!disposal && !d.owner.accepting())
        {
            return Err(SERVICE_BUSY);
        }
        // Retained WDEF Dispose currently runs from the original Window release
        // ticket after owner stop. A blanket accepting check would strand that
        // Window and therefore DDEF/IMAG cleanup. Restricting other stopped-owner
        // invocation requires the paired genuine cleanup authority, not a wire
        // message/boolean exception. Already pinned calls retain their lifetime.
        let entry = cell.entry.ok_or(SERVICE_BUSY)?;
        let execution = d.execution.pin().map_err(|_| SERVICE_BUSY)?;
        Ok((Invocation { execution:  Some(execution),
                         definition: Some(d.clone()) },
            entry))
    }).map_err(|e| e.status())?
}

pub(crate) fn invoke(reference: DefinitionRef, call: &mut DefinitionCall) -> i64
{
    invoke_admitted(reference, call, false)
}

/// The only stopped-source admission is a real Window Manager cleanup turn.
/// Its nonconstructible permit borrows the original invalidate-first ticket;
/// inspecting the wire message alone never grants this authority.
pub(crate) fn invoke_disposal(permit: crate::toolbox::windows::manager::DefinitionDisposal<'_>,
                              call: &mut DefinitionCall)
                              -> i64
{
    if call.kind != mac_abi::definitions::WDEF || call.message != mac_abi::definitions::DISPOSE
    {
        return BAD_ARGUMENT as i64;
    }
    invoke_admitted(permit.entry(), call, true)
}

fn invoke_admitted(reference: DefinitionRef, call: &mut DefinitionCall, disposal: bool) -> i64
{
    let (_pin, entry) = match pin(reference, call.kind, call.id, disposal)
    {
        Ok(v) => v,
        Err(e) => return e as i64
    };
    let Entry::Trusted(entry) = entry
    else
    {
        // A Process definition is never an S-mode callable. Its manager must
        // yield an owned callback turn, ending Desktop before user execution.
        return SERVICE_BUSY as i64;
    };
    crate::context::current_thread_context().definitions
                                            .with(call.kind, call.id, call.surface, || unsafe {
                                                entry(call)
                                            })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Drain
{
    Objects,
    Threads,
    Invocations,
    Preparation,
    Readers
    {
        retained: usize
    },
    Mapping(crate::machine::paging::modules::CleanupCause)
}
#[derive(Clone, Copy)]
pub(crate) enum Registration
{
    Definition,
    Storage
}
impl Registration
{
    pub(crate) const ALL: [Self; 2] = [Self::Definition, Self::Storage];

    pub(crate) fn specification(self) -> Specification<'static>
    {
        Specification { kind:     KindCode(match self
                                           {
                                               Self::Definition => *b"DDEF",
                                               Self::Storage => *b"DSTA"
                                           }),
                        label:    match self
                        {
                            Self::Definition => "Loaded definition callables",
                            Self::Storage => "Definition callable backing"
                        },
                        provider: "System Definition Manager",
                        capacity: CapacityPolicy::GrowableUntilMemory,
                        source:   Source::Resident }
    }
}
impl Release<dyn crate::toolbox::handles::cleanup::context::Context, Failure> for Registration
{
    fn release(&self, _: &mut (dyn crate::toolbox::handles::cleanup::context::Context + 'static),
               ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        let owner = ticket.key().owner().incarnation();
        if matches!(self, Self::Storage)
        {
            return match state_owned(ticket, |ticket, s| {
                      if s.store.publication_preparations(ticket.key().owner()) != 0
                      {
                          return Err(Failure::definition_pending(ticket,
                                                                 owner,
                                                                 Drain::Preparation));
                      }
                      s.store.release_storage_with_backing(ticket).map_err(Failure::from)
                  })
            {
                Ok(result) =>
                {
                    drop(result?);
                    Ok(())
                }
                Err((_, ticket)) => Err(Failure::context_unavailable(ticket, "definition registry"))
            };
        }
        let definition = match state(|s| s.store.releasing(&ticket).cloned())
        {
            Ok(Some(definition)) =>
            {
                RetirementRead { definition,
                                 _released: ClaimReaderReleased }
            }
            _ => return Err(Failure::context_unavailable(ticket, "definition registry"))
        };
        definition.retiring.store(true, Ordering::Release);
        // A User definition Region owns a genuine SPCE pin until its mapping
        // unmap/TLB ACK. Requiring SPCE to disappear here would prevent that
        // same DDEF mapping from ever releasing its parent. All other objects
        // still drain first; actual THRD/readers/Loading checks remain below.
        const DEFINITION_DRAIN_ROOTS: [KindCode; crate::images::ROOT_KINDS.len() + 2] =
            [crate::images::ROOT_KINDS[0],
             crate::images::ROOT_KINDS[1],
             crate::images::ROOT_KINDS[2],
             crate::images::ROOT_KINDS[3],
             KindCode(*b"SPCE"),
             KindCode(*b"SPST")];
        if definition.owner.has_resources_outside(&DEFINITION_DRAIN_ROOTS)
        {
            return Err(Failure::definition_pending(ticket, owner, Drain::Objects));
        }
        if InvocationGate::run(|| crate::machine::scheduler::has_threads(owner))
        {
            return Err(Failure::definition_pending(ticket, owner, Drain::Threads));
        }
        WAITING_PREPARATIONS.store(true, Ordering::Release);
        WAITING_READERS.store(true, Ordering::Release);
        let admission = state(|_| {
            let mut cell = definition.state.lock();
            if cell.busy
            {
                return Err(Drain::Preparation);
            }
            // Bind/Invocation/Preparation admission also holds State. Test all
            // real retained readers BEFORE closing so an original foreign
            // Window can still run its genuine ticket-owned Dispose.
            let readers = Arc::strong_count(&definition.definition);
            if readers != 2
            {
                return Err(Drain::Readers { retained: readers.saturating_sub(2) });
            }
            let exclusion = definition.execution.begin_load().map_err(|_| Drain::Invocations)?;
            cell.closing = true;
            Ok(exclusion)
        });
        let exclusion = match admission
        {
            Ok(Ok(exclusion)) => exclusion,
            Ok(Err(cause)) => return Err(Failure::definition_pending(ticket, owner, cause)),
            Err(_) => return Err(Failure::context_unavailable(ticket, "definition registry"))
        };
        let mapping = InvocationGate::run(|| {
            let cell = definition.state.lock();
            if cell.mapping.is_none() && cell.entry.is_some()
            {
                return Err(Drain::Mapping(crate::machine::paging::modules::CleanupCause::State {
                    base: definition.base.load(Ordering::Acquire)
                }));
            }
            Ok(cell.mapping.clone())
        });
        let mapping = match mapping
        {
            Ok(mapping) => mapping,
            Err(cause) => return Err(Failure::definition_pending(ticket, owner, cause))
        };
        if let Some(mapping) = &mapping
        {
            if !mapping.settled()
            {
                // Desktop owns this walk. Only publish the existing mapping's
                // real maintenance notice here; no PTE operation/ACK or page
                // disposal runs while Desktop is borrowed.
                WAITING_MONITOR.store(true, Ordering::Release);
                mapping.request_retirement();
                return Err(Failure::definition_pending(ticket,
                                                       owner,
                                                       Drain::Mapping(mapping.refusal())));
            }
        }
        // Absent mapping is admitted only for a never-published preparation
        // (entry=None above). A real mapping must already be settled here.
        drop(mapping);
        let removed = match state_owned(ticket, |ticket, s| {
                  let result = s.store.release_with_backing(ticket);
                  if result.is_ok()
                  {
                      s.index.remove(&definition.identity);
                  }
                  result
              })
        {
            Ok(result) => result,
            Err((_, ticket)) =>
            {
                return Err(Failure::context_unavailable(ticket, "definition registry"));
            }
        };
        drop(exclusion);
        drop(definition);
        match removed
        {
            Ok(removed) =>
            {
                removed.dispose();
                Ok(())
            }
            Err(cause) => Err(cause.into())
        }
    }
}
/// A cleanup reader can temporarily block a guard-free retirement claim. Its
/// checkpoint runs after the actual Arc release, without depending on an exact
/// final count or borrowing payload which may just have been destroyed.
struct RetirementRead
{
    definition: Arc<Definition>,
    _released:  ClaimReaderReleased
}
impl core::ops::Deref for RetirementRead
{
    type Target = Definition;

    fn deref(&self) -> &Definition
    {
        &self.definition
    }
}
struct ClaimReaderReleased;
impl Drop for ClaimReaderReleased
{
    fn drop(&mut self)
    {
        if WAITING_CLAIM_READERS.swap(false, Ordering::AcqRel)
        {
            crate::machine::paging::modules::changed();
            crate::toolbox::processes::retirement::resources_released();
        }
    }
}
/// One real canonical DDEF/execution exclusion. The original cleanup ticket
/// remains in the owner walk; neither source nor mapping is detached from it.
pub(crate) struct MappingRetirement
{
    exclusion:  Option<mac_kernel_model::image::Loading>,
    definition: Option<Arc<Definition>>,
    base:       u64,
    not_run:    bool
}
impl MappingRetirement
{
    pub(crate) fn base(&self) -> u64
    {
        self.base
    }

    pub(crate) fn run(mut self) -> Result<(), crate::machine::paging::modules::Error>
    {
        use crate::machine::paging::modules::Error;
        self.not_run = false;
        let definition = self.definition.as_ref().ok_or(Error::State)?;
        // These were true before the canonical walk requested retirement.
        // Revalidate before any PTE effect, outside all manager guards. Closing
        // prevents new invocation/preparation; the retained Loading excludes
        // calls already in flight, and busy excludes a second retirement turn.
        if definition.owner.accepting()
           || !crate::images::Manager::root_only(&definition.owner)
           || InvocationGate::run(|| {
               crate::machine::scheduler::has_threads(definition.owner.owner().incarnation())
           })
        {
            return Err(Error::Closed);
        }
        let mapping = InvocationGate::run(|| {
            let cell = definition.state.lock();
            if !cell.closing || !cell.busy
            {
                return Err(Error::State);
            }
            let mapping = cell.mapping.as_ref().ok_or(Error::State)?;
            if !mapping.ready()
            {
                return Err(Error::State);
            }
            Ok(mapping.clone())
        })?;
        mapping.retire()
    }
}
impl Drop for MappingRetirement
{
    fn drop(&mut self)
    {
        drop(self.exclusion.take());
        if let Some(definition) = self.definition.take()
        {
            InvocationGate::run(|| definition.state.lock().busy = false);
            drop(definition);
        }
        // Abandoning a claimed operation before run consumed no notice. A run
        // refused by a busy monitor waits for that real lease release instead.
        if self.not_run
        {
            crate::machine::paging::modules::changed();
        }
        crate::toolbox::processes::retirement::resources_released();
    }
}
pub(crate) fn mapping_retirement(after: u64) -> Result<Option<MappingRetirement>, Error>
{
    if DEFINITIONS.get().is_none()
    {
        return Ok(None);
    }
    let (claim, changed) = state(|s| {
        let mut changed = false;
        let candidate = s.store
                         .retained_values()
                         .filter(|definition| {
                             if definition.base.load(Ordering::Acquire) <= after
                                || !definition.retiring.load(Ordering::Acquire)
                             {
                                 return false;
                             }
                             let cell = definition.state.lock();
                             if !cell.closing
                                || cell.busy
                                || !cell.mapping.as_ref().is_some_and(|mapping| mapping.ready())
                             {
                                 return false;
                             }
                             // Skip this blocked row without starving another
                             // definition. Arm then recheck its actual reader
                             // release race; notification happens outside State.
                             if Arc::strong_count(definition) != 1
                             {
                                 WAITING_CLAIM_READERS.store(true, Ordering::Release);
                                 changed |= Arc::strong_count(definition) == 1;
                                 return false;
                             }
                             true
                         })
                         .min_by_key(|definition| definition.base.load(Ordering::Acquire));
        let Some(definition) = candidate
        else
        {
            return (None, changed);
        };
        // Store is the sole reader here and State excludes a new cleanup
        // reader until this real busy claim is published. Closing already
        // prevents new author/preparation admission.
        let mut cell = definition.state.lock();
        let exclusion = match definition.execution.begin_load()
        {
            Ok(exclusion) => exclusion,
            Err(_) => return (None, changed)
        };
        cell.busy = true;
        (Some(MappingRetirement { exclusion:  Some(exclusion),
                                  definition: Some(definition.clone()),
                                  base:       definition.base.load(Ordering::Acquire),
                                  not_run:    true }),
         changed)
    })?;
    if changed
    {
        crate::machine::paging::modules::changed();
    }
    Ok(claim)
}
pub(crate) fn monitor_released()
{
    if WAITING_MONITOR.swap(false, Ordering::AcqRel)
    {
        crate::toolbox::processes::retirement::resources_released();
    }
}
#[derive(Clone, Copy)]
pub(crate) struct Range
{
    pub(crate) base:             u64,
    pub(crate) limit:            u64,
    pub(crate) owner:            DomainId,
    pub(crate) retirement_pages: usize
}
pub(crate) fn visibility_snapshot() -> Result<Vec<Range>, Error>
{
    let mut ranges = Vec::new();
    loop
    {
        let count = state(|s| s.store.retained_values().count())?;
        ranges.try_reserve(count).map_err(|_| Error::Allocation)?;
        let full = state(|s| {
            for d in s.store.retained_values()
            {
                if ranges.len() == ranges.capacity()
                {
                    return false;
                }
                ranges.push(Range { base:             d.base.load(Ordering::Acquire),
                                    limit:            d.limit.load(Ordering::Acquire),
                                    owner:            d.owner.owner(),
                                    // Scalar demand from the actual canonical
                                    // Mapping; no execution/account pin escapes.
                                    retirement_pages:
                                        d.state
                                         .lock()
                                         .mapping
                                         .as_ref()
                                         .map_or(0, |mapping| mapping.retirement_pages()) })
            }
            true
        })?;
        if full
        {
            return Ok(ranges);
        }
        ranges.clear();
    }
}
