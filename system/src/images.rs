//! Registered immutable sources and private flat execution reservations.
#![forbid(unsafe_code)]
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use mac_abi::*;
use mac_object_table::{aliases,
                       domain::ReleasedResource,
                       registry::{Release, Source as RegistrationSource, Specification},
                       store::CompletionKey};

use crate::toolbox::handles::{CapacityPolicy, InvocationGate, KindCode, KindLease, OwnerRef,
                              cleanup::Failure};

pub(crate) const ROOT_KINDS: [KindCode; 2] = [KindCode(*b"IMAG"), KindCode(*b"IMST")];

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

#[derive(Clone, Copy)]
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
        }
    }
}
struct Instance
{
    owner:      OwnerRef,
    module:     bool,
    executable: Vec<core::ops::Range<u64>>,
    base:       u64,
    limit:      u64,
    execution:  mac_kernel_model::image::ExecutionGate,
    activity:   Arc<Activity>,
    binding:    spin::Once<Binding>
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

    /// Immutable package bytes can outlive this execution instance. A restart
    /// still receives a distinct destination and fresh writable image state.
    pub(crate) fn shared_source(&self) -> Arc<Source>
    {
        self.source.clone()
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
    /// Run the byte initialization/relocation/instruction-publication boundary
    /// without borrowing the image manager. The writer owns no alias ticket;
    /// the existing registered owner walk retains all published metadata even
    /// if writing or a concurrent owner stop refuses this load.
    pub(crate) fn write<E>(self,
                           write: impl FnOnce(&[mac_formats::resource::Resource],
                                  u64) -> Result<usize, E>)
                           -> Result<Loaded, LoadError<E>>
    {
        if !self.read.instance.owner.accepting()
        {
            return Err(LoadError::Admission(SERVICE_BUSY));
        }
        let entry =
            write(&self.read.source.resources, self.read.instance.base).map_err(LoadError::Write)?;
        // This avoids finishing a load already known to be stopped. It is not
        // atomic execution admission: normal spawn/module entry still checks
        // genuine authority, and the retained pin protects a later stop race.
        if !self.read.instance.owner.accepting()
        {
            return Err(LoadError::Admission(SERVICE_BUSY));
        }
        let Self { loading,
                   read } = self;
        Ok(Loaded { binding: read.binding,
                    entry,
                    _execution: Execution { _execution: loading.finish(),
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

pub(crate) struct Manager
{
    sources: aliases::Store<Arc<Source>, InvocationGate, Arc<Instance>>
}
impl Manager
{
    pub(crate) fn new(system: OwnerRef, kinds: [KindLease; 4]) -> Result<Self, Status>
    {
        let [alias, backing, object, object_backing] = kinds;
        Ok(Self { sources: aliases::Store::new(system, (alias, backing), (object, object_backing)).map_err(status)? })
    }

    /// Caller owns a genuine child ledger before any source is admitted. All
    /// fallible metadata allocation precedes executable-memory writes.
    pub(crate) fn prepare_load(&mut self, owner: &OwnerRef, source: Arc<Source>, preferred: u64,
                               extent: u64, relocation_base: u64, limit: u64,
                               mut executable: Vec<core::ops::Range<u64>>)
                               -> Result<PreparedLoad, Status>
    {
        let role_matches_owner =
            matches!((&source.role, owner.owner().kind()),
                     (Role::Application { .. }, mac_object_table::domain::DomainKind::Process)
                     | (Role::Module, mac_object_table::domain::DomainKind::Module));
        if !role_matches_owner
        {
            return Err(ACCESS_DENIED);
        }
        if !owner.accepting()
        {
            return Err(SERVICE_BUSY);
        }
        let free = |base: u64| {
            base.checked_add(extent).is_some_and(|end| {
                                        end <= limit
                                        && !self.sources
                                                .retained()
                                                .any(|(_, instance)| {
                                                    base < instance.limit && instance.base < end
                                                })
                                    })
        };
        let base = if free(preferred)
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
                               .filter(|(_, i)| candidate < i.limit && i.base < end)
                               .map(|(_, i)| i.limit)
                               .max();
                match next
                {
                    Some(next) => candidate = next,
                    None => break candidate
                }
            }
        };
        for range in &mut executable
        {
            let start = range.start.checked_sub(preferred).ok_or(BAD_ABI)?;
            let end = range.end.checked_sub(preferred).ok_or(BAD_ABI)?;
            if start >= end || end > extent
            {
                return Err(BAD_ABI);
            }
            *range =
                base.checked_add(start).ok_or(BAD_ABI)?..base.checked_add(end).ok_or(BAD_ABI)?;
        }
        let activity =
            Arc::try_new(Activity { readers:  AtomicUsize::new(0),
                                    retiring: AtomicBool::new(false) }).map_err(|_| NO_MEMORY)?;
        let instance = Arc::try_new(Instance { owner: owner.clone(), module: source.role == Role::Module, executable, base,
            limit: base.checked_add(extent).ok_or(NO_MEMORY)?,
            execution: mac_kernel_model::image::ExecutionGate::try_new().map_err(|_| NO_MEMORY)?, activity,
            binding: spin::Once::new() }).map_err(|_| NO_MEMORY)?;
        let (_, receipt) = self.sources
                               .insert_with_receipt(owner, source, instance.clone())
                               .map_err(status)?
                               .into_parts();
        let binding = Binding(receipt);
        instance.binding.call_once(|| binding);
        let read = self.read(binding)?;
        let loading = read.instance.execution.begin_load().map_err(|_| SERVICE_BUSY)?;
        // The process owns every published resource, including refused loads.
        // retained() includes live/pending/deferred aliases, so another load
        // still sees this destination while its writer is outside the manager.
        // Read/activity and Loading block actual reclamation/execution until
        // the preparation finishes or drops into that same owner-walk barrier.
        Ok(PreparedLoad { loading,
                          read })
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
        let pin = read.instance.execution.pin().map_err(|_| SERVICE_BUSY)?;
        Ok(ApplicationExecution(Execution { _execution: pin,
                                            _source:    read }))
    }

    pub(crate) fn execute_module(&self, binding: Binding) -> Result<ModuleExecution, Status>
    {
        let read = self.read(binding)?;
        if !read.instance.module
           || read.instance.owner.owner().kind() != mac_object_table::domain::DomainKind::Module
        {
            return Err(ACCESS_DENIED);
        }
        let pin = read.instance.execution.pin().map_err(|_| SERVICE_BUSY)?;
        Ok(ModuleExecution(Execution { _execution: pin,
                                       _source:    read }))
    }

    pub(crate) fn finish_module_retiring(&mut self) -> usize
    {
        self.sources.finish_deferred(|instance| {
                        instance.module
                        && !instance.owner.accepting()
                        && Self::root_only(&instance.owner)
                        && instance.activity.readers.load(Ordering::SeqCst) == 0
                    })
    }

    pub(crate) fn next_retiring(&self, after: u64) -> Option<(u64, u64)>
    {
        self.sources
            .deferred_tags()
            .filter_map(|instance| {
                let owner = instance.owner.owner().incarnation();
                (!instance.module
                 && owner > after
                 && !instance.owner.accepting()
                 && Self::root_only(&instance.owner)).then_some(owner)
            })
            .min()
            .and_then(|owner| {
                self.sources.deferred_tags().find_map(|instance| {
                                                (instance.owner.owner().incarnation() == owner)
                .then(|| instance.binding.get().map(|binding| (owner, binding.id()))).flatten()
                                            })
            })
    }

    pub(crate) fn finish_retiring(&mut self) -> usize
    {
        self.sources.finish_deferred(|instance| {
                        !instance.module
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
                    instance.activity.retiring.store(true, Ordering::SeqCst);
                }
                self.sources.defer_alias(ticket)
            }
            Registration::Backing => self.sources.release_alias_storage(ticket),
            Registration::Object => self.sources.release_object(ticket),
            Registration::ObjectBacking => self.sources.release_object_storage(ticket)
        }.map_err(Into::into)
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
