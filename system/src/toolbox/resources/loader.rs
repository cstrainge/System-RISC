//! Native application/code-resource loading with shared automatic translation routing.
use alloc::vec::Vec;
use core::fmt;

use mac_abi::{BAD_ARGUMENT, DefinitionEntry, NOT_FOUND, Status};
use mac_formats::{Error, Result,
                  code::{CodeRegistry, NoTranslator},
                  native::{Descriptor, Role, image},
                  resource::Resource,
                  text};

/// Format refusal and executable publication failure retain distinct identity.
/// The trap boundary deliberately chooses numeric Status; loading callers keep
/// the original transport cause until that boundary or the startup diagnostic.
#[derive(Debug)]
pub(crate) enum LoadError
{
    Format(Error),
    Publication(crate::machine::monitor::Error)
}
impl fmt::Display for LoadError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Format(cause) => cause.fmt(out),
            Self::Publication(cause) => write!(out, "executable publication refused: {cause}")
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
impl From<crate::machine::monitor::Error> for LoadError
{
    fn from(cause: crate::machine::monitor::Error) -> Self
    {
        Self::Publication(cause)
    }
}
pub(crate) type LoadResult<T> = core::result::Result<T, LoadError>;

pub fn read_startup(name: &str) -> Result<Vec<Resource>>
{
    crate::kernel::with_files(|files| {
        // Both startup packages become immutable System registry sources.
        crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
            files.startup_resources(name, crate::kernel::heap_available())
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
       || !application_region(descriptor.load_base, descriptor.load_limit)
    {
        return Err(Error::Malformed("application reservation overlaps resident images"));
    }
    let code = resources.iter()
                        .find(|r| r.kind == *b"RV64" && r.id == 0)
                        .ok_or(Error::Missing("application executable resource is missing"))?;
    let image = image::decode(&code.data, &descriptor)?;
    Ok((descriptor, &code.data, image))
}
pub fn application_extent(resources: &[Resource]) -> Result<u64>
{
    let (descriptor, _, _) = application_parts(resources)?;
    Ok(descriptor.load_limit - descriptor.load_base)
}
pub(crate) fn application_at(resources: &[Resource], destination: u64) -> LoadResult<usize>
{
    let (descriptor, bytes, _) = application_parts(resources)?;
    let extent = descriptor.load_limit - descriptor.load_base;
    let end = destination.checked_add(extent)
                         .ok_or(Error::Malformed("application destination overflow"))?;
    if destination & 4095 != 0 || !application_region(destination, end)
    {
        return Err(Error::Malformed("application destination overlaps resident images").into());
    }
    let table = resources.iter()
                         .find(|r| r.kind == *b"RLOC" && r.id == 0)
                         .ok_or(Error::Missing("native relocation metadata is missing"))?;
    load(&descriptor, bytes, Some(&table.data), destination)
}

fn load(descriptor: &Descriptor, bytes: &[u8], relocations: Option<&[u8]>, destination: u64)
        -> LoadResult<usize>
{
    let plan = image::LoadPlan::new(bytes, descriptor, relocations, destination)?;
    // The image registry/definition allocator owns this disjoint RAM reservation.
    let output = unsafe { core::slice::from_raw_parts_mut(destination as *mut u8, plan.length()) };
    plan.write(output)?;
    // Entries remain unpublished until the architecture has synchronized
    // instructions on every admitted participant, or refused with its typed
    // cause. A loading-hart fence alone does not publish executable code.
    crate::machine::monitor::publish_instructions()?;
    Ok(plan.entry() as usize)
}

struct Binding
{
    kind:    [u8; 4],
    id:      i16,
    origin:  u64,
    base:    u64,
    limit:   u64,
    entry:   DefinitionEntry,
    _source: Option<crate::images::Read>
}
pub struct Definitions
{
    registry: CodeRegistry,
    bindings: Vec<Binding>
}
impl Definitions
{
    pub fn new(_files: &'static super::manager::Files) -> Self
    {
        Self { registry: CodeRegistry::new(),
               bindings: Vec::new() }
    }

    pub(crate) fn evict_origin(&mut self, origin: u64)
    {
        self.bindings.retain(|binding| binding.origin != origin);
    }

    pub(crate) fn resolve(&mut self, kind: [u8; 4], id: i16) -> LoadResult<DefinitionEntry>
    {
        // A process-resource cleanup wrapper can nest inside the resident
        // service scope. The genuine Activity still selects System definitions.
        // Ordinary window invocation clears Activity before reaching here.
        if let Some(activity) = crate::context::recovery_activity()
        {
            let source =
                activity.resource_source()
                        .map_err(|_| {
                            Error::Malformed("System UI resource authority is unavailable")
                        })?;
            return self.resolve_system(source, kind, id);
        }
        self.resolve_selected(kind, id)
    }

    pub(crate) fn resolve_system(&mut self, source: crate::kernel::SystemResources, kind: [u8; 4],
                                 id: i16)
                                 -> LoadResult<DefinitionEntry>
    {
        source.with(|| self.resolve_selected(kind, id))
              .map_err(|_| Error::Malformed("System UI resource scope is unavailable"))?
    }

    fn resolve_selected(&mut self, kind: [u8; 4], id: i16) -> LoadResult<DefinitionEntry>
    {
        let lookup = crate::kernel::definition_resource_scope();
        crate::kernel::with_resources_in(lookup, |manager| {
            let (scope, resource) =
                manager.get(lookup,
                            mac_abi::ResourceKey { kind,
                                                   id })
                       .map_err(|_| Error::Missing("definition resource is missing"))?;
            let origin = scope.image_id().unwrap_or(0);
            if let Some(binding) =
                self.bindings.iter().find(|b| b.kind == kind && b.id == id && b.origin == origin)
            {
                return Ok(binding.entry);
            }
            // The shared definition loader owns code/relocation staging,
            // independently of the application which first requested it.
            let code =
                crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                    self.registry.prepare(resource, origin, &mut NoTranslator)
                })?;
            let d = &code.descriptor;
            let extent = d.load_limit - d.load_base;
            if extent
               > crate::machine::memory::DEFINITION_LIMIT - crate::machine::memory::DEFINITION_BASE
               || extent & 4095 != 0
               || d.load_base & 4095 != 0
            {
                return Err(Error::Malformed("invalid code resource reservation").into());
            }
            // Definition resources are independently relocatable modules. Allocate
            // top-down rather than depending on their build-time preferred addresses.
            let mut base = crate::machine::memory::DEFINITION_LIMIT - extent;
            loop
            {
                if base < crate::machine::memory::DEFINITION_BASE
                {
                    return Err(Error::BudgetExceeded("code resource address space exhausted").into());
                }
                let overlap =
                    self.bindings.iter().find(|b| base < b.limit && b.base < base + extent);
                let Some(other) = overlap
                else
                {
                    break;
                };
                base = other.base
                            .checked_sub(extent)
                            .ok_or(Error::Malformed("code resource placement overflow"))?;
            }
            // Reserve publication storage and retain the source before touching
            // executable memory. Refusal leaves no half-published definition.
            crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                self.bindings.try_reserve(1)
            }).map_err(|_| Error::AllocationFailed("definition binding allocation failed"))?;
            let source = if origin == 0
            {
                None
            }
            else
            {
                manager.retain_origin()
                       .map_err(|_| Error::AllocationFailed("definition source pin refused"))?
            };
            let address = load(d, &code.code, Some(&code.relocations), base)?;
            let entry: DefinitionEntry = unsafe { core::mem::transmute(address) };
            self.bindings.push(Binding { kind,
                                         id,
                                         origin,
                                         base,
                                         limit: base + extent,
                                         entry,
                                         _source: source });
            serial!("CODE_LOADED {}{} entry=0x{:x} origin={} linked=0x{:x} actual=0x{:x}",
                    text::decode(&kind),
                    id,
                    address,
                    origin,
                    d.load_base,
                    base);
            Ok(entry)
        }).map_err(|_| Error::Missing("selected definition resource catalog is unavailable"))?
    }
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
        LoadError::Format(Error::Malformed(_)) => BAD_ARGUMENT,
        LoadError::Publication(cause) => cause.status()
    }
}

pub fn application_region(base: u64, limit: u64) -> bool
{
    crate::machine::memory::application_region(base, limit)
}
pub fn reservation(resources: &[Resource]) -> Result<(u64, u64)>
{
    let resource = resources.iter()
                            .find(|r| r.kind == *b"NABI" && r.id == 0)
                            .ok_or(Error::Missing("application ABI resource is missing"))?;
    let d = Descriptor::decode(&resource.data)?;
    if d.role != Role::Application || !application_region(d.load_base, d.load_limit)
    {
        return Err(Error::Malformed("application reservation overlaps resident images"));
    }
    Ok((d.load_base, d.load_limit))
}
