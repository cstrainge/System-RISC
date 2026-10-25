//! Native application/code-resource loading with shared automatic translation routing.
use alloc::vec::Vec;
use core::arch::asm;

use mac_abi::{BAD_ARGUMENT, DefinitionEntry, NOT_FOUND, Status};
use mac_formats::{Error, Result,
                  code::{CodeRegistry, NoTranslator},
                  native::{Descriptor, Role, image},
                  resource::Resource,
                  text};

pub fn read_startup(name: &str) -> Result<Vec<Resource>>
{
    crate::kernel::with_files(|files| {
        files.startup_resources(name, crate::kernel::heap_available())
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
pub fn application(resources: &[Resource]) -> Result<usize>
{
    let (descriptor, _, _) = application_parts(resources)?;
    application_at(resources, descriptor.load_base)
}
pub fn application_at(resources: &[Resource], destination: u64) -> Result<usize>
{
    let (descriptor, bytes, _) = application_parts(resources)?;
    let extent = descriptor.load_limit - descriptor.load_base;
    let end = destination.checked_add(extent)
                         .ok_or(Error::Malformed("application destination overflow"))?;
    if destination & 4095 != 0 || !application_region(destination, end)
    {
        return Err(Error::Malformed("application destination overlaps resident images"));
    }
    let table = resources.iter()
                         .find(|r| r.kind == *b"RLOC" && r.id == 0)
                         .ok_or(Error::Missing("native relocation metadata is missing"))?;
    load(&descriptor, bytes, Some(&table.data), destination)
}

fn load(descriptor: &Descriptor, bytes: &[u8], relocations: Option<&[u8]>, destination: u64)
        -> Result<usize>
{
    let plan = image::LoadPlan::new(bytes, descriptor, relocations, destination)?;
    // The image registry/definition allocator owns this disjoint RAM reservation.
    let output = unsafe { core::slice::from_raw_parts_mut(destination as *mut u8, plan.length()) };
    plan.write(output)?;
    unsafe {
        asm!("fence.i", options(nostack));
    }
    Ok(plan.entry() as usize)
}

struct Binding
{
    kind:   [u8; 4],
    id:     i16,
    origin: u32,
    base:   u64,
    limit:  u64,
    entry:  DefinitionEntry
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

    pub fn resolve(&mut self, kind: [u8; 4], id: i16) -> Result<DefinitionEntry>
    {
        let manager =
            crate::kernel::resources().ok_or(Error::Missing("resource catalog is missing"))?;
        let (scope, resource) =
            manager.get(mac_abi::resources::CHAIN,
                        mac_abi::ResourceKey { kind,
                                               id })
                   .map_err(|_| Error::Missing("definition resource is missing"))?;
        let origin = if scope == mac_abi::resources::APPLICATION
        {
            crate::context::current_thread_context().identity().app.0 as u32
        }
        else
        {
            0
        };
        if let Some(binding) =
            self.bindings.iter().find(|b| b.kind == kind && b.id == id && b.origin == origin)
        {
            return Ok(binding.entry);
        }
        let code = self.registry.prepare(resource, origin, &mut NoTranslator)?;
        let d = &code.descriptor;
        let extent = d.load_limit - d.load_base;
        if extent
           > crate::machine::memory::DEFINITION_LIMIT - crate::machine::memory::DEFINITION_BASE
           || extent & 4095 != 0
           || d.load_base & 4095 != 0
        {
            return Err(Error::Malformed("invalid code resource reservation"));
        }
        // Definition resources are independently relocatable modules. Allocate
        // top-down rather than depending on their build-time preferred addresses.
        let mut base = crate::machine::memory::DEFINITION_LIMIT - extent;
        loop
        {
            if base < crate::machine::memory::DEFINITION_BASE
            {
                return Err(Error::BudgetExceeded("code resource address space exhausted"));
            }
            let overlap = self.bindings.iter().find(|b| base < b.limit && b.base < base + extent);
            let Some(other) = overlap
            else
            {
                break;
            };
            base = other.base
                        .checked_sub(extent)
                        .ok_or(Error::Malformed("code resource placement overflow"))?;
        }
        let address = load(d, &code.code, Some(&code.relocations), base)?;
        let entry: DefinitionEntry = unsafe { core::mem::transmute(address) };
        self.bindings.push(Binding { kind,
                                     id,
                                     origin,
                                     base,
                                     limit: base + extent,
                                     entry });
        serial!("CODE_LOADED {}{} entry=0x{:x} origin={} linked=0x{:x} actual=0x{:x}",
                text::decode(&kind),
                id,
                address,
                origin,
                d.load_base,
                base);
        Ok(entry)
    }
}

pub fn status(error: Error) -> Status
{
    match error
    {
        Error::BudgetExceeded(_) | Error::AllocationFailed(_) => mac_abi::NO_MEMORY,
        Error::Missing(_) => NOT_FOUND,
        Error::TranslationRequired(_) => mac_abi::TRANSLATION_REQUIRED,
        Error::Malformed(_) => BAD_ARGUMENT
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
