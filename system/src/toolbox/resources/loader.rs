//! Native application/code-resource loading with shared automatic translation routing.
use alloc::vec::Vec;
use core::arch::asm;

use mac_abi::{BAD_ARGUMENT, BootInfo, DefinitionEntry, NOT_FOUND, OK, Status};
use mac_formats::{Error, Result,
                  code::{CodeRegistry, NoTranslator},
                  elf,
                  native::{Descriptor, Role},
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

pub fn application(resources: &[Resource]) -> Result<usize>
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
    load(&descriptor, &code.data)
}

fn load(descriptor: &Descriptor, bytes: &[u8]) -> Result<usize>
{
    let image = elf::decode(bytes, descriptor.load_base, descriptor.load_limit)?;
    for segment in &image.segments
    {
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr().add(segment.offset as usize),
                                           segment.address as *mut u8,
                                           segment.file_size as usize);
            core::ptr::write_bytes((segment.address + segment.file_size) as *mut u8,
                                   0,
                                   (segment.memory_size - segment.file_size) as usize);
        }
    }
    unsafe {
        asm!("fence.i", options(nostack));
    }
    Ok(image.entry as usize)
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
        if d.load_base < 0x82000000
           || d.load_limit > 0x83000000
           || self.bindings.iter().any(|b| d.load_base < b.limit && b.base < d.load_limit)
        {
            return Err(Error("code resource reservation overlaps resident images"));
        }
        let address = load(d, &code.elf)?;
        let entry: DefinitionEntry = unsafe { core::mem::transmute(address) };
        self.bindings.push(Binding { kind,
                                     id,
                                     origin,
                                     base: d.load_base,
                                     limit: d.load_limit,
                                     entry });
        serial!("CODE_LOADED {}{} entry=0x{:x} origin={}",
                text::decode(&kind),
                id,
                address,
                origin);
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
