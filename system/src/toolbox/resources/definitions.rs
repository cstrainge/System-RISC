//! Registered definition callables and the single native invocation boundary.
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{ACCESS_DENIED, BAD_ARGUMENT, DefinitionCall, NO_MEMORY, NOT_FOUND, SERVICE_BUSY,
              Status};
use mac_kernel_model::image::{ExecutionGate, ExecutionPin};
use mac_object_table::{domain::ReleasedResource,
                       registry::{Release, Source, Specification},
                       shared::InvocationGate as _,
                       store::CompletionKey};

use crate::toolbox::handles::{CapacityPolicy, InvocationGate, KindCode, OwnerRef, Store,
                              cleanup::Failure};

#[derive(Debug)]
pub(crate) enum Error
{
    Registry(crate::state::AccessError),
    Facility(mac_object_table::domain::PoolError),
    Preparation(mac_object_table::store::PreparedError),
    OwnerUnavailable(Status),
    Allocation,
    Placement,
    Source,
    Stopping,
    Execution(mac_kernel_model::image::Error)
}
impl Error
{
    pub(crate) fn status(&self) -> Status
    {
        match self {
            Self::Registry(cause) => cause.status(),
            Self::Facility(cause) => crate::toolbox::handles::registry_status(
                &mac_object_table::registry::Error::Pool(*cause)),
            Self::Preparation(mac_object_table::store::PreparedError::Pool(cause)) =>
                crate::toolbox::handles::registry_status(&mac_object_table::registry::Error::Pool(*cause)),
            Self::Preparation(mac_object_table::store::PreparedError::CapacityChanged) => SERVICE_BUSY,
            Self::Preparation(_) => mac_abi::BAD_ABI,
            Self::OwnerUnavailable(status) => *status,
            Self::Allocation | Self::Placement => NO_MEMORY,
            Self::Source => ACCESS_DENIED,
            Self::Stopping => SERVICE_BUSY,
            Self::Execution(mac_kernel_model::image::Error::Allocation | mac_kernel_model::image::Error::PinLimit) => NO_MEMORY,
            Self::Execution(mac_kernel_model::image::Error::Busy) => SERVICE_BUSY,
        }
    }
}
impl core::fmt::Display for Error
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Registry(cause) => write!(out, "definition registry unavailable: {cause:?}"),
            Self::Facility(cause) => write!(out, "definition registration refused: {cause:?}"),
            Self::Preparation(cause) =>
            {
                write!(out, "definition publication preparation refused: {cause}")
            }
            Self::OwnerUnavailable(status) =>
            {
                write!(out, "definition source authority unavailable: status {status}")
            }
            Self::Allocation => out.write_str("definition callable metadata allocation failed"),
            Self::Placement => out.write_str("definition executable address space exhausted"),
            Self::Source =>
            {
                out.write_str("definition source does not match its original owner/origin")
            }
            Self::Stopping =>
            {
                out.write_str("definition source stopped before callable publication")
            }
            Self::Execution(cause) => cause.fmt(out)
        }
    }
}

/// Original registered completion authority, never an executable pointer or a
/// caller-constructed ID. Copies still validate the full generation at invocation.
#[derive(Clone, Copy)]
pub(crate) struct DefinitionRef(CompletionKey);
struct Definition
{
    owner:     OwnerRef,
    kind:      [u8; 4],
    id:        i16,
    origin:    u64,
    base:      u64,
    limit:     u64,
    entry:     mac_abi::DefinitionEntry,
    execution: ExecutionGate,
    retiring:  AtomicBool,
    _source:   Option<crate::images::Read>
}
static DEFINITIONS: crate::state::TryState<Store<Arc<Definition>>> = crate::state::TryState::new();

pub(super) fn initialize() -> Result<(), Error>
{
    let (objects, storage) =
        crate::toolbox::handles::definition_kinds().map_err(Error::OwnerUnavailable)?;
    let store = Store::owned_prepared(objects, storage).map_err(Error::Facility)?;
    DEFINITIONS.publish(store).map_err(Error::Registry)
}

/// Fixed-arena placement remains serialized by Desktop during this slice.
/// Canonical retained records, including cleanup quarantine, own existing ranges.
pub(super) fn placement(extent: u64) -> Result<u64, Error>
{
    InvocationGate::run(|| {
        DEFINITIONS.with(|store| {
                       let mut base =
                           crate::machine::memory::DEFINITION_LIMIT.checked_sub(extent)
                                                                   .ok_or(Error::Placement)?;
                       loop
                       {
                           if base < crate::machine::memory::DEFINITION_BASE
                           {
                               return Err(Error::Placement);
                           }
                           let overlap =
                               store.retained_values()
                                    .find(|entry| base < entry.limit && entry.base < base + extent);
                           match overlap
                           {
                               None => return Ok(base),
                               Some(other) =>
                               {
                                   base = other.base.checked_sub(extent).ok_or(Error::Placement)?
                               }
                           }
                       }
                   })
                   .map_err(Error::Registry)?
    })
}

pub(super) fn publish(owner: OwnerRef, source: Option<crate::images::Read>, kind: [u8; 4],
                      id: i16, origin: u64, base: u64, limit: u64, entry: mac_abi::DefinitionEntry)
                      -> Result<DefinitionRef, Error>
{
    match &source
    {
        Some(source)
            if source.binding().id() == origin && source.owner().owner() == owner.owner() =>
        {}
        None if origin == 0
                && owner.owner().kind() == mac_object_table::domain::DomainKind::System =>
        {}
        _ => return Err(Error::Source)
    }
    if !owner.accepting()
    {
        return Err(Error::Stopping);
    }
    let allocation = source.as_ref().map(|source| source.allocation().clone());
    let publish = || {
        let definition =
            Arc::try_new(Definition { owner: owner.clone(),
                                      kind,
                                      id,
                                      origin,
                                      base,
                                      limit,
                                      entry,
                                      execution:
                                          ExecutionGate::try_new().map_err(Error::Execution)?,
                                      retiring: AtomicBool::new(false),
                                      _source: source }).map_err(|_| Error::Allocation)?;
        let plan = InvocationGate::run(|| DEFINITIONS.with(|store| store.plan_publication(&owner)))
            .map_err(Error::Registry)?.map_err(Error::Facility)?;
        let preparation = plan.prepare().map_err(Error::Facility)?;
        let admitted = InvocationGate::run(|| {
            DEFINITIONS.with_owned(preparation, |preparation, store| {
                           store.admit_prepared(preparation)
                       })
        });
        let (permit, retired) = match admitted
        {
            Ok(Ok(admission)) => admission,
            Ok(Err(failure)) =>
            {
                let cause = failure.cause;
                drop(failure);
                return Err(Error::Preparation(cause));
            }
            Err((cause, preparation)) =>
            {
                drop(preparation);
                return Err(Error::Registry(cause));
            }
        };
        drop(retired);
        let published = InvocationGate::run(|| {
            DEFINITIONS.with_owned((permit, definition), |(permit, definition), store| {
                           store.publish_prepared(permit, definition)
                       })
        });
        match published
        {
            Ok(Ok((publication, retired))) =>
            {
                drop(retired);
                Ok(DefinitionRef(publication.into_parts().1))
            }
            Ok(Err(failure)) =>
            {
                let cause = failure.cause;
                drop(failure);
                Err(Error::Preparation(cause))
            }
            Err((cause, pending)) =>
            {
                drop(pending);
                Err(Error::Registry(cause))
            }
        }
    };
    match allocation {
        Some(account) => crate::allocator::with_target(crate::allocator::Target::Owner {
            account, class: mac_kernel_model::policy::MemoryClass::Kernel
        }, publish),
        None => crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, publish),
    }
}

/// Cache rows carry identity only; stale rows cannot resurrect completed slots.
pub(super) fn matches(reference: DefinitionRef, kind: [u8; 4], id: i16, origin: u64)
                      -> Result<bool, Error>
{
    InvocationGate::run(|| {
        DEFINITIONS.with(|store| {
                       let Some(entry) = store.completing(&reference.0)
                       else
                       {
                           return Ok(false);
                       };
                       if !entry.owner.accepting()
                       {
                           return Err(Error::Stopping);
                       }
                       Ok(entry.kind == kind && entry.id == id && entry.origin == origin)
                   })
                   .map_err(Error::Registry)?
    })
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
        let retiring =
            self.definition.as_ref().is_some_and(|entry| entry.retiring.load(Ordering::Acquire));
        drop(self.definition.take());
        if retiring
        {
            crate::toolbox::processes::retirement::resources_released();
        }
    }
}
fn pin(reference: DefinitionRef, call: &DefinitionCall) -> Result<Invocation, Status>
{
    InvocationGate::run(|| {
        DEFINITIONS.with(|store| {
                       let definition = store.completing(&reference.0).ok_or(NOT_FOUND)?;
                       if definition.kind != call.kind.to_be_bytes() || definition.id != call.id
                       {
                           return Err(BAD_ARGUMENT);
                       }
                       if definition.owner.owner() != reference.0.owner()
                       {
                           return Err(ACCESS_DENIED);
                       }
                       let execution = definition.execution.pin().map_err(|_| SERVICE_BUSY)?;
                       Ok(Invocation { execution:  Some(execution),
                                       definition: Some(definition.clone()) })
                   })
                   .map_err(|cause| cause.status())?
    })
}

pub(crate) fn invoke(reference: DefinitionRef, call: &mut DefinitionCall) -> i64
{
    let pin = match pin(reference, call)
    {
        Ok(pin) => pin,
        Err(status) => return status as i64
    };
    let Some(definition) = pin.definition.as_ref()
    else
    {
        return SERVICE_BUSY as i64;
    };
    // The original registered receipt remains valid for an existing object's
    // Dispose after stop. No public lookup or new admission can forge it. The
    // cleanup walk cannot release this source/code while the invocation is held.
    crate::context::current_thread_context().definitions.with(call.kind,
                                                              call.id,
                                                              call.surface,
                                                              || {
                                                                  // SAFETY: loader-validated entry
                                                                  // from the generation-checked
                                                                  // original
                                                                  // record; actual execution/source
                                                                  // lifetime is pinned across this
                                                                  // call.
                                                                  unsafe {
                                                                      (definition.entry)(call)
                                                                  }
                                                              })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Drain
{
    Objects,
    Threads,
    Invocations
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
        let (kind, label) = match self
        {
            Self::Definition => (*b"DDEF", "Loaded definition callables"),
            Self::Storage => (*b"DSTA", "Definition callable backing")
        };
        Specification { kind: KindCode(kind),
                        label,
                        provider: "System Definition Manager",
                        capacity: CapacityPolicy::GrowableUntilMemory,
                        source: Source::Resident }
    }
}
impl Release<dyn crate::toolbox::handles::cleanup::context::Context, Failure> for Registration
{
    fn release(&self, _: &mut (dyn crate::toolbox::handles::cleanup::context::Context + 'static),
               ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        // Do not nest the scheduler lock under the definition registry. The
        // canonical stopping owner forbids fresh threads; unresolved thread
        // publications also remain non-root resources checked below.
        let threads = matches!(self, Self::Definition)
                      && InvocationGate::run(|| {
                          crate::machine::scheduler::has_threads(ticket.key().owner().incarnation())
                      });
        let removed = InvocationGate::run(|| {
            DEFINITIONS.with_owned(ticket, |ticket, store| {
                           if matches!(self, Self::Storage)
                           {
                               return store.release_storage_with_backing(ticket)
                                           .map(|retired| (None, retired, None))
                                           .map_err(Failure::from);
                           }
                           let exclusion = if let Some(definition) = store.releasing(&ticket)
                           {
                               definition.retiring.store(true, Ordering::Release);
                               let owner = definition.owner.owner().incarnation();
                               if !crate::images::Manager::root_only(&definition.owner)
                               {
                                   return Err(Failure::definition_pending(ticket,
                                                                          owner,
                                                                          Drain::Objects));
                               }
                               if threads
                               {
                                   return Err(Failure::definition_pending(ticket,
                                                                          owner,
                                                                          Drain::Threads));
                               }
                               match definition.execution.begin_load()
                               {
                                   Ok(exclusion) => Some(exclusion),
                                   Err(_) =>
                                   {
                                       return Err(Failure::definition_pending(ticket,
                                                                              owner,
                                                                              Drain::Invocations));
                                   }
                               }
                           }
                           else
                           {
                               None
                           };
                           store.release_with_backing(ticket)
                                .map(|removed| (Some(removed), None, exclusion))
                                .map_err(Failure::from)
                       })
        });
        match removed
        {
            Ok(result) =>
            {
                drop(result?);
                Ok(())
            }
            Err((_, ticket)) => Err(Failure::context_unavailable(ticket, "definition registry"))
        }
    }
}
