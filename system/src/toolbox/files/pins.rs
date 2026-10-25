//! Executable references are process aliases of System-owned catalog pins.
//! Generation checks and last-alias release live in the shared facility.
#![forbid(unsafe_code)]
use mac_abi::*;
use mac_object_table::{aliases,
                       domain::ReleasedResource,
                       registry::{CleanupFailure, Release, Source, Specification}};

use crate::toolbox::handles::{CapacityPolicy, InvocationGate, KindCode, KindLease, OwnerRef,
                              cleanup::Failure};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct File
{
    pub(super) volume: u32,
    pub(super) id:     u32
}
pub(super) struct Pins
{
    aliases: Option<aliases::Store<File, InvocationGate>>
}
impl Pins
{
    pub(super) const fn new() -> Self
    {
        Self { aliases: None }
    }

    pub(super) fn prepare(&mut self, system: OwnerRef, kinds: [KindLease; 4])
                          -> Result<(), Status>
    {
        if self.aliases.is_none()
        {
            let [alias, backing, object, object_backing] = kinds;
            self.aliases = Some(aliases::Store::new(system,
                                                    (alias, backing),
                                                    (object, object_backing)).map_err(status)?);
        }
        Ok(())
    }

    pub(super) fn insert(&mut self, owner: &OwnerRef, volume: u32, id: u32) -> Result<(), Status>
    {
        let store = self.aliases.as_mut().ok_or(NOT_SUPPORTED)?;
        if store.iter().any(|(_, actual, _, _)| actual == owner.owner())
        {
            return Err(BAD_ABI);
        }
        store.insert(owner,
                     File { volume,
                            id },
                     ())
             .map(|_| ())
             .map_err(status)
    }

    pub(super) fn objects(&self) -> impl Iterator<Item = &File>
    {
        self.aliases.iter().flat_map(|store| store.objects())
    }

    pub(super) fn len(&self) -> usize
    {
        self.objects().count()
    }
}
fn status(error: aliases::Error) -> Status
{
    match error
    {
        aliases::Error::Pool(error) => crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error)),
        aliases::Error::Missing { .. } | aliases::Error::TargetGone { .. } => NOT_FOUND,
        aliases::Error::Foreign { .. } => ACCESS_DENIED,
        aliases::Error::ReferenceLimit { .. } => NO_MEMORY,
        aliases::Error::NotSystem { .. } => BAD_ABI
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
            Self::Alias => (*b"FPIN", "Executable aliases"),
            Self::Backing => (*b"FPST", "Executable alias backing"),
            Self::Object => (*b"FPOB", "Shared executable pins"),
            Self::ObjectBacking => (*b"FPOS", "Shared executable backing")
        };
        Specification { kind: KindCode(kind),
                        label,
                        provider: "System File Manager",
                        capacity: CapacityPolicy::GrowableUntilMemory,
                        source: Source::Resident }
    }
}
pub(crate) trait PinHost
{
    fn release_file_pin(&mut self, ticket: ReleasedResource, kind: Registration)
                        -> Result<(), Failure>;
}
impl<C: PinHost> Release<C, Failure> for Registration
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_file_pin(ticket, *self)
    }
}
impl PinHost for super::Manager
{
    fn release_file_pin(&mut self, ticket: ReleasedResource, kind: Registration)
                        -> Result<(), Failure>
    {
        let Some(store) = self.executables.aliases.as_mut()
        else
        {
            return Err(Failure::registration_mismatch(ticket));
        };
        match kind
        {
            Registration::Alias => store.release_alias(ticket),
            Registration::Backing => store.release_alias_storage(ticket),
            Registration::Object => store.release_object(ticket),
            Registration::ObjectBacking => store.release_object_storage(ticket)
        }.map_err(Into::into)
    }
}
impl super::Manager
{
    pub(crate) fn prepare_executable_pins(&mut self, system: OwnerRef, kinds: [KindLease; 4])
                                          -> Result<(), Status>
    {
        self.executables.prepare(system, kinds)
    }
}
