//! Registered immutable sources and private flat execution reservations.
#![forbid(unsafe_code)]
mod recovery;
mod user_runtime;
pub(crate) use recovery::{PendingRecoveryImage, RecoveryDisposition, RecoveryImageFailure};
mod sharing;
mod interning;
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use mac_abi::*;
use mac_object_table::{aliases,
                       domain::ReleasedResource,
                       registry::{Release, Source as RegistrationSource, Specification},
                       store::CompletionKey};

use crate::toolbox::handles::{CapacityPolicy, InvocationGate, KindCode, KindLease, OwnerRef,
                              cleanup::Failure};

pub(crate) const ROOT_KINDS: [KindCode; 4] =
    [KindCode(*b"IMAG"), KindCode(*b"IMST"), KindCode(*b"DDEF"), KindCode(*b"DSTA")];

#[derive(PartialEq)]
pub(crate) enum Role
{
    Application
    {
        authority: u64
    },
    Module
}

#[derive(PartialEq)]
pub(crate) struct Source
{
    pub role:      Role,
    pub resources: Vec<mac_formats::resource::Resource>,
    pub creator:   [u8; 4],
    pub file_type: [u8; 4]
}

impl Source
{
    pub(crate) fn application_authority(&self) -> Option<u64>
    {
        match self.role
        {
            Role::Application { authority } => Some(authority),
            Role::Module => None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Binding(CompletionKey);
impl Binding
{
    pub(crate) fn id(self) -> u64
    {
        self.0.id()
    }

    pub(crate) fn owner(self) -> u64
    {
        self.0.owner().incarnation()
    }
}

struct Activity
{
    readers:  AtomicUsize,
    retiring: AtomicBool
}
impl Activity
{
    fn retain(self: &Arc<Self>) -> Result<Use, Status>
    {
        self.readers
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |count| count.checked_add(1))
            .map_err(|_| NO_MEMORY)?;
        Ok(Use(self.clone()))
    }
}
struct Use(Arc<Activity>);
impl Drop for Use
{
    fn drop(&mut self)
    {
        if self.0.readers.fetch_sub(1, Ordering::SeqCst) == 1
           && self.0.retiring.load(Ordering::SeqCst)
        {
            crate::toolbox::processes::retirement::resources_released();
            crate::drivers::recovery_changed();
            crate::machine::paging::modules::changed();
        }
    }
}
struct Instance
{
    owner:              OwnerRef,
    module:             bool,
    executable:         Vec<core::ops::Range<u64>>,
    writable:           Vec<core::ops::Range<u64>>,
    initialized:        AtomicBool,
    mapping:            spin::Once<Arc<crate::machine::paging::modules::Mapping>>,
    retirement_claimed: AtomicBool,
    base:               u64,
    limit:              u64,
    execution:          mac_kernel_model::image::ExecutionGate,
    activity:           Arc<Activity>,
    binding:            spin::Once<Binding>,
    allocation:         crate::machine::scheduler::accounting::AllocationAuthority
}

/// Fields destroy source/instance payload references before the final use signal.
/// An alias cannot acknowledge cleanup while a reader still retains its payload.
pub(crate) struct Read
{
    source:   Arc<Source>,
    instance: Arc<Instance>,
    binding:  Binding,
    usage:    Use
}
impl Read
{
    pub(crate) fn source(&self) -> &Source
    {
        &self.source
    }

    /// Genuine source owner; a caller number cannot reconstruct this authority.
    pub(crate) fn owner(&self) -> &OwnerRef
    {
        &self.instance.owner
    }

    /// Original admission capability, retained with this execution instance.
    /// Resource consumers must not substitute the current drawing app's budget.
    pub(crate) fn allocation(&self) -> &crate::machine::scheduler::accounting::AllocationAuthority
    {
        &self.instance.allocation
    }

    /// Immutable package bytes can outlive this execution instance. A restart
    /// still receives a distinct destination and fresh writable image state.
    pub(crate) fn shared_source(&self) -> Arc<Source>
    {
        self.source.clone()
    }

    fn user_alias(&self, address: usize, bytes: usize, write: bool) -> Result<usize, Status>
    {
        self.instance
            .mapping
            .get()
            .ok_or(SERVICE_BUSY)?
            .user_alias(address, bytes, write)
            .map_err(|e| e.status())
    }

    pub(crate) fn user_spans(&self) -> Result<Vec<crate::machine::paging::user::Span>, Status>
    {
        if self.instance.module
           || self.instance.owner.owner().kind() != mac_object_table::domain::DomainKind::Process
        {
            return Err(ACCESS_DENIED);
        }
        self.instance.mapping.get().ok_or(SERVICE_BUSY)?.user_spans().map_err(|e| e.status())
    }

    pub(crate) fn binding(&self) -> Binding
    {
        self.binding
    }

    pub(crate) fn retain(&self) -> Result<Self, Status>
    {
        let usage = self.usage.0.retain()?;
        Ok(Self { source: self.source.clone(),
                  instance: self.instance.clone(),
                  binding: self.binding,
                  usage })
    }
}

/// The scheduler drops its stack, then this execution pin, then its source use.
pub(crate) struct Execution
{
    _execution: mac_kernel_model::image::ExecutionPin,
    _source:    Read
}
/// A real initialized module alias and its execution gate remain pinned together.
pub(crate) struct ModuleExecution(Execution);
impl ModuleExecution
{
    pub(crate) fn binding(&self) -> Binding
    {
        self.0._source.binding()
    }

    pub(crate) fn module(&self) -> u64
    {
        self.0._source.instance.owner.owner().incarnation()
    }

    pub(crate) fn frame(&self) -> ModuleFrame
    {
        ModuleFrame(self.0._source.instance.clone())
    }
}
/// Only a retained module execution can install this executor metadata.
/// Its enclosing invocation keeps the actual execution pin until restoration.
#[derive(Clone)]
pub(crate) struct ModuleFrame(Arc<Instance>);
impl ModuleFrame
{
    /// The authentic instance gate excludes every actual module invocation and
    /// worker stack while cleanup reclaims runtime DMA memory. This is not a
    /// new execution protocol or permission to retarget an allocation receipt.
    pub(crate) fn exclude_execution(&self) -> Result<mac_kernel_model::image::Loading, Status>
    {
        self.0.execution.begin_load().map_err(|_| SERVICE_BUSY)
    }

    pub(crate) fn authenticate(&self, module: u64) -> Result<(), Status>
    {
        if self.0.owner.owner().incarnation() == module { Ok(()) } else { Err(ACCESS_DENIED) }
    }

    pub(crate) fn owner(&self, module: u64) -> Result<OwnerRef, Status>
    {
        self.authenticate(module)?;
        Ok(self.0.owner.clone())
    }

    pub(crate) fn callback_allowed(&self, module: u64, address: usize) -> Result<(), Status>
    {
        self.authenticate(module)?;
        if address & 1 == 0
           && self.0.executable.iter().any(|range| range.contains(&(address as u64)))
        {
            Ok(())
        }
        else
        {
            Err(ACCESS_DENIED)
        }
    }
}
/// Process callback execution admitted against one actual initialized image.
/// This is not a resource selection or permission to impersonate a Process.
pub(crate) struct ApplicationExecution(Execution);
impl ApplicationExecution
{
    pub(crate) fn validate(&self, owner: &OwnerRef, address: usize) -> Result<(), Status>
    {
        let instance = &self.0._source.instance;
        if instance.owner.owner() != owner.owner()
           || address == 0
           || address & 1 != 0
           || !instance.executable.iter().any(|range| range.contains(&(address as u64)))
        {
            return Err(ACCESS_DENIED);
        }
        Ok(())
    }
}
/// An atomic wait key identifies one real image instance and byte offset. Its
/// fields cannot be assembled from a caller's numbers or a reused CPU address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ApplicationAtomic32Key
{
    image:  Binding,
    offset: u64
}
/// Owns initialized execution backing until the scheduler releases the wait.
/// Permission admission concerns the actual segment, not reservation padding,
/// package bytes, or a claim of process-private page-table enforcement.
#[must_use = "retain the initialized image pin until atomic wait access ends"]
pub(crate) struct ApplicationAtomic32
{
    execution: ApplicationExecution,
    key:       ApplicationAtomic32Key,
    address:   usize
}
impl ApplicationAtomic32
{
    pub(crate) fn key(&self) -> ApplicationAtomic32Key
    {
        self.key
    }

    /// The architecture atomic-access boundary may use only this admitted
    /// aligned whole-word address while retaining this guard.
    pub(crate) fn address(&self) -> usize
    {
        self.address
    }

    /// Recheck at the scheduler turn. The wait service must independently
    /// check its own root closing state before parking or accessing the word.
    pub(crate) fn still_admitted(&self) -> bool
    {
        let instance = &self.execution.0._source.instance;
        instance.owner.accepting() && !instance.activity.retiring.load(Ordering::Acquire)
    }
}
pub(crate) struct Loaded
{
    binding:    Binding,
    entry:      usize,
    _execution: Execution
}

/// The registered alias reserves the real destination before this owner leaves
/// the manager. Loading drops before Read's final activity signal on refusal;
/// an interrupted write can never make its reservation look reclaimable.
/// No entry or execution pin exists until the writer has completed publication.
#[must_use = "finish the prepared write or leave its original alias to the owner walk"]
pub(crate) struct PreparedLoad
{
    loading: mac_kernel_model::image::Loading,
    read:    Read
}
impl PreparedLoad
{
    pub(crate) fn write_application(
        self, account: &crate::machine::scheduler::accounting::AllocationAuthority)
        -> Result<Loaded, LoadError<crate::toolbox::resources::loader::LoadError>>
    {
        let mapping = self.prepare_application_mapping(account, None).map_err(LoadError::Write)?;
        self.publish_mapping(mapping).map_err(|cause| LoadError::Write(cause.into()))
    }

    /// Only an actual prepared alias can become a pending recovery image.
    /// Original instance/account authority is retained, never reconstructed.
    pub(crate) fn into_recovery(
        self, permit: crate::allocator::recovery::Permit)
        -> Result<PendingRecoveryImage, (crate::machine::paging::modules::Error, Self)>
    {
        use crate::machine::paging::modules::Error;
        if self.read.instance.module
           || self.read.instance.owner.owner().kind()
              != mac_object_table::domain::DomainKind::Process
           || self.read.instance.allocation.owner() != self.read.instance.owner.owner()
        {
            return Err((Error::Owner, self));
        }
        if let Err(cause) = self.ensure_active()
        {
            return Err((cause, self));
        }
        Ok(PendingRecoveryImage::new(self, permit))
    }

    fn prepare_application_mapping(
        &self, account: &crate::machine::scheduler::accounting::AllocationAuthority,
        recovery: Option<&crate::allocator::recovery::Permit>)
        -> Result<Arc<crate::machine::paging::modules::Mapping>,
                  crate::toolbox::resources::loader::LoadError>
    {
        let (descriptor, bytes, relocations) =
            crate::toolbox::resources::loader::application_code(&self.read.source.resources)?;
        use crate::machine::paging::modules::Mapping;
        match recovery
        {
            Some(permit) =>
            {
                Mapping::prepare_recovery_application(&self.read.instance.owner, account, permit,
                                                      &descriptor, bytes, relocations,
                                                      self.read.instance.base)
            }
            None =>
            {
                Mapping::prepare(&self.read.instance.owner, account, &descriptor, bytes,
                                 relocations, self.read.instance.base)
            }
        }.map_err(Into::into)
    }

    /// The alias already reserves this execution VA. Initialize private real
    /// pages, anchor their actual owner, then publish PTEs and both fences with
    /// no IMAGES/Runtime guard held. Refusal leaves this same alias to its walk.
    pub(crate) fn write_module(self,
                               account:&crate::machine::scheduler::accounting::AllocationAuthority,
                               code: &mac_formats::code::NativeCode)
                               -> Result<Loaded, crate::machine::paging::modules::Error>
    {
        use crate::machine::paging::modules::{Error, Mapping};
        if !self.read.instance.module
        {
            return Err(Error::Owner);
        }
        let mapping = Mapping::prepare(&self.read.instance.owner, account, &code.descriptor,
                                       &code.code, &code.relocations, self.read.instance.base)?;
        self.publish_mapping(mapping)
    }

    fn publish_mapping(self, mapping: Arc<crate::machine::paging::modules::Mapping>)
                       -> Result<Loaded, crate::machine::paging::modules::Error>
    {
        self.anchor_mapping(&mapping)?;
        self.publish_anchored()?;
        self.finish_mapping().map_err(|(cause, prepared)| {
                                 drop(prepared);
                                 cause
                             })
    }

    fn anchor_mapping(&self, mapping: &Arc<crate::machine::paging::modules::Mapping>)
                      -> Result<(), crate::machine::paging::modules::Error>
    {
        if !Arc::ptr_eq(self.read.instance.mapping.call_once(|| mapping.clone()), mapping)
        {
            return Err(crate::machine::paging::modules::Error::State);
        }
        self.ensure_active()
    }

    fn ensure_active(&self) -> Result<(), crate::machine::paging::modules::Error>
    {
        // A stopping walk may have deferred the alias before its anchor existed.
        // Arm the same retained Mapping; only its real retirement/ACK frees it.
        if self.read.instance.activity.retiring.load(Ordering::Acquire)
           || !self.read.instance.owner.accepting()
        {
            if let Some(mapping) = self.read.instance.mapping.get()
            {
                mapping.request_retirement();
            }
            return Err(crate::machine::paging::modules::Error::Closed);
        }
        Ok(())
    }

    fn publish_anchored(&self) -> Result<(), crate::machine::paging::modules::Error>
    {
        self.ensure_active()?;
        let mapping = self.read
                          .instance
                          .mapping
                          .get()
                          .ok_or(crate::machine::paging::modules::Error::State)?;
        let published = mapping.publish();
        let active = self.ensure_active();
        published?;
        active
    }

    fn finish_mapping(self) -> Result<Loaded, (crate::machine::paging::modules::Error, Self)>
    {
        use crate::machine::paging::modules::Error;
        if let Err(cause) = self.ensure_active()
        {
            return Err((cause, self));
        }
        // The real Mapping owns the checked relocated entry and reports it only
        // after successful publication. No late descriptor arithmetic/reparse.
        let Some(entry) = self.read
                              .instance
                              .mapping
                              .get()
                              .and_then(|mapping| mapping.entry())
                              .and_then(|entry| usize::try_from(entry).ok())
        else
        {
            return Err((Error::State, self));
        };
        let Self { loading,
                   read } = self;
        let execution = loading.finish();
        read.instance.initialized.store(true, Ordering::Release);
        Ok(Loaded { binding: read.binding,
                    entry,
                    _execution: Execution { _execution: execution,
                                            _source:    read } })
    }
}

impl Loaded
{
    pub(crate) fn into_module(self) -> Result<ModuleExecution, Status>
    {
        if !self._execution._source.instance.module
           || self._execution._source.instance.owner.owner().kind()
              != mac_object_table::domain::DomainKind::Module
        {
            return Err(ACCESS_DENIED);
        }
        Ok(ModuleExecution(self._execution))
    }

    pub(crate) fn user_read(&self) -> Result<Read, Status>
    {
        self._execution._source.retain()
    }

    pub(crate) fn range(&self) -> core::ops::Range<u64>
    {
        self._execution._source.instance.base..self._execution._source.instance.limit
    }

    pub(crate) fn binding(&self) -> Binding
    {
        self.binding
    }

    pub(crate) fn entry(&self) -> usize
    {
        self.entry
    }
}

/// Image ownership/admission can fail independently of writing its code.
/// Keep the writer's typed cause until the caller chooses its API boundary.
#[derive(Debug)]
pub(crate) enum LoadError<E>
{
    Admission(Status),
    Write(E)
}
impl<E> From<Status> for LoadError<E>
{
    fn from(cause: Status) -> Self
    {
        Self::Admission(cause)
    }
}
impl<E: core::fmt::Display> core::fmt::Display for LoadError<E>
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Admission(status) => write!(out, "image admission refused: status {status}"),
            Self::Write(cause) => cause.fmt(out)
        }
    }
}

fn prepare_instance(owner: &OwnerRef,
                    allocation: &crate::machine::scheduler::accounting::AllocationAuthority,
                    module: bool, base: u64, extent: u64, linked_origin: u64,
                    mut executable: Vec<core::ops::Range<u64>>,
                    mut writable: Vec<core::ops::Range<u64>>)
                    -> Result<Arc<Instance>, Status>
{
    for ranges in [&mut executable, &mut writable]
    {
        for range in ranges
        {
            let start = range.start.checked_sub(linked_origin).ok_or(BAD_ABI)?;
            let end = range.end.checked_sub(linked_origin).ok_or(BAD_ABI)?;
            if start >= end || end > extent
            {
                return Err(BAD_ABI);
            }
            *range =
                base.checked_add(start).ok_or(BAD_ABI)?..base.checked_add(end).ok_or(BAD_ABI)?;
        }
    }
    let activity =
        Arc::try_new(Activity { readers:  AtomicUsize::new(0),
                                retiring: AtomicBool::new(false) }).map_err(|_| NO_MEMORY)?;
    let instance = Arc::try_new(Instance { owner: owner.clone(), module, executable, writable,
            initialized: AtomicBool::new(false), mapping: spin::Once::new(), retirement_claimed:AtomicBool::new(false), base,
            limit: base.checked_add(extent).ok_or(NO_MEMORY)?,
            execution: mac_kernel_model::image::ExecutionGate::try_new().map_err(|_| NO_MEMORY)?, activity,
            binding: spin::Once::new(), allocation: allocation.clone() }).map_err(|_| NO_MEMORY)?;
    Ok(instance)
}

pub(crate) struct Manager
{
    sources: aliases::Store<Arc<Source>, InvocationGate, Arc<Instance>>
}
impl Manager
{
    pub(crate) fn new(system: OwnerRef, kinds: [KindLease; 4]) -> Result<Self, Status>
    {
        let [alias, backing, object, object_backing] = kinds;
        Ok(Self { sources: aliases::Store::new_prepared(system,
                                                        (alias, backing),
                                                        (object, object_backing)).map_err(status)? })
    }

    fn free_placement(&self, owner: mac_object_table::domain::DomainId, base: u64, extent: u64,
                      limit: u64)
                      -> bool
    {
        base.checked_add(extent).is_some_and(|end| {
                                    end <= limit
                                    && !self.sources.retained().any(|(_, instance)| {
                                                                   (instance.module
                                                                    || instance.owner.owner()
                                                                       == owner)
                                                                   && base < instance.limit
                                                                   && instance.base < end
                                                               })
                                })
    }

    fn placement(&self, owner: mac_object_table::domain::DomainId, preferred: u64, extent: u64,
                 relocation_base: u64, limit: u64)
                 -> Result<u64, Status>
    {
        let base = if self.free_placement(owner, preferred, extent, limit)
        {
            preferred
        }
        else
        {
            let mut candidate = relocation_base;
            loop
            {
                let end =
                    candidate.checked_add(extent).filter(|end| *end <= limit).ok_or(NO_MEMORY)?;
                let next = self.sources
                               .retained()
                               .filter(|(_, i)| {
                                   (i.module || i.owner.owner() == owner)
                                   && candidate < i.limit
                                   && i.base < end
                               })
                               .map(|(_, i)| i.limit)
                               .max();
                match next
                {
                    Some(next) => candidate = next,
                    None => break candidate
                }
            }
        };
        Ok(base)
    }

    pub(crate) fn read(&self, binding: Binding) -> Result<Read, Status>
    {
        let (source, instance) = self.sources.completing(&binding.0).ok_or(NOT_FOUND)?;
        Self::retain(source, instance, binding)
    }

    pub(crate) fn public_read(&self, id: u64) -> Result<Read, Status>
    {
        let (_, source, instance) = self.sources.get_with_owner(id).ok_or(NOT_FOUND)?;
        // Validate a live public alias before borrowing its original admission
        // receipt. A raw ID never mints completion authority for a retired slot.
        let usage = instance.activity.retain()?;
        Ok(Read { source: source.clone(),
                  instance: instance.clone(),
                  binding: *instance.binding.get().ok_or(NOT_FOUND)?,
                  usage })
    }

    fn retain(source: &Arc<Source>, instance: &Arc<Instance>, binding: Binding)
              -> Result<Read, Status>
    {
        let usage = instance.activity.retain()?;
        Ok(Read { source: source.clone(),
                  instance: instance.clone(),
                  binding,
                  usage })
    }

    pub(crate) fn root_only(owner: &OwnerRef) -> bool
    {
        !owner.has_resources_outside(&ROOT_KINDS)
    }

    pub(crate) fn execute_application(&self, owner: &OwnerRef, binding: Binding)
                                      -> Result<ApplicationExecution, Status>
    {
        let read = self.read(binding)?;
        if read.instance.module
           || !owner.accepting()
           || owner.owner().kind() != mac_object_table::domain::DomainKind::Process
           || read.instance.owner.owner() != owner.owner()
        {
            return Err(ACCESS_DENIED);
        }
        // SERVICE_BUSY is an unpublished/loading image, never a code pointer
        // validation failure. A refused writer cannot reopen execution.
        if !read.instance.initialized.load(Ordering::Acquire)
        {
            return Err(SERVICE_BUSY);
        }
        let pin = read.instance.execution.pin().map_err(|_| SERVICE_BUSY)?;
        Ok(ApplicationExecution(Execution { _execution: pin,
                                            _source:    read }))
    }

    pub(crate) fn pin_application_atomic32(&self, owner: &OwnerRef, binding: Binding,
                                           address: usize)
                                           -> Result<ApplicationAtomic32, Status>
    {
        if address == 0
           || !address.is_multiple_of(core::mem::align_of::<core::sync::atomic::AtomicU32>())
        {
            return Err(BAD_ARGUMENT);
        }
        let start = address as u64;
        let end = address.checked_add(core::mem::size_of::<u32>()).ok_or(BAD_ARGUMENT)? as u64;
        let execution = self.execute_application(owner, binding)?;
        let instance = &execution.0._source.instance;
        if !instance.writable.iter().any(|range| start >= range.start && end <= range.end)
        {
            return Err(ACCESS_DENIED);
        }
        let offset = start.checked_sub(instance.base).ok_or(BAD_ABI)?;
        let cpu = execution.0._source.user_alias(address, core::mem::size_of::<u32>(), true)?;
        Ok(ApplicationAtomic32 { key: ApplicationAtomic32Key { image: execution.0
                                                                               ._source
                                                                               .binding,
                                                               offset },
                                 execution,
                                 address: cpu })
    }

    pub(crate) fn execute_module(&self, binding: Binding) -> Result<ModuleExecution, Status>
    {
        let read = self.read(binding)?;
        if !read.instance.module
           || read.instance.owner.owner().kind() != mac_object_table::domain::DomainKind::Module
        {
            return Err(ACCESS_DENIED);
        }
        if !read.instance.initialized.load(Ordering::Acquire)
        {
            return Err(SERVICE_BUSY);
        }
        let pin = read.instance.execution.pin().map_err(|_| SERVICE_BUSY)?;
        Ok(ModuleExecution(Execution { _execution: pin,
                                       _source:    read }))
    }

    pub(crate) fn mapping_retirement(&self, after: u64) -> Option<MappingRetirement>
    {
        let instance = self.sources
                           .retained()
                           .map(|(_, instance)| instance)
                           .filter(|instance| {
                               instance.owner.owner().incarnation() > after
                               && !instance.owner.accepting()
                               && Self::root_only(&instance.owner)
                               && instance.activity.readers.load(Ordering::SeqCst) == 0
                               && !instance.retirement_claimed.load(Ordering::Acquire)
                               && instance.mapping.get().is_some_and(|mapping| mapping.ready())
                           })
                           .min_by_key(|instance| instance.owner.owner().incarnation())?;
        instance.retirement_claimed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .ok()?;
        let exclusion = match instance.execution.begin_load()
        {
            Ok(exclusion) => exclusion,
            Err(_) =>
            {
                instance.retirement_claimed.store(false, Ordering::Release);
                return None;
            }
        };
        Some(MappingRetirement { exclusion: Some(exclusion),
                                 owner:     instance.owner.owner().incarnation(),
                                 instance:  Some(instance.clone()),
                                 active:    true })
    }

    pub(crate) fn module_dma_address(&self, cpu: u64, bytes: u64)
                                     -> Result<u64, crate::machine::paging::modules::Error>
    {
        use crate::machine::paging::modules::Error;
        let end = cpu.checked_add(bytes).ok_or(Error::DmaSpan { address: cpu,
                                                                 bytes })?;
        let instance =
            self.sources
                .retained()
                .map(|(_, instance)| instance)
                .find(|instance| instance.module && cpu >= instance.base && end <= instance.limit)
                .ok_or(Error::Owner)?;
        instance.mapping.get().ok_or(Error::State)?.dma_address(cpu, bytes)
    }

    pub(crate) fn finish_module_retiring(&mut self) -> usize
    {
        self.sources.finish_deferred(|instance| {
                        instance.module
                        && Arc::strong_count(instance) == 1
                        && !instance.retirement_claimed.load(Ordering::Acquire)
                        && instance.mapping.get().is_none_or(|mapping| mapping.settled())
                        && !instance.owner.accepting()
                        && Self::root_only(&instance.owner)
                        && instance.activity.readers.load(Ordering::SeqCst) == 0
                    })
    }

    pub(crate) fn finish_retiring(&mut self) -> usize
    {
        self.sources.finish_deferred(|instance| {
                        !instance.module
                        && Arc::strong_count(instance) == 1
                        && !instance.retirement_claimed.load(Ordering::Acquire)
                        && instance.mapping.get().is_none_or(|mapping| mapping.settled())
                        && !instance.owner.accepting()
                        && Self::root_only(&instance.owner)
                        && instance.activity.readers.load(Ordering::SeqCst) == 0
                    })
    }

    pub(crate) fn release(&mut self, ticket: ReleasedResource, registration: Registration)
                          -> Result<(), Failure>
    {
        match registration
        {
            Registration::Alias =>
            {
                if let Some((_, instance)) = self.sources.releasing(&ticket)
                {
                    if !instance.activity.retiring.swap(true, Ordering::AcqRel)
                    {
                        // Publish invalidity before the coalesced, lock-free
                        // scheduler notification. Wait maintenance owns the
                        // actual wake/cancel decision and heavy pin release.
                        crate::machine::scheduler::address_wait_changed();
                    }
                }
                if let Some((_, instance)) = self.sources.releasing(&ticket)
                   && let Some(mapping) = instance.mapping.get()
                   && (!mapping.settled()
                       || instance.retirement_claimed.load(Ordering::Acquire)
                       || Arc::strong_count(instance) != 1)
                {
                    mapping.request_retirement();
                    return Err(Failure::module_mapping(ticket, mapping.refusal()));
                }
                self.sources.defer_alias(ticket)
            }
            Registration::Backing => self.sources.release_alias_storage(ticket),
            Registration::Object => self.sources.release_object(ticket),
            Registration::ObjectBacking => self.sources.release_object_storage(ticket)
        }.map_err(Into::into)
    }
}

/// One real exclusion/anchor claim; original cleanup ticket remains in the
/// canonical walk. No physical owner is detached into a stack-only obligation.
pub(crate) struct MappingRetirement
{
    exclusion: Option<mac_kernel_model::image::Loading>,
    instance:  Option<Arc<Instance>>,
    owner:     u64,
    active:    bool
}
impl MappingRetirement
{
    pub(crate) fn owner(&self) -> u64
    {
        self.owner
    }

    pub(crate) fn run(mut self) -> Result<(), crate::machine::paging::modules::Error>
    {
        self.active = false;
        let instance =
            self.instance.as_ref().ok_or(crate::machine::paging::modules::Error::State)?;
        let mapping =
            instance.mapping.get().ok_or(crate::machine::paging::modules::Error::State)?;
        // Actual execution exclusion is held and every non-image ledger item
        // has drained. Cleanup callbacks remain possible before this point;
        // after it no future gate may republish an unmapped/quarantined entry.
        instance.initialized.store(false, Ordering::Release);
        mapping.retire()
    }
}
impl Drop for MappingRetirement
{
    fn drop(&mut self)
    {
        drop(self.exclusion.take());
        if let Some(instance) = self.instance.take()
        {
            instance.retirement_claimed.store(false, Ordering::Release);
            drop(instance);
        }
        if self.active
        {
            crate::machine::paging::modules::changed();
        }
        crate::drivers::recovery_changed();
        crate::toolbox::processes::retirement::resources_released();
    }
}

fn status(error: aliases::Error) -> Status
{
    match error {
        aliases::Error::Pool(error) => crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error)),
        aliases::Error::Missing {..} | aliases::Error::TargetGone {..} => NOT_FOUND,
        aliases::Error::Foreign {..} => ACCESS_DENIED,
        aliases::Error::ReferenceLimit {..} => NO_MEMORY,
        aliases::Error::NotSystem {..} => BAD_ABI
    }
}
#[derive(Clone, Copy)]
pub(crate) enum Registration
{
    Alias,
    Backing,
    Object,
    ObjectBacking
}
impl Registration
{
    pub(crate) const ALL: [Self; 4] =
        [Self::Alias, Self::Backing, Self::Object, Self::ObjectBacking];

    pub(crate) fn specification(self) -> Specification<'static>
    {
        let (kind, label) = match self
        {
            Self::Alias => (*b"IMAG", "Execution image aliases"),
            Self::Backing => (*b"IMST", "Image alias backing"),
            Self::Object => (*b"IMOB", "Shared immutable image sources"),
            Self::ObjectBacking => (*b"IMOS", "Image source backing")
        };
        Specification { kind: KindCode(kind),
                        label,
                        provider: "System Image Manager",
                        capacity: CapacityPolicy::GrowableUntilMemory,
                        source: RegistrationSource::Resident }
    }
}
impl Release<dyn crate::toolbox::handles::cleanup::context::Context, Failure> for Registration
{
    fn release(&self, _: &mut (dyn crate::toolbox::handles::cleanup::context::Context + 'static),
               ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        crate::kernel::release_image(ticket, *self)
    }
}
