//! Native application/code-resource loading with shared automatic translation routing.
use alloc::vec::Vec;
use core::fmt;

use mac_abi::{BAD_ARGUMENT, NOT_FOUND, Status};
use mac_formats::{Error, Result,
                  code::{CodeRegistry, NoTranslator},
                  native::{Descriptor, Role, image},
                  resource::Resource};

use super::definitions::{self, DefinitionRef};

/// Format refusal and executable publication failure retain distinct identity.
/// The trap boundary deliberately chooses numeric Status; loading callers keep
/// the original transport cause until that boundary or the startup diagnostic.
#[derive(Debug)]
pub(crate) enum LoadError
{
    Format(Error),
    Mapping(crate::machine::paging::modules::Error),
    Definition(definitions::Error)
}
impl fmt::Display for LoadError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Format(cause) => cause.fmt(out),
            Self::Mapping(cause) => cause.fmt(out),
            Self::Definition(cause) => cause.fmt(out)
        }
    }
}
impl From<Error> for LoadError
{
    fn from(cause: Error) -> Self
    {
        Self::Format(cause)
    }
}
impl From<crate::machine::paging::modules::Error> for LoadError
{
    fn from(cause: crate::machine::paging::modules::Error) -> Self
    {
        Self::Mapping(cause)
    }
}
impl From<definitions::Error> for LoadError
{
    fn from(cause: definitions::Error) -> Self
    {
        Self::Definition(cause)
    }
}
pub(crate) type LoadResult<T> = core::result::Result<T, LoadError>;

pub fn read_startup(name: &str) -> Result<Vec<Resource>>
{
    read_startup_application(name).map(|loaded| loaded.resources)
}

pub(crate) fn read_startup_application(name: &str)
                                       -> Result<crate::toolbox::files::application::Loaded>
{
    crate::kernel::with_files(|files| {
        // Both startup packages become immutable System registry sources.
        crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
            files.startup_application(name, crate::kernel::heap_available())
        })
    }).map_err(|error| {
          serial!("STARTUP_RESOURCE_SERVICE_REFUSED name={} cause={:?}", name, error);
          Error::Malformed("startup filesystem service is unavailable")
      })?
      .map_err(|status| {
          serial!("STARTUP_RESOURCE_LOAD_REFUSED name={} status={} heap_available={}",
                  name,
                  status,
                  crate::kernel::heap_available());
          match status
          {
              mac_abi::NO_MEMORY =>
              {
                  Error::AllocationFailed("startup resource budget or allocation exhausted")
              }
              mac_abi::NOT_FOUND => Error::Missing("startup file or resource is missing"),
              mac_abi::TRANSLATION_REQUIRED =>
              {
                  Error::TranslationRequired("startup code requires translation")
              }
              _ =>
              {
                  Error::Malformed("startup resource loading failed; see the named file and \
                                    status in the boot log")
              }
          }
      })
}

fn application_parts(resources: &[Resource]) -> Result<(Descriptor, &[u8], image::Image)>
{
    let descriptor = resources.iter()
                              .find(|r| r.kind == *b"NABI" && r.id == 0)
                              .ok_or(Error::Missing("application ABI resource is missing"))?;
    let descriptor = Descriptor::decode(&descriptor.data)?;
    if descriptor.role != Role::Application
       || descriptor.load_base != crate::machine::memory::VM_APPLICATION_IMAGE_BASE
       || !application_region(descriptor.load_base, descriptor.load_limit)
       || !descriptor.load_limit.is_multiple_of(crate::machine::memory::PAGE_BYTES)
    {
        return Err(Error::Malformed("application link layout is incompatible; rebuild with the \
                                     current SDK"));
    }
    let code = resources.iter()
                        .find(|r| r.kind == *b"RV64" && r.id == 0)
                        .ok_or(Error::Missing("application executable resource is missing"))?;
    let image = image::decode(&code.data, &descriptor)?;
    Ok((descriptor, &code.data, image))
}
/// Range identity is derived once from the checked segment stream. Executable
/// callbacks need file bytes; atomic data may also occupy actual writable BSS.
/// The registry rebases these ranges once with the actual image placement.
pub(crate) struct ApplicationLayout
{
    pub(crate) preferred:  u64,
    pub(crate) extent:     u64,
    pub(crate) executable: Vec<core::ops::Range<u64>>,
    pub(crate) writable:   Vec<core::ops::Range<u64>>
}
pub(crate) fn application_layout(resources: &[Resource]) -> Result<ApplicationLayout>
{
    let (descriptor, _, image) = application_parts(resources)?;
    let mut executable = Vec::new();
    let mut writable = Vec::new();
    for ranges in [&mut executable, &mut writable]
    {
        ranges.try_reserve_exact(image.segments.len()).map_err(|_| {
                                                           Error::AllocationFailed("application \
                                                                                    image range \
                                                                                    allocation \
                                                                                    failed")
                                                       })?;
    }
    for segment in &image.segments
    {
        let start = descriptor.load_base + segment.offset;
        if segment.flags & image::EXECUTE != 0 && segment.length != 0
        {
            executable.push(start..start + segment.length as u64);
        }
        if segment.flags & image::WRITE != 0
        {
            writable.push(start..start + segment.memory_length);
        }
    }
    Ok(ApplicationLayout { preferred: descriptor.load_base,
                           extent: descriptor.load_limit - descriptor.load_base,
                           executable,
                           writable })
}
pub(crate) fn application_code(resources: &[Resource]) -> Result<(Descriptor, &[u8], &[u8])>
{
    let (descriptor, bytes, _) = application_parts(resources)?;
    let table = resources.iter()
                         .find(|r| r.kind == *b"RLOC" && r.id == 0)
                         .ok_or(Error::Missing("native relocation metadata is missing"))?;
    Ok((descriptor, bytes, &table.data))
}

/// Native definition storage is initialized once by real System bootstrap.
/// Caller staging must finish structural/owner validation before cold prepare.
pub(crate) fn initialize_definitions() -> LoadResult<()>
{
    definitions::initialize().map_err(LoadError::Definition)
}
fn family(kind: [u8; 4], id: i16) -> LoadResult<()>
{
    if CodeRegistry::new().convention(kind, id).is_some()
    {
        Ok(())
    }
    else
    {
        Err(Error::Malformed("resource type is not declared executable").into())
    }
}
pub(crate) fn cached_definition(kind: [u8; 4], id: i16) -> LoadResult<Option<DefinitionRef>>
{
    family(kind, id)?;
    if let Some(activity) = crate::context::recovery_activity()
    {
        let source =
            activity.resource_source()
                    .map_err(|_| Error::Malformed("System UI resource authority unavailable"))?;
        return cached_system_definition(source, kind, id);
    }
    cached_selected(kind, id)
}
pub(crate) fn cached_system_definition(source: crate::kernel::SystemResources, kind: [u8; 4],
                                       id: i16)
                                       -> LoadResult<Option<DefinitionRef>>
{
    family(kind, id)?;
    source.with(|| cached_selected(kind, id))
          .map_err(|_| Error::Malformed("System definition source unavailable"))?
}
fn cached_selected(kind: [u8; 4], id: i16) -> LoadResult<Option<DefinitionRef>>
{
    let lookup = crate::kernel::definition_resource_scope();
    crate::kernel::with_resources_in(lookup, |manager| {
        let (scope, _) = manager.get(lookup,
                                     mac_abi::ResourceKey { kind,
                                                            id })
                                .map_err(|_| Error::Missing("definition resource is missing"))?;
        definitions::cached(kind, id, scope.image_id().unwrap_or(0)).map_err(LoadError::Definition)
    }).map_err(|_| Error::Missing("selected definition catalog unavailable"))?
}
/// Decoding, page acquisition and real publication ACKs run here, outside
/// Desktop/definition-manager guards. No author callback is part of this phase.
pub(crate) fn prepare_definition(kind: [u8; 4], id: i16) -> LoadResult<DefinitionRef>
{
    family(kind, id)?;
    if let Some(activity) = crate::context::recovery_activity()
    {
        let source =
            activity.resource_source()
                    .map_err(|_| Error::Malformed("System UI resource authority unavailable"))?;
        return prepare_system_definition(source, kind, id);
    }
    prepare_selected(select_definition(kind, id)?, kind, id)
}
pub(crate) fn prepare_system_definition(source: crate::kernel::SystemResources, kind: [u8; 4],
                                        id: i16)
                                        -> LoadResult<DefinitionRef>
{
    family(kind, id)?;
    let selected = source.with(|| select_definition(kind, id))
                         .map_err(|_| Error::Malformed("System definition source unavailable"))??;
    prepare_selected(selected, kind, id)
}
enum SelectedDefinition
{
    Ready(DefinitionRef),
    Source
    {
        origin: u64,
        source: definitions::SourceBinding
    }
}
/// This callback owns no IMAGES guard. Nevertheless only exact owned source
/// authority leaves it: neither a Manager/catalog borrow nor an unrelated
/// selected application Read remains across mapping/monitor waits.
fn select_definition(kind: [u8; 4], id: i16) -> LoadResult<SelectedDefinition>
{
    let lookup = crate::kernel::definition_resource_scope();
    crate::kernel::with_resources_in(lookup, |manager| {
        let (scope, _) = manager.get(lookup, mac_abi::ResourceKey { kind, id })
            .map_err(|_| Error::Missing("definition resource is missing"))?;
        let origin = scope.image_id().unwrap_or(0);
        if let Some(reference) = definitions::cached(kind, id, origin)?
        {
            return Ok(SelectedDefinition::Ready(reference));
        }
        let source = if origin == 0
        {
            let owner = crate::toolbox::handles::system_owner().map_err(definitions::Error::OwnerUnavailable)?;
            definitions::SourceBinding::System(crate::kernel::system_resources(&owner).map_err(definitions::Error::OwnerUnavailable)?)
        }
        else
        {
            let read = manager.retain_origin().map_err(|_| definitions::Error::Source)?.ok_or(definitions::Error::Source)?;
            if read.binding().id() != origin
            {
                return Err(definitions::Error::Source.into());
            }
            definitions::SourceBinding::Image(read)
        };
        Ok(SelectedDefinition::Source { origin, source })
    }).map_err(|_| Error::Missing("selected definition catalog unavailable"))?
}

/// Classification from genuine CHAIN provenance only. It performs no code
/// publication or manager operation, so trusted definitions retain their fast
/// path before the user callback dispatcher elects to handle a request.
pub(crate) fn process_definition(kind: [u8; 4], id: i16) -> LoadResult<bool>
{
    family(kind, id)?;
    if crate::context::recovery_activity().is_some()
    {
        return Ok(false);
    }
    match select_definition(kind, id)?
    {
        SelectedDefinition::Ready(reference) =>
        {
            definitions::is_user(reference).map_err(|cause| {
                                               definitions::Error::OwnerUnavailable(cause).into()
                                           })
        }
        SelectedDefinition::Source { source,
                                     .. } =>
        {
            Ok(source.owner().owner().kind() == mac_object_table::domain::DomainKind::Process)
        }
    }
}
fn prepare_selected(selected: SelectedDefinition, kind: [u8; 4], id: i16)
                    -> LoadResult<DefinitionRef>
{
    let (origin, source) = match selected
    {
        SelectedDefinition::Ready(reference) => return Ok(reference),
        SelectedDefinition::Source { origin,
                                     source } => (origin, source)
    };
    if let Some(reference) = definitions::finish_existing(kind, id, origin)?
    {
        return Ok(reference);
    }
    let target = source.allocation_target();
    crate::allocator::with_target(target, || {
        let code = match &source
        {
            definitions::SourceBinding::Image(read) =>
            {
                // Decode directly from the exact retained original source.
                // Manager is an immutable stack view, not a registry guard.
                let manager = super::manager::Manager::new(&[], &read.source().resources, &[]);
                let (_, resource) =
                    manager.get(mac_abi::resources::APPLICATION,
                                mac_abi::ResourceKey { kind,
                                                       id })
                           .map_err(|_| {
                               Error::Missing("retained definition resource is missing")
                           })?;
                CodeRegistry::new().prepare(resource, origin, &mut NoTranslator)?
            }
            definitions::SourceBinding::System(proof) =>
            {
                proof.with(|| {
                         crate::kernel::with_resources_in(mac_abi::resources::SYSTEM, |manager| {
                             let (_, resource) = manager.get(mac_abi::resources::SYSTEM,
                                                             mac_abi::ResourceKey { kind,
                                                                                    id })
                                                        .map_err(|_| {
                                                            Error::Missing("System definition \
                                                                            resource is missing")
                                                        })?;
                             CodeRegistry::new().prepare(resource, origin, &mut NoTranslator)
                         })
                     })
                     .map_err(|_| Error::Missing("System definition source unavailable"))?
                     .map_err(|_| Error::Missing("System definition catalog unavailable"))??
            }
        };
        // Owned NativeCode and original source authority remain. All resource
        // callback borrows have ended before physical publication/ACK.
        definitions::prepare(source, kind, id, code).map_err(LoadError::Definition)
    })
}

pub(crate) fn status(error: impl Into<LoadError>) -> Status
{
    match error.into()
    {
        LoadError::Format(Error::BudgetExceeded(_) | Error::AllocationFailed(_)) =>
        {
            mac_abi::NO_MEMORY
        }
        LoadError::Format(Error::Missing(_)) => NOT_FOUND,
        LoadError::Format(Error::TranslationRequired(_)) => mac_abi::TRANSLATION_REQUIRED,
        LoadError::Format(Error::Malformed(_) | Error::TextConversion(_)) => BAD_ARGUMENT,
        LoadError::Mapping(crate::machine::paging::modules::Error::Format(cause)) =>
        {
            status(LoadError::Format(cause))
        }
        LoadError::Mapping(cause) => cause.status(),
        LoadError::Definition(cause) => cause.status()
    }
}

pub fn application_region(base: u64, limit: u64) -> bool
{
    crate::machine::memory::application_region(base, limit)
}
