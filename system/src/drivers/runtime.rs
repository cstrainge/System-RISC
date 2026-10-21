//! Executable module boundary. RAM reservations, entry casts and foreign slice
//! borrows stay here; admission/ownership and all registry logic are safe Rust.
use alloc::{boxed::Box, vec::Vec};

use mac_abi::{ACCESS_DENIED, BAD_ABI, BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, NOT_SUPPORTED, OK,
              SERVICE_BUSY, Status};
use mac_formats::{module::Package, native::image, resource::Resource};
use mac_module::{ClassId, PackageKind, Target, abi};

use super::{Error, Registry};
use crate::state::TryState;
static RUNTIME: TryState<Runtime> = TryState::new();
struct Runtime
{
    registry: Registry,
    bindings: Vec<Binding>,
    next:     u64,
    limit:    u64
}
struct Binding
{
    module:   u32,
    base:     u64,
    limit:    u64,
    _host:    Box<abi::Host>,
    instance: Option<abi::Instance>
}
fn status(error: Error) -> Status
{
    match error
    {
        Error::Stale => NOT_FOUND,
        Error::Access => ACCESS_DENIED,
        Error::Busy | Error::State => SERVICE_BUSY,
        Error::Exhausted => NO_MEMORY,
        Error::Contract(_) => BAD_ABI,
        Error::Duplicate | Error::Exclusive => BAD_ARGUMENT
    }
}
pub(crate) fn initialize(arena: core::ops::Range<u64>, target: Target) -> Result<(), Status>
{
    if arena.start >= arena.end || arena.start & 4095 != 0 || arena.end & 4095 != 0
    {
        return Err(BAD_ARGUMENT);
    }
    RUNTIME.publish(Runtime { registry: Registry::new(target, &[]),
                              bindings: Vec::new(),
                              next:     arena.start,
                              limit:    arena.end })
           .map_err(crate::state::AccessError::status)
}

pub(crate) fn load(resources: &[Resource], file_type: [u8; 4], directory: PackageKind)
                   -> Result<u32, Status>
{
    // Complete structure/hash/relocation checks before changing RAM or entry.
    let package = Package::inspect(resources, file_type).map_err(|error| {
                                                            serial!("MODULE_PACKAGE_REJECTED \
                                                                     reason={}",
                                                                    error);
                                                            BAD_ABI
                                                        })?;
    let (module, entry, host, base, limit) =
        RUNTIME.with(|runtime| {
                   let extent =
                       package.code.descriptor.load_limit - package.code.descriptor.load_base;
                   let base = runtime.next;
                   let limit = base.checked_add(extent).ok_or(NO_MEMORY)?;
                   if limit > runtime.limit
                   {
                       return Err(NO_MEMORY);
                   }
                   let plan = image::LoadPlan::new(&package.code.code,
                                                   &package.code.descriptor,
                                                   Some(&package.code.relocations),
                                                   base).map_err(|_| BAD_ABI)?;
                   runtime.bindings.try_reserve(1).map_err(|_| NO_MEMORY)?;
                   let module = runtime.registry
                                       .stage(package.contract, directory, file_type)
                                       .map_err(status)?;
                   let host = Box::new(abi::Host { header: abi::Header::of::<abi::Host>(),
                                                   module,
                                                   reserved: 0,
                                                   context: u64::from(module),
                                                   call: Some(broker),
                                                   resolve: Some(resolve) });
                   let pointer = &*host as *const abi::Host;
                   // This allocator owns this whole disjoint RAM reservation until reboot.
                   let output =
                       unsafe { core::slice::from_raw_parts_mut(base as *mut u8, plan.length()) };
                   if plan.write(output).is_err()
                   {
                       runtime.registry.rollback(module).map_err(status)?;
                       return Err(BAD_ABI);
                   }
                   unsafe {
                       core::arch::asm!("fence.i", options(nostack));
                   }
                   let entry = plan.entry();
                   runtime.next = limit;
                   runtime.bindings.push(Binding { module,
                                                   base,
                                                   limit,
                                                   _host: host,
                                                   instance: None });
                   Ok((module, entry, pointer, base, limit))
               })
               .map_err(crate::state::AccessError::status)??;
    let entry: abi::Entry = unsafe { core::mem::transmute(entry as usize) };
    let mut instance = abi::Instance::default();
    // No runtime/registry/Files/desktop guard is held during foreign code.
    serial!("MODULE_ENTER name={} id={} entry=0x{:x}", package.contract.name, module,
            entry as usize);
    let result = unsafe { entry(host, &mut instance) };
    let valid=instance.header.valid::<abi::Instance>() && instance.dispatch.is_some_and(|dispatch|{
        let address=dispatch as usize as u64;
        address & 1==0 && image::decode(&package.code.code,&package.code.descriptor).is_ok_and(|image|
            image.segments.iter().any(|s|s.flags & image::EXECUTE!=0 && address>=base+s.offset && address<base+s.offset+s.length as u64))
    });
    RUNTIME.with(|runtime| {
               if result != OK || !valid
               {
                   runtime.registry.rollback(module).map_err(status)?;
                   return Err(if result < 0 { result } else { BAD_ABI });
               }
               let binding =
                   runtime.bindings
                          .iter_mut()
                          .find(|b| b.module == module && b.base == base && b.limit == limit)
                          .ok_or(NOT_FOUND)?;
               binding.instance = Some(instance);
               if let Err(error) = runtime.registry.commit(module)
               {
                   let _ = runtime.registry.rollback(module);
                   return Err(status(error));
               }
               serial!("MODULE_READY id={} kind={} entry=0x{:x} base=0x{:x}",
                       module,
                       core::str::from_utf8(&file_type).unwrap_or("????"),
                       entry as usize,
                       base);
               serial!("MODULE_PUBLISHED name={} id={}", package.contract.name, module);
               Ok(module)
           })
           .map_err(crate::state::AccessError::status)?
}
unsafe fn checked_call<'a>(pointer: *mut abi::Call) -> Result<&'a mut abi::Call, Status>
{
    if pointer.is_null() || !pointer.addr().is_multiple_of(core::mem::align_of::<abi::Call>())
    {
        return Err(BAD_ARGUMENT);
    }
    // Only resident System or already-admitted kernel modules may call broker.
    // Their ABI contract establishes live/exclusive request and buffer borrows.
    let call = unsafe { &mut *pointer };
    if !call.header.valid::<abi::Call>() || call.reserved != 0
    {
        return Err(BAD_ABI);
    }
    if call.input_len > 1024 * 1024
       || call.output_capacity > 1024 * 1024
       || call.input_len != 0 && call.input.is_null()
       || call.output_capacity != 0 && call.output.is_null()
    {
        return Err(BAD_ARGUMENT);
    }
    let input_end = call.input.addr().checked_add(call.input_len as usize).ok_or(BAD_ARGUMENT)?;
    let output_end =
        call.output.addr().checked_add(call.output_capacity as usize).ok_or(BAD_ARGUMENT)?;
    let call_start = pointer.addr();
    let call_end = call_start.checked_add(core::mem::size_of::<abi::Call>()).ok_or(BAD_ARGUMENT)?;
    let overlap = |start: usize, end: usize, other_start: usize, other_end: usize| {
        start < end && other_start < other_end && start < other_end && other_start < end
    };
    if overlap(call.input.addr(), input_end, call.output.addr(), output_end)
       || overlap(call_start, call_end, call.input.addr(), input_end)
       || overlap(call_start, call_end, call.output.addr(), output_end)
    {
        return Err(BAD_ARGUMENT);
    }
    Ok(call)
}
unsafe extern "C" fn broker(caller: u64, provider: u32, pointer: *mut abi::Call) -> Status
{
    // The C callback never owns or retains the caller's buffers.
    match unsafe { invoke(caller, provider, pointer) }
    {
        Ok(status) => status,
        Err(status) => status
    }
}
unsafe fn invoke(caller: u64, provider: u32, pointer: *mut abi::Call) -> Result<Status, Status>
{
    let call = unsafe { checked_call(pointer) }?;
    call.output_len = 0;
    let (module, ticket, instance) =
        RUNTIME.with(|runtime| {
                   let interface = if call.phase == abi::START
                                   {
                                       runtime.registry.interface(provider)
                                   }
                                   else
                                   {
                                       runtime.registry.interface_for_request(caller, call.ticket)
                                   }.map_err(status)?;
                   if interface.offer.class.0 != call.class
                   {
                       return Err(BAD_ARGUMENT);
                   }
                   let ticket = match call.phase
                   {
                       abi::START if call.ticket == 0 =>
                       {
                           runtime.registry
                                  .begin(caller, provider, call.operation, call.input_len as usize,
                                         call.output_capacity as usize)
                                  .map_err(status)?
                       }
                       abi::POLL | abi::CANCEL =>
                       {
                           let request =
                               runtime.registry.request(caller, call.ticket).map_err(status)?;
                           if request.interface != provider
                              || request.class.0 != call.class
                              || request.operation != call.operation
                              || call.input_len > interface.offer.max_request
                              || call.output_capacity > request.max_reply
                           {
                               return Err(BAD_ARGUMENT);
                           }
                           call.ticket
                       }
                       _ => return Err(BAD_ARGUMENT)
                   };
                   let instance = runtime.bindings
                                         .iter()
                                         .find(|b| b.module == interface.module)
                                         .and_then(|b| b.instance)
                                         .ok_or(NOT_FOUND)?;
                   runtime.registry.enter_request(caller, ticket).map_err(status)?;
                   Ok((interface.module, ticket, instance))
               })
               .map_err(crate::state::AccessError::status)??;
    call.ticket = ticket;
    let dispatch = instance.dispatch.ok_or(NOT_SUPPORTED)?;
    let original = *call;
    let result = unsafe { dispatch(instance.state, call) };
    let output_len = call.output_len;
    let mutated = call.header.version != original.header.version
                  || call.header.size != original.header.size
                  || call.ticket != original.ticket
                  || call.phase != original.phase
                  || call.class != original.class
                  || call.operation != original.operation
                  || call.input != original.input
                  || call.input_len != original.input_len
                  || call.output != original.output
                  || call.output_capacity != original.output_capacity
                  || call.reserved != original.reserved;
    *call = original;
    call.output_len = output_len;
    // A module cannot forge a different caller's ticket through mutable Call.
    let malformed = mutated
                    || call.ticket != ticket
                    || call.output_len > call.output_capacity
                    || result == abi::PENDING && call.output_len != 0;
    call.ticket = ticket;
    RUNTIME.with(|runtime| runtime.registry.leave_request(caller, ticket).map_err(status))
           .map_err(crate::state::AccessError::status)??;
    if result == abi::PENDING && !malformed
    {
        return Ok(result);
    }
    RUNTIME.with(|runtime| {
               let request = runtime.registry.request(caller, ticket).map_err(status)?;
               if request.module != module
               {
                   return Err(BAD_ARGUMENT);
               }
               runtime.registry
                      .complete(caller,
                                ticket,
                                if malformed { 0 } else { call.output_len as usize })
                      .map_err(status)?;
               Ok(())
           })
           .map_err(crate::state::AccessError::status)??;
    if malformed || result > 0
    {
        call.output_len = 0;
        return Err(BAD_ABI);
    }
    Ok(result)
}

pub(crate) fn discover() -> Result<(), Status>
{
    let candidates=crate::kernel::with_files(|files|files.module_candidates()).map_err(crate::state::AccessError::status)??;
    let mut waiting = Vec::new();
    for candidate in candidates
    {
        if candidate.file_type != candidate.directory.file_type()
        {
            serial!("MODULE_REJECTED name={} reason=wrong-installation-directory",
                    core::str::from_utf8(&candidate.name).unwrap_or("?"));
            continue;
        }
        let metadata = crate::kernel::with_files(|files| {
                           files.module_metadata(candidate.volume, candidate.cnid)
                       }).map_err(crate::state::AccessError::status)?;
        match metadata
        {
            Ok(metadata) if mac_module::Contract::decode(&metadata).is_ok() =>
            {
                waiting.push((candidate, metadata))
            }
            _ =>
            {
                serial!("MODULE_REJECTED name={} reason=missing-or-invalid-contract",
                        core::str::from_utf8(&candidate.name).unwrap_or("?"))
            }
        }
    }
    while !waiting.is_empty()
    {
        let mut progress = false;
        let mut index = 0;
        while index < waiting.len()
        {
            let (candidate, metadata) = &waiting[index];
            let contract = mac_module::Contract::decode(metadata).map_err(|_| BAD_ABI)?;
            let admitted = RUNTIME.with(|runtime| {
                                      runtime.registry.admit(contract, candidate.directory,
                                                             candidate.file_type)
                                  })
                                  .map_err(crate::state::AccessError::status)?;
            if admitted == Err(Error::Contract(mac_module::Error::MissingDependency))
            {
                index += 1;
                continue;
            }
            let (candidate, _) = waiting.remove(index);
            progress = true;
            if let Err(error) = admitted
            {
                serial!("MODULE_REJECTED name={} reason={:?}",
                        core::str::from_utf8(&candidate.name).unwrap_or("?"),
                        error);
                continue;
            }
            let resources = crate::kernel::with_files(|files| {
                                files.module_resources(candidate.volume, candidate.cnid)
                            }).map_err(crate::state::AccessError::status)?;
            let result = resources.and_then(|resources| {
                                      load(&resources, candidate.file_type, candidate.directory)
                                  });
            if let Err(status) = result
            {
                serial!("MODULE_REJECTED name={} status={}",
                        core::str::from_utf8(&candidate.name).unwrap_or("?"),
                        status);
            }
        }
        if !progress
        {
            for (candidate, _) in waiting
            {
                serial!("MODULE_REJECTED name={} reason=missing-or-cyclic-dependency",
                        core::str::from_utf8(&candidate.name).unwrap_or("?"));
            }
            break;
        }
    }
    Ok(())
}
unsafe extern "C" fn resolve(caller: u64, class: u32, version: u32, output: *mut u32) -> Status
{
    if output.is_null() || !output.addr().is_multiple_of(core::mem::align_of::<u32>())
    {
        return BAD_ARGUMENT;
    }
    match RUNTIME.with(|runtime| {
                     runtime.registry.resolve(caller, ClassId(class.to_le_bytes()), version)
                 })
                 .map_err(crate::state::AccessError::status)
                 .and_then(|r| r.map_err(status))
    {
        Ok(provider) =>
        {
            unsafe { output.write(provider) };
            OK
        }
        Err(status) => status
    }
}
