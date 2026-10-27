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
pub(crate) struct Source
{
    pub authority: u64,
    pub resources: Vec<mac_formats::resource::Resource>,
    pub creator:   [u8; 4],
    pub file_type: [u8; 4]
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
        }
    }
}
struct Instance
{
    owner:     OwnerRef,
    base:      u64,
    limit:     u64,
    execution: mac_kernel_model::image::ExecutionGate,
    activity:  Arc<Activity>,
    binding:   spin::Once<Binding>
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
pub(crate) struct Loaded
{
    binding:    Binding,
    entry:      usize,
    _execution: Execution
}
impl Loaded
{
    pub(crate) fn binding(&self) -> Binding
    {
        self.binding
    }

    pub(crate) fn entry(&self) -> usize
    {
        self.entry
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
    pub(crate) fn load(&mut self, owner: &OwnerRef, source: Arc<Source>, preferred: u64,
                       extent: u64, relocation_base: u64, limit: u64,
                       write: impl FnOnce(&[mac_formats::resource::Resource],
                              u64) -> Result<usize, Status>)
                       -> Result<Loaded, Status>
    {
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
        let activity =
            Arc::try_new(Activity { readers:  AtomicUsize::new(0),
                                    retiring: AtomicBool::new(false) }).map_err(|_| NO_MEMORY)?;
        let instance = Arc::try_new(Instance { owner: owner.clone(), base,
            limit: base.checked_add(extent).ok_or(NO_MEMORY)?,
            execution: mac_kernel_model::image::ExecutionGate::try_new().map_err(|_| NO_MEMORY)?, activity,
            binding: spin::Once::new() }).map_err(|_| NO_MEMORY)?;
        let (_, receipt) = self.sources
                               .insert_with_receipt(owner, source, instance.clone())
                               .map_err(status)?
                               .into_parts();
        let binding = Binding(receipt);
        instance.binding.call_once(|| binding);
        let result = (|| {
            let read = self.read(binding)?;
            let loading = read.instance.execution.begin_load().map_err(|_| SERVICE_BUSY)?;
            let entry = write(&read.source.resources, base)?;
            Ok(Loaded { binding,
                        entry,
                        _execution: Execution { _execution: loading.finish(),
                                                _source:    read } })
        })();
        // The process owns every published resource, including refused loads.
        // Its common walk retains the real alias/backing and completes cleanup.
        result
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

    pub(crate) fn next_retiring(&self, after: u64) -> Option<(u64, u64)>
    {
        self.sources
            .deferred_tags()
            .filter_map(|instance| {
                let owner = instance.owner.owner().incarnation();
                (owner > after && !instance.owner.accepting() && Self::root_only(&instance.owner))
                .then_some(owner)
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
                        !instance.owner.accepting()
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
            Self::Alias => (*b"IMAG", "Application image aliases"),
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
impl Release<crate::toolbox::runtime::Desktop, Failure> for Registration
{
    fn release(&self, _: &mut crate::toolbox::runtime::Desktop, ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        crate::kernel::release_image(ticket, *self)
    }
}
