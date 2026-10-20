//! Native application/code-resource loading with shared automatic translation routing.
use alloc::vec::Vec;
use core::arch::asm;

use mac_abi::{BAD_ARGUMENT, BootInfo, DefinitionEntry, NOT_FOUND, OK, Status};
use mac_formats::{Error, Result,
                  code::{CodeRegistry, NoTranslator},
                  native::{Descriptor, Role, image},
                  resource::{self, Resource},
                  text};

pub fn read_startup(info: &BootInfo, name: &str) -> Result<Vec<u8>>
{
    let read = info.read_startup.ok_or(Error("startup volume access is unavailable"))?;
    let mut length = 0usize;
    let status = unsafe {
        read(info.startup_context, name.as_ptr(), name.len(), core::ptr::null_mut(), 0, &mut length)
    };
    if status != OK || length == 0 || length > 4 * 1024 * 1024
    {
        return Err(Error("startup resource fork is missing or too large"));
    }
    let mut output = alloc::vec![0; length];
    let status = unsafe {
        read(info.startup_context,
             name.as_ptr(),
             name.len(),
             output.as_mut_ptr(),
             output.len(),
             &mut length)
    };
    if status != OK || length != output.len()
    {
        return Err(Error("startup resource fork read failed"));
    }
    Ok(output)
}

fn application_parts(resources: &[Resource]) -> Result<(Descriptor, &[u8], image::Image)>
{
    let descriptor = resources.iter()
                              .find(|r| r.kind == *b"NABI" && r.id == 0)
                              .ok_or(Error("application ABI resource is missing"))?;
    let descriptor = Descriptor::decode(&descriptor.data)?;
    if descriptor.role != Role::Application
       || !application_region(descriptor.load_base, descriptor.load_limit)
    {
        return Err(Error("application reservation overlaps resident images"));
    }
    let code = resources.iter()
                        .find(|r| r.kind == *b"RV64" && r.id == 0)
                        .ok_or(Error("application executable resource is missing"))?;
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
    let end = destination.checked_add(extent).ok_or(Error("application destination overflow"))?;
    if destination & 4095 != 0 || !application_region(destination, end)
    {
        return Err(Error("application destination overlaps resident images"));
    }
    let table = resources.iter()
                         .find(|r| r.kind == *b"RLOC" && r.id == 0)
                         .ok_or(Error("native relocation metadata is missing"))?;
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
        let manager = crate::kernel::resources().ok_or(Error("resource catalog is missing"))?;
        let (scope, resource) = manager.get(mac_abi::resources::CHAIN,
                                            mac_abi::ResourceKey { kind,
                                                                   id })
                                       .map_err(|_| Error("definition resource is missing"))?;
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
        if extent > 0x1000000 || extent & 4095 != 0 || d.load_base & 4095 != 0
        {
            return Err(Error("invalid code resource reservation"));
        }
        // Definition resources are independently relocatable modules. Allocate
        // top-down rather than depending on their build-time preferred addresses.
        let mut base = 0x83000000u64 - extent;
        loop
        {
            if base < 0x82000000
            {
                return Err(Error("code resource address space exhausted"));
            }
            let overlap = self.bindings.iter().find(|b| base < b.limit && b.base < base + extent);
            let Some(other) = overlap
            else
            {
                break;
            };
            base =
                other.base.checked_sub(extent).ok_or(Error("code resource placement overflow"))?;
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

pub fn resources(bytes: &[u8]) -> Result<Vec<Resource>>
{
    resource::decode(bytes)
}
pub fn status(error: Error) -> Status
{
    if error.0.contains("translation")
    {
        mac_abi::TRANSLATION_REQUIRED
    }
    else if error.0.contains("missing")
    {
        NOT_FOUND
    }
    else
    {
        BAD_ARGUMENT
    }
}

pub fn application_region(base: u64, limit: u64) -> bool
{
    base < limit
    && ((base >= 0x81000000 && limit <= 0x82000000) || (base >= 0x83000000 && limit <= 0x87000000))
}
pub fn reservation(resources: &[Resource]) -> Result<(u64, u64)>
{
    let resource = resources.iter()
                            .find(|r| r.kind == *b"NABI" && r.id == 0)
                            .ok_or(Error("application ABI resource is missing"))?;
    let d = Descriptor::decode(&resource.data)?;
    if d.role != Role::Application || !application_region(d.load_base, d.load_limit)
    {
        return Err(Error("application reservation overlaps resident images"));
    }
    Ok((d.load_base, d.load_limit))
}
