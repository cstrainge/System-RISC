//! Executable module boundary. RAM reservations, entry casts and foreign slice
//! borrows stay here; admission/ownership and all registry logic are safe Rust.
use alloc::{boxed::Box, vec::Vec};

use mac_abi::{ACCESS_DENIED, BAD_ABI, BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, NOT_SUPPORTED, OK,
              SERVICE_BUSY, Status, WRONG_THREAD};
use mac_formats::{module::Package, native::image, resource::Resource};
use mac_module::{ClassId, PackageKind, Target, abi};

use super::{Error, Registry, status};
use crate::state::TryState;
mod admission;
mod allocation;
mod critical;
mod invocation;
mod workers;
pub(crate) use workers::Launch as WorkerLaunch;
mod discovery;
mod file_handlers;
mod recovery;
pub(crate) use discovery::discover;
pub(crate) use file_handlers::{file_handler_icon, file_handler_provider};
pub(crate) use recovery::changed as recovery_changed;
static RUNTIME: TryState<Runtime> = TryState::new();
struct Runtime
{
    registry:  Registry,
    bindings:  Vec<Binding>,
    residents: Vec<ResidentBinding>,
    next:      u64,
    limit:     u64
}
struct Binding
{
    module:        u64,
    image:         crate::images::Binding,
    base:          u64,
    limit:         u64,
    _host:         Box<abi::Host>,
    instance:      Option<abi::Instance>,
    file_handlers: Vec<file_handlers::Icon>
}
/// Class adapters receive checked fresh borrows, never C pointers or registry
/// guards. They may return Pending but cannot retain either borrowed buffer.
pub(crate) struct ResidentCall<'a>
{
    pub caller:    u64,
    pub ticket:    u64,
    pub phase:     u32,
    pub operation: u32,
    pub input:     &'a [u8]
}
pub(crate) enum ResidentCompletion
{
    Complete(usize),
    Pending
}
pub(crate) use super::synchronous::Callback as SynchronousCallback;
pub(crate) type ResidentCallback =
    fn(ResidentCall<'_>, &mut [u8]) -> Result<ResidentCompletion, Status>;
struct ResidentBinding
{
    interface:   u64,
    callback:    Option<ResidentCallback>,
    synchronous: SynchronousCallback
}
#[derive(Clone, Copy)]
enum DispatchTarget
{
    Module(abi::Instance, crate::images::Binding),
    Resident(ResidentCallback)
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum CompletionPolicy
{
    Caller,
    Recovery
}

pub(crate) fn inspect<T: crate::toolbox::inspection::Subject>(processes:&mac_object_table::domain::Directory<T,crate::toolbox::handles::InvocationGate>,
                                                              caller: u64,
                                                              call: &mut mac_abi::inspection::Call)
                                                              -> Status
{
    if call.scope == mac_abi::inspection::SELF
    {
        return crate::toolbox::inspection::request(processes, caller, call);
    }
    match RUNTIME.with(|runtime| runtime.registry.inspect(processes, caller, call))
    {
        Ok(status) => status,
        Err(crate::state::AccessError::Unavailable) =>
        {
            crate::toolbox::inspection::request(processes, caller, call)
        }
        Err(error) =>
        {
            let detail =
                "module lifetime registry is serving another invocation; retry this inspection";
            call.detail[..detail.len()].copy_from_slice(detail.as_bytes());
            call.detail_length = detail.len() as u32;
            error.status()
        }
    }
}

/// Install an explicitly non-deferred resident entry. An optional ticket-owning
/// entry is separate: VDEV uses it only for RESET, while SCEX is sync-only.
/// This callback must not yield, park, wait for I/O or enter foreign module
/// code. Its bounded admission/effect span uses the native invocation gate;
/// runtime/owner locks are released before callback entry. This is not SMP.
pub(crate) fn register_synchronous_service(offer: mac_module::Offer,
                                           callback: SynchronousCallback,
                                           asynchronous: Option<ResidentCallback>)
                                           -> Result<u64, Status>
{
    RUNTIME.with(|runtime| {
               runtime.residents.try_reserve(1).map_err(|_| NO_MEMORY)?;
               let interface = runtime.registry.register_service(offer).map_err(status)?;
               runtime.residents.push(ResidentBinding { interface,
                                                        callback: asynchronous,
                                                        synchronous: callback });
               Ok(interface)
           })
           .map_err(crate::state::AccessError::status)?
}

pub(crate) fn scrap_authority(
    caller: u64)
    -> Result<Result<crate::toolbox::scrap::extension::Authority,
                     crate::toolbox::scrap::extension::Error>,
              crate::state::AccessError>
{
    RUNTIME.with(|runtime| {
               crate::toolbox::scrap::extension::Authority::authenticate(&runtime.registry, caller)
           })
}

pub(crate) fn device_allowed(module: u64, bus: ClassId, key: [u8; 16]) -> Result<(), Status>
{
    RUNTIME.with(|runtime| runtime.registry.device_allowed(module, bus, key).map_err(status))
           .map_err(crate::state::AccessError::status)?
}

pub(crate) fn module_authority(module: u64) -> Result<crate::toolbox::handles::OwnerRef, Status>
{
    RUNTIME.with(|runtime| runtime.registry.module_ledger(module).map_err(status))
           .map_err(crate::state::AccessError::status)?
}

pub(crate) fn module_allocation(module: u64) -> Result<super::registry::ModuleAllocation, Status>
{
    RUNTIME.with(|runtime| runtime.registry.module_allocation(module).map_err(status))
           .map_err(crate::state::AccessError::status)?
}

pub(crate) fn provider_module(interface: u64) -> Result<u64, Status>
{
    RUNTIME.with(|runtime| {
               let module = runtime.registry.interface(interface).map_err(status)?.module;
               if module == 0
               {
                   return Err(ACCESS_DENIED);
               }
               Ok(module)
           })
           .map_err(crate::state::AccessError::status)?
}

/// Class-neutral admission policy: a provider that serializes all outstanding
/// requests cannot also serve a backing dependency of its own pending request.
pub(crate) fn provider_serialized(interface: u64) -> Result<bool, Status>
{
    RUNTIME.with(|runtime| {
               let provider = runtime.registry.interface(interface).map_err(status)?;
               Ok(provider.offer.flags & mac_module::Offer::SERIALIZED != 0)
           })
           .map_err(crate::state::AccessError::status)?
}

/// Only the admitted module's pinned load reservation can be offered for DMA.
/// The bus adapter still checks device ownership and maps it for that device.
pub(crate) fn owns_memory(module: u64, base: u64, length: u64) -> Result<(), Status>
{
    let end = base.checked_add(length).ok_or(BAD_ARGUMENT)?;
    if length == 0
    {
        return Err(BAD_ARGUMENT);
    }
    RUNTIME.with(|runtime| {
               let binding =
                   runtime.bindings.iter().find(|b| b.module == module).ok_or(NOT_FOUND)?;
               if base < binding.base || end > binding.limit
               {
                   return Err(ACCESS_DENIED);
               }
               Ok(())
           })
           .map_err(crate::state::AccessError::status)?
}

pub(crate) fn initialize(arena: core::ops::Range<u64>, target: Target) -> Result<(), Status>
{
    if arena.start >= arena.end || arena.start & 4095 != 0 || arena.end & 4095 != 0
    {
        return Err(BAD_ARGUMENT);
    }
    let owner = crate::toolbox::handles::system_owner()?;
    let kinds = crate::toolbox::handles::registry(|registry| {
                    Ok([registry.get(crate::toolbox::handles::KindCode(*b"DIFC"))?,
                        registry.get(crate::toolbox::handles::KindCode(*b"DIFS"))?,
                        registry.get(crate::toolbox::handles::KindCode(*b"DDEP"))?,
                        registry.get(crate::toolbox::handles::KindCode(*b"DDST"))?,
                        registry.get(crate::toolbox::handles::KindCode(*b"DREQ"))?,
                        registry.get(crate::toolbox::handles::KindCode(*b"DRQS"))?])
                }).map_err(|error| error.status())?
                  .map_err(|error| crate::toolbox::handles::registry_status(&error))?;
    RUNTIME.publish(Runtime { registry:  Registry::new(target, owner, kinds, crate::machine::memory::MODULE_KERNEL_BUDGET_BYTES).map_err(status)?,
                              bindings:  Vec::new(),
                              residents: Vec::new(),
                              next:      arena.start,
                              limit:     arena.end })
           .map_err(crate::state::AccessError::status)
}

pub(crate) fn load(resources: Vec<Resource>, file_type: [u8; 4], directory: PackageKind)
                   -> Result<u64, Status>
{
    let source =
        crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
            alloc::sync::Arc::try_new(crate::images::Source { role: crate::images::Role::Module,
                                                              resources,
                                                              creator: *b"????",
                                                              file_type })
        }).map_err(|_| NO_MEMORY)?;
    let resources = &source.resources[..];
    // Complete structure/hash/relocation checks before changing RAM or entry.
    let package = Package::inspect(resources, file_type).map_err(|error| {
                                                            serial!("MODULE_PACKAGE_REJECTED \
                                                                     reason={}",
                                                                    error);
                                                            admission::package_status(error)
                                                        })?;
    let handlers = file_handlers::prepare(resources, package.contract)?;
    let mut rules = Vec::new();
    rules.try_reserve_exact(handlers.len()).map_err(|_| NO_MEMORY)?;
    rules.extend(handlers.iter().map(|h| h.rule));
    let (module, loaded, host, base, limit) =
        RUNTIME.with(|runtime| {
                   let extent =
                       package.code.descriptor.load_limit - package.code.descriptor.load_base;
                   let base = runtime.next;
                   let limit = base.checked_add(extent).ok_or(NO_MEMORY)?;
                   if limit > runtime.limit
                   {
                       serial!("MODULE_CAPACITY_REFUSED name={} requested={} available={} \
                                next=0x{:x} limit=0x{:x} \
                                action=rebuild_with_a_larger_checked_module_reservation",
                               package.contract.name,
                               extent,
                               runtime.limit.saturating_sub(base),
                               base,
                               runtime.limit);
                       return Err(NO_MEMORY);
                   }
                   let plan = image::LoadPlan::new(&package.code.code,
                                                   &package.code.descriptor,
                                                   Some(&package.code.relocations),
                                                   base).map_err(|_| BAD_ABI)?;
                   runtime.bindings.try_reserve(1).map_err(|_| NO_MEMORY)?;
                   let decoded = image::decode(&package.code.code, &package.code.descriptor).map_err(|_| BAD_ABI)?;
                   let module = runtime.registry
                                       .stage(package.contract, directory, file_type)
                                       .map_err(status)?;
                   if let Err(error) = runtime.registry.install_file_handlers(module, &rules)
                   {
                       if let Err(cleanup) = runtime.registry.rollback(module)
                       {
                           serial!("MODULE_INIT_CLEANUP_RETAINED module={} original={} cleanup={}",
                                   module, error, cleanup);
                           return Err(status(cleanup));
                       }
                       return Err(status(error));
                   }
                   let host = Box::try_new(abi::Host { header: abi::Header::of::<abi::Host>(),
                                                       module,
                                                       reserved: 0,
                                                       context: u64::from(module),
                                                       call: Some(broker),
                                                       resolve: Some(resolve),
                                                       allocate: Some(allocation::allocate),
                                                       deallocate: Some(allocation::deallocate),
                                                       reallocate: Some(allocation::reallocate),
                                                       with_module_allocation: Some(allocation::with_module_allocation),
                                                       workers: Some(workers::control),
                                                       critical_section: Some(critical::enter) });
                   let host = match host
                   {
                       Ok(host) => host,
                       Err(_) =>
                       {
                           runtime.registry.rollback(module).map_err(status)?;
                           return Err(NO_MEMORY);
                       }
                   };
                   let pointer = &*host as *const abi::Host;
                   let loaded = (|| {
                       let owner = runtime.registry.module_ledger(module).map_err(status)?;
                       let account = runtime.registry.module_allocation(module).map_err(status)?;
                       crate::allocator::with_target(crate::allocator::Target::Owner {
                           account, class: mac_kernel_model::policy::MemoryClass::Kernel
                       }, || {
                           let mut executable = Vec::new();
                           executable.try_reserve_exact(decoded.segments.len()).map_err(|_| NO_MEMORY)?;
                           executable.extend(decoded.segments.iter().filter(|s| s.flags & image::EXECUTE != 0)
                               .map(|s| base + s.offset .. base + s.offset + s.length as u64));
                           crate::kernel::load_module(&owner, source.clone(), base, extent, executable, |_, _| {
                               // Exact disjoint RAM remains reserved until reset. No reclamation is claimed.
                               let output = unsafe { core::slice::from_raw_parts_mut(base as *mut u8, plan.length()) };
                               plan.write(output).map_err(|_| BAD_ABI)?;
                               unsafe { core::arch::asm!("fence.i", options(nostack)); }
                               Ok(plan.entry() as usize)
                           })
                       })
                   })();
                   let loaded = match loaded {
                       Ok(loaded) => loaded,
                       Err(error) => { runtime.registry.rollback(module).map_err(status)?; return Err(error); }
                   };
                   let image = loaded.binding();
                   runtime.next = limit;
                   runtime.bindings.push(Binding { module, image,
                                                   base,
                                                   limit,
                                                   _host: host,
                                                   instance: None,
                                                   file_handlers: handlers });
                   Ok((module, loaded, pointer, base, limit))
               })
               .map_err(crate::state::AccessError::status)??;
    let entry: abi::Entry = unsafe { core::mem::transmute(loaded.entry()) };
    let execution = match loaded.into_module()
    {
        Ok(execution) => execution,
        Err(error) =>
        {
            RUNTIME.with(|runtime| runtime.registry.rollback(module).map_err(status))
                   .map_err(crate::state::AccessError::status)??;
            return Err(error);
        }
    };
    let mut instance = abi::Instance::default();
    // No runtime/registry/Files/desktop guard is held during foreign code.
    serial!("MODULE_ENTER name={} id={} entry=0x{:x}", package.contract.name, module,
            entry as usize);
    // Initialization is one real module operation. Direct nested broker calls
    // must inherit this same root across Start/Poll/Cancel; admitting a fresh
    // root on each callback makes a genuine pending ticket look foreign.
    // This result reaches the same rollback path even if admission or context
    // installation fails before any foreign code executes.
    let result = module_authority(module).and_then(|owner| {
                                             let allocation = crate::allocation::Target::Owner {
            account: module_allocation(module)?,
            class: mac_kernel_model::policy::MemoryClass::Kernel
        };
                                             super::jobs::subjects::resident()?.admit(
            &crate::toolbox::handles::subject::Authority::Client(owner), allocation)
                                         })
                                         .and_then(|subject| {
                                             crate::context::with_io_subject(subject, || {
                                                 invocation::enter(&execution, || unsafe {
                                                     entry(host, &mut instance)
                                                 })
                                             }).and_then(core::convert::identity)
                                         })
                                         .unwrap_or_else(|status| status);
    let valid=instance.header.valid::<abi::Instance>() && instance.dispatch.is_some_and(|dispatch|{
        let address=dispatch as usize as u64;
        address & 1==0 && image::decode(&package.code.code,&package.code.descriptor).is_ok_and(|image|
            image.segments.iter().any(|s|s.flags & image::EXECUTE!=0 && address>=base+s.offset && address<base+s.offset+s.length as u64))
    });
    drop(execution);
    crate::machine::interrupts::with_masked(|| {
        RUNTIME.with(|runtime| {
                   if result != OK || !valid
                   {
                       if let Err(cleanup) = runtime.registry.rollback(module)
                       {
                           serial!("MODULE_INIT_CLEANUP_RETAINED module={} original={} reason={}",
                                   module,
                                   if result < 0 { result } else { BAD_ABI },
                                   cleanup);
                           return Err(status(cleanup));
                       }
                       return Err(if result < 0 { result } else { BAD_ABI });
                   }
                   let binding =
                       runtime.bindings
                              .iter_mut()
                              .find(|b| b.module == module && b.base == base && b.limit == limit)
                              .ok_or(NOT_FOUND)?;
                   binding.instance = Some(instance);
                   let owner = runtime.registry.module_ledger(module).map_err(status)?;
                   if let Err(error) = runtime.registry.commit(module)
                   {
                       if let Err(cleanup) = runtime.registry.rollback(module)
                       {
                           serial!("MODULE_INIT_CLEANUP_RETAINED module={} original={} cleanup={}",
                                   module, error, cleanup);
                           return Err(status(cleanup));
                       }
                       return Err(status(error));
                   }
                   // All backing was admitted while workers were dormant. Keep
                   // registry publication and the armed-to-runnable transition in
                   // this same resident admission turn; activation allocates nothing.
                   crate::machine::scheduler::activate_workers(&owner);
                   serial!("MODULE_READY id={} kind={} entry=0x{:x} base=0x{:x}",
                           module,
                           core::str::from_utf8(&file_type).unwrap_or("????"),
                           entry as usize,
                           base);
                   serial!("MODULE_PUBLISHED name={} id={}", package.contract.name, module);
                   Ok(module)
               })
               .map_err(crate::state::AccessError::status)?
    })
}
unsafe extern "C" fn broker(caller: u64, provider: u64, pointer: *mut abi::Call) -> Status
{
    if let Err(error) = crate::context::authenticate_module(caller)
    {
        return error;
    }
    if crate::context::critical_active()
    {
        return WRONG_THREAD;
    }
    // The C callback never owns or retains the caller's buffers.
    match unsafe { invoke(caller, provider, pointer) }
    {
        Ok(status) => status,
        Err(status) => status
    }
}
unsafe fn invoke(caller: u64, provider: u64, pointer: *mut abi::Call) -> Result<Status, Status>
{
    unsafe { invoke_origin(super::registry::requests::Origin::Direct, caller, provider, pointer) }
}
unsafe fn invoke_origin(origin: super::registry::requests::Origin, caller: u64, provider: u64,
                        pointer: *mut abi::Call)
                        -> Result<Status, Status>
{
    if let Some(subject) = crate::context::io_subject()
    {
        return unsafe {
            invoke_scoped(origin,
                          &subject,
                          caller,
                          provider,
                          pointer,
                          CompletionPolicy::Caller,
                          &mut false)
        };
    }
    // A module may call its declared services during entry, outside a queued
    // operation. Admit that real module once; nested calls inherit its frame.
    let owner = if caller == 0
    {
        crate::toolbox::handles::system_owner()?
    }
    else
    {
        module_authority(caller)?
    };
    let authority = crate::toolbox::handles::subject::Authority::Client(owner);
    let subject = if caller == 0
    {
        // Resident requests retain the actual executor's original allocation
        // selection. The broker's numeric caller label grants no account.
        super::jobs::subjects::resident()?.admit_current(&authority)?
    }
    else
    {
        super::jobs::subjects::resident()?.admit(&authority, crate::allocation::Target::Owner {
            account: module_allocation(caller)?,
            class: mac_kernel_model::policy::MemoryClass::Kernel
        })?
    };
    crate::context::with_io_subject(subject.clone(), || {
        unsafe {
            invoke_scoped(origin,
                          &subject,
                          caller,
                          provider,
                          pointer,
                          CompletionPolicy::Caller,
                          &mut false)
        }
    }).and_then(core::convert::identity)
}
unsafe fn invoke_scoped(origin: super::registry::requests::Origin,
                        subject: &super::jobs::subjects::Subject, caller: u64, provider: u64,
                        pointer: *mut abi::Call, policy: CompletionPolicy, started: &mut bool)
                        -> Result<Status, Status>
{
    use super::registry::requests::Access;
    let call = unsafe { super::synchronous::checked_call(pointer) }?;
    call.output_len = 0;
    if call.phase == abi::SYNCHRONOUS
    {
        // Keep root validation and this bounded leaf in one native invocation.
        // The shared core releases the registry lock before the callback.
        return crate::machine::interrupts::with_masked(|| unsafe {
            super::synchronous::invoke(call, origin, caller, |call| {
                RUNTIME.with(|runtime| {
                           super::synchronous::authorize(&runtime.registry,
                                                         subject,
                                                         caller,
                                                         provider,
                                                         call,
                                                         |interface| {
                                                             runtime.residents
                                                                    .iter()
                                                                    .find(|binding| {
                                                                        binding.interface
                                                                        == interface
                                                                    })
                                                                    .map(|binding| {
                                                                        binding.synchronous
                                                                    })
                                                         })
                       })
                       .map_err(crate::state::AccessError::status)?
            })
        });
    }

    if call.phase == abi::START
    {
        if call.ticket != 0
        {
            return Err(BAD_ARGUMENT);
        }
        // Validate the same central authority/offer contract before allocating
        // payload storage. Publication rechecks it after fallible preparation;
        // an invalid/foreign request must not masquerade as heap refusal.
        RUNTIME.with(|runtime| {
                   let interface = runtime.registry
                                          .authorize_call(caller, provider, call.input_len as usize,
                                                          call.output_capacity as usize)
                                          .map_err(status)?;
                   if interface.offer.class.0 != call.class
                   {
                       return Err(BAD_ARGUMENT);
                   }
                   Ok(())
               })
               .map_err(crate::state::AccessError::status)??;
    }

    // checked_call validated this exact borrow. Retain Start payload in the
    // canonical request before publishing its ticket; later wire Poll calls do
    // not need to lend that original buffer again.
    let input = if call.input_len == 0
    {
        &[]
    }
    else
    {
        unsafe { core::slice::from_raw_parts(call.input, call.input_len as usize) }
    };
    let mut preparation = if call.phase == abi::START
    {
        Some(super::registry::requests::dispatch::prepare(subject, origin, input,
                                                          call.output_capacity as usize)?)
    }
    else
    {
        None
    };
    let (module, ticket, target, invocation, mut lease, delivery) =
        RUNTIME.with(|runtime| {
                   let interface = if call.phase == abi::START
                                   {
                                       runtime.registry.interface(provider)
                                   }
                                   else
                                   {
                                       runtime.registry
                                  .interface_for_request(caller,
                                                         Access::Continuing { subject,
                                                                              ticket:
                                                                                  call.ticket })
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
                                  .begin_scoped(subject,
                                                caller,
                                                provider,
                                                call.operation,
                                                input,
                                                call.output_capacity as usize,
                                                origin,
                                                preparation.as_mut().ok_or(BAD_ARGUMENT)?)
                                  .map_err(status)?
                       }
                       abi::POLL | abi::CANCEL =>
                       {
                           let request = runtime.registry
                                                .request(caller,
                                                         Access::Continuing { subject,
                                                                              ticket: call.ticket })
                                                .map_err(status)?;
                           if request.origin() != origin
                              || request.interface != provider
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
                   // Publication is the irrevocable handoff: every later
                   // refusal must return the real ticket, including code-pin
                   // or invocation preparation failure before foreign entry.
                   call.ticket = ticket;
                   let storage = runtime.registry
                                        .request_dispatch(caller,
                                                          Access::Continuing { subject,
                                                                               ticket })
                                        .map_err(status)?;
                   let mut lease = storage.acquire()?;
                   let delivery = lease.delivery(call.phase);
                   let invocation = if delivery.is_some()
                   {
                       runtime.registry
                              .request_invocation(caller,
                                                  Access::Continuing { subject,
                                                                       ticket })
                              .map_err(status)?
                   }
                   else
                   {
                       subject.clone()
                   };
                   let target = if delivery.is_some()
                   {
                       Some({
                           if interface.module == 0
                           {
                               DispatchTarget::Resident(runtime.residents
                                                               .iter()
                                                               .find(|r| r.interface == provider)
                                                               .ok_or(NOT_FOUND)?
                                                               .callback
                                                               .ok_or(NOT_SUPPORTED)?)
                           }
                           else
                           {
                               {
                                   let binding = runtime.bindings
                                                        .iter()
                                                        .find(|b| b.module == interface.module)
                                                        .ok_or(NOT_FOUND)?;
                                   DispatchTarget::Module(binding.instance.ok_or(NOT_FOUND)?,
                                                          binding.image)
                               }
                           }
                       })
                   }
                   else
                   {
                       None
                   };
                   Ok((interface.module, ticket, target, invocation, lease, delivery))
               })
               .map_err(crate::state::AccessError::status)??;
    // Record and the invocation lease now own preparation; release this
    // publication-time alias outside the registry before any foreign entry.
    drop(preparation);
    call.ticket = ticket;
    if let Some(phase) = delivery
    {
        let (result, length, malformed, entered) = {
            let (original_input, output, entered) = lease.buffers()?;
            let input = if phase == abi::START { original_input } else { input };
            // Retry transport capacity cannot alter the admitted operation.
            // Provider output always uses the original owned reply capacity;
            // the current caller's capacity is checked only at final copy.
            let mut forwarded = abi::Call { header: abi::Header::of::<abi::Call>(),
                                            ticket,
                                            phase,
                                            class: call.class,
                                            operation: call.operation,
                                            input: input.as_ptr(),
                                            input_len: input.len() as u32,
                                            output: output.as_mut_ptr(),
                                            output_capacity: output.len() as u32,
                                            output_len: 0,
                                            reserved: 0 };
            let original = forwarded;
            let admitted_reply_bytes = output.len();
            let mut invalid_reply = false;
            // The lease owns its buffers. No mailbox/registry/IRQ guard spans
            // the callback, and each real invocation retains its original root.
            let result =
                crate::context::with_io_subject(invocation, || -> Result<Status, Status> {
                    Ok(match target.ok_or(BAD_ABI)?
                    {
                        DispatchTarget::Module(instance, image) =>
                        {
                            let dispatch = instance.dispatch.ok_or(NOT_SUPPORTED)?;
                            let execution = crate::kernel::module_execution(image)?;
                            invocation::enter(&execution, || {
                                *started = true;
                                *entered = true;
                                unsafe { dispatch(instance.state, &mut forwarded) }
                            })?
                        }
                        DispatchTarget::Resident(callback) =>
                        {
                            *started = true;
                            *entered = true;
                            match callback(ResidentCall { caller,
                                                          ticket,
                                                          phase,
                                                          operation: call.operation,
                                                          input },
                                           output)
                            {
                                Ok(ResidentCompletion::Complete(length))
                                    if length <= admitted_reply_bytes =>
                                {
                                    forwarded.output_len = length as u32;
                                    OK
                                }
                                Ok(ResidentCompletion::Complete(_)) =>
                                {
                                    invalid_reply = true;
                                    BAD_ABI
                                }
                                Ok(ResidentCompletion::Pending) => abi::PENDING,
                                Err(status) if status < 0 => status,
                                Err(_) =>
                                {
                                    invalid_reply = true;
                                    BAD_ABI
                                }
                            }
                        }
                    })
                }).and_then(core::convert::identity);
            let malformed = invalid_reply
                            || forwarded.header.version != original.header.version
                            || forwarded.header.size != original.header.size
                            || forwarded.ticket != original.ticket
                            || forwarded.phase != original.phase
                            || forwarded.class != original.class
                            || forwarded.operation != original.operation
                            || forwarded.input != original.input
                            || forwarded.input_len != original.input_len
                            || forwarded.output != original.output
                            || forwarded.output_capacity != original.output_capacity
                            || forwarded.reserved != original.reserved
                            || forwarded.output_len > original.output_capacity
                            || result == Ok(abi::PENDING) && forwarded.output_len != 0;
            (result, forwarded.output_len as usize, malformed, *entered)
        };
        match result
        {
            Err(cause) =>
            {
                if entered
                {
                    lease.uncertain(cause);
                }
                // Before-entry Drop restores Unentered/Pending; after-entry
                // failure retains uncertainty rather than inventing an ACK.
                return Err(cause);
            }
            Ok(_) if malformed => lease.uncertain(BAD_ABI),
            Ok(status) => lease.returned(status, length)
        }
    }
    let Some((result, reply)) = lease.reply()?
    else
    {
        return Ok(abi::PENDING);
    };
    if reply.len() > call.output_capacity as usize
    {
        // A smaller retry buffer cannot discard a previously committed reply.
        return Err(BAD_ARGUMENT);
    }
    let length = reply.len();
    if !reply.is_empty()
    {
        unsafe { core::slice::from_raw_parts_mut(call.output, length) }.copy_from_slice(reply);
    }
    // Restore owned reply storage and really decrement the outside alias
    // before any canonical ACK. A refused registry borrow can then replay it.
    drop(lease);
    if policy == CompletionPolicy::Caller
    {
        let retirement =
            RUNTIME.with(|runtime| {
                       let access = Access::Continuing { subject,
                                                         ticket };
                       let request = runtime.registry.request(caller, access).map_err(status)?;
                       if request.module != module
                       {
                           return Err(BAD_ARGUMENT);
                       }
                       runtime.registry.complete_dispatch(caller, access, length).map_err(status)
                   })
                   .map_err(crate::state::AccessError::status)??;
        // Actual buffers/refunds precede Removed's completion ACK, outside all
        // registry/mailbox guards. Failed entry cannot pin a completed child.
        drop(retirement);
        call.ticket = 0;
    }
    // Recovery owns its existing unique cancellation ACK plan; its bytes are
    // copied above and no second execution protocol or early retire is used.
    call.output_len = length as u32;
    Ok(result)
}

unsafe extern "C" fn resolve(caller: u64, class: u32, version: u32, output: *mut u64) -> Status
{
    if let Err(error) = crate::context::authenticate_module(caller)
    {
        return error;
    }
    if output.is_null() || !output.is_aligned()
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
/// A kernel caller still resolves a live, declared class/version through the
/// same registry as modules. Filesystem adapters never call a code address.
fn resolve_kernel(class: ClassId, version: u32) -> Result<u64, Status>
{
    RUNTIME.with(|runtime| runtime.registry.resolve(0, class, version).map_err(status))
           .map_err(crate::state::AccessError::status)?
}

/// Select a provider by declared capabilities, independently of driver names.
pub(crate) fn find_kernel_provider(class: ClassId, version: u32, capabilities: u64)
                                   -> Result<u64, Status>
{
    RUNTIME.with(|runtime| {
               runtime.registry
                      .select(class, version, capabilities)
                      .map_err(status)?
                      .ok_or(NOT_FOUND)
           })
           .map_err(crate::state::AccessError::status)?
}

struct NativeBroker;
impl super::jobs::Broker for NativeBroker
{
    fn invoke(&mut self, request: super::jobs::Call<'_>) -> super::jobs::Outcome
    {
        use super::jobs::{Outcome, Owner, Phase};
        // File Manager jobs carry trusted thread ownership but invoke as the
        // resident service. A module-owned job keeps its original declaration
        // checks; the queue cannot lend it the kernel's authority.
        let caller = match request.owner
        {
            Owner::Module(module) => u64::from(module),
            Owner::Kernel | Owner::Thread(_) => 0
        };
        let mut call = abi::Call { header:          abi::Header::of::<abi::Call>(),
                                   ticket:          *request.ticket,
                                   phase:           match request.phase
                                   {
                                       Phase::Start => abi::START,
                                       Phase::Poll => abi::POLL,
                                       Phase::Cancel => abi::CANCEL
                                   },
                                   class:           request.class.0,
                                   operation:       request.operation,
                                   input:           request.input.as_ptr(),
                                   input_len:       request.input.len() as u32,
                                   output:          request.output.as_mut_ptr(),
                                   output_capacity: request.output.len() as u32,
                                   output_len:      0,
                                   reserved:        0 };
        // SAFETY: Call and both disjoint borrowed slices remain live only for
        // this invocation. checked_call validates the broker wire contract;
        // no registry or job-queue guard survives the callback.
        let result = crate::context::with_io_subject(request.subject.queued(), || {
                         unsafe {
                             invoke_origin(super::registry::requests::Origin::Queued,
                                           caller,
                                           request.provider,
                                           &mut call)
                         }
                     }).and_then(core::convert::identity);
        *request.ticket = call.ticket;
        // Provider errors are Ok(negative_status). Err(Busy) can precede entry
        // or follow a returned callback during registry contention. The ticket
        // retains delivery state and any terminal reply, so retry is correlation,
        // never permission to repeat an already-completed provider effect.
        if result == Err(SERVICE_BUSY)
        {
            return Outcome::Contended;
        }
        let result = result.unwrap_or_else(|status| status);
        match result
        {
            OK => Outcome::Complete(call.output_len as usize),
            abi::PENDING => Outcome::Pending,
            error => Outcome::Failed(error)
        }
    }
}


// Queue owns the audited IRQ/preemption lock boundary, including guard Drop.
// No lock survives a foreign callback; empty pumps are one atomic flag read.
static JOBS: super::jobs::Queue = super::jobs::Queue::new();

pub(crate) fn prepare_recovery_job(owner: &crate::toolbox::handles::OwnerRef)
    -> Result<mac_object_table::store::PublicationReservation<crate::toolbox::handles::InvocationGate>, Status>
{
    JOBS.reserve_publication(owner)
}

pub(crate) fn prepare_recovery_requests(owner: &crate::toolbox::handles::OwnerRef)
    -> Result<[mac_object_table::store::PublicationReservation<crate::toolbox::handles::InvocationGate>; 2], Status>
{
    RUNTIME.with(|runtime| runtime.registry.reserve_recovery_requests(owner).map_err(status))
           .map_err(crate::state::AccessError::status)?
}

fn recovery_subject(subject: &super::jobs::subjects::Subject, interface: u64)
                    -> Result<super::jobs::subjects::Subject, Status>
{
    if !subject.uses_protected_publication()
    {
        return Ok(subject.clone());
    }
    let validated = RUNTIME.with(|runtime| runtime.registry.validate_recovery_graph(interface))
                           .map_err(crate::state::AccessError::status)?;
    match validated
    {
        Ok(()) => Ok(subject.clone()),
        // Select capacity policy before publishing or entering a provider.
        // The dispatcher and canonical identities are the same. The lifetime
        // Activity remains pinned even when the graph needs ordinary memory.
        Err(Error::RecoveryUnsupported) => Ok(subject.with_ordinary_publication()),
        Err(error) => Err(status(error))
    }
}

fn with_job_storage<R>(subject: &super::jobs::subjects::Subject, call: impl FnOnce() -> R) -> R
{
    if subject.uses_protected_publication()
    {
        if let Some(activity) = subject.recovery_activity()
        {
            return activity.with_storage(call);
        }
    }
    crate::context::without_recovery(call)
}

/// Resident adapters enqueue only owned bytes. The owner comes from trusted
/// execution context, never from the application's request payload.
fn enqueue_job(subject: super::jobs::subjects::Subject, owner: super::jobs::Owner, class: ClassId,
               version: u32, operation: u32, input: Vec<u8>, output_bytes: usize)
               -> Result<OwnedJob, Status>
{
    let allocation = subject.allocation().clone();
    crate::allocator::with_target(allocation, || {
        let provider = match owner
        {
            super::jobs::Owner::Module(module) =>
            {
                RUNTIME.with(|runtime| {
                           runtime.registry
                                  .resolve(u64::from(module), class, version)
                                  .map_err(status)
                       })
                       .map_err(crate::state::AccessError::status)??
            }
            _ => resolve_kernel(class, version)?
        };
        let subject = recovery_subject(&subject, provider)?;
        with_job_storage(&subject, || {
            let work = super::jobs::Work::new(provider, class, operation, input, output_bytes)?;
            let (id, receipt) =
                JOBS.submit(&subject, owner, work).map_err(|error| error.status)?.into_parts();
            Ok(OwnedJob { owner,
                          id,
                          receipt,
                          subject: subject.clone() })
        })
    })
}

pub(crate) fn release_job(ticket: mac_object_table::domain::ReleasedResource, storage: bool)
                          -> Result<(), crate::toolbox::handles::cleanup::Failure>
{
    JOBS.release(ticket, storage)
}

pub(crate) fn job_pending(thread: u64, id: u64) -> Result<bool, Status>
{
    JOBS.pending_waiter(thread, id)
}

pub(crate) fn discard_thread_jobs(thread: u64)
{
    let queue = &JOBS;
    queue.discard_owner(super::jobs::Owner::Thread(thread));
    crate::toolbox::files::abandon_provider_thread(thread);
}

/// At most one bounded callback per scheduler sample. Empty queues cost one
/// atomic load, with no registry lookup, allocation or device access.
static DEPENDENCIES_CHANGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
pub(crate) fn dependencies_changed()
{
    DEPENDENCIES_CHANGED.store(true, core::sync::atomic::Ordering::Release);
}
pub(crate) fn pin_backing_provider(dependent: u64, provider: u64,
                                   scope: &mac_object_table::scope::Lease<crate::toolbox::handles::InvocationGate>)
                                   -> Result<(), Status>
{
    RUNTIME.with(|runtime| {
               runtime.registry.pin_backing_provider(dependent, provider, scope).map_err(status)
           })
           .map_err(crate::state::AccessError::status)?
}

static REQUEST_RETIREMENTS: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
static REQUEST_RETIREMENTS_WAITING: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
pub(crate) fn request_retirement_changed()
{
    REQUEST_RETIREMENTS_WAITING.store(true, core::sync::atomic::Ordering::Release);
    REQUEST_RETIREMENTS.store(true, core::sync::atomic::Ordering::Release);
}
/// Actual subscribed lease progress, not a fabricated resource ACK. Only
/// atomic owner cleanup/cancellation checkpoints are requested here.
pub(crate) fn request_progress_changed(owner: mac_object_table::domain::DomainId)
{
    use mac_object_table::domain::DomainKind;
    match owner.kind()
    {
        DomainKind::Process => crate::toolbox::processes::retirement::resources_released(),
        DomainKind::Module => recovery::changed(),
        // The System service root is permanent; owned queued work retains its
        // own cancellation readiness. Modal reuse still waits for actual ACK.
        DomainKind::System => ()
    }
}

pub(crate) fn request_retirement_alias_released()
{
    if REQUEST_RETIREMENTS_WAITING.load(core::sync::atomic::Ordering::Acquire)
    {
        REQUEST_RETIREMENTS.store(true, core::sync::atomic::Ordering::Release);
    }
}
fn retire_request_payload()
{
    if !REQUEST_RETIREMENTS.swap(false, core::sync::atomic::Ordering::AcqRel)
    {
        return;
    }
    match RUNTIME.with(|runtime| {
                     let (retirement, pending) = runtime.registry.take_request_retirement();
                     // Serialize this observation with publication/removal in the same
                     // registry borrow; a later real retirement cannot lose its wake.
                     REQUEST_RETIREMENTS_WAITING.store(pending,
                                                       core::sync::atomic::Ordering::Release);
                     if retirement.is_some() && pending
                     {
                         REQUEST_RETIREMENTS.store(true, core::sync::atomic::Ordering::Release);
                     }
                     retirement
                 })
    {
        Ok(retirement) =>
        {
            // Slot backing and Record die before Removed's actual ACK. Its
            // existing resource_released hook wakes owner/modal retirement.
            drop(retirement);
        }
        Err(_) => request_retirement_changed()
    }
}

pub(crate) fn pump_jobs()
{
    retire_request_payload();
    recovery::pump();
    if DEPENDENCIES_CHANGED.swap(false, core::sync::atomic::Ordering::AcqRel)
       && RUNTIME.with(|runtime| runtime.registry.reap_closed_dependencies()).is_err()
    {
        DEPENDENCIES_CHANGED.store(true, core::sync::atomic::Ordering::Release);
    }

    if !JOBS.has_ready()
    {
        return;
    }
    if let Some(dispatch) = JOBS.next_dispatch()
    {
        dispatch_job(dispatch);
    }
    else
    {
        deliver_job_returns();
    }
}

fn wake_job(notice: super::jobs::CompletionNotice)
{
    if let Some(thread) = notice.waiter
    {
        crate::machine::scheduler::wake_io(thread,
                                           crate::context::DeferredIo::DriverJob(notice.job));
    }
}
fn deliver_job_returns()
{
    if let Some(notice) = JOBS.take_notice()
    {
        wake_job(notice);
    }
    if let Some(failure) = JOBS.take_return_failure()
    {
        serial!("DRIVER_JOB_QUARANTINED job={} owner={:?} {}", failure.job, failure.owner,
                failure.cause);
    }
}
fn dispatch_job(mut dispatch: super::jobs::Dispatch<'_>)
{
    if let Err(status) = dispatch.invoke(&mut NativeBroker)
    {
        serial!("DRIVER_JOB_INVOKE_REFUSED status={status}; dispatch returns through its owned \
                 guard");
    }
    match JOBS.finish(dispatch)
    {
        Ok(Some(notice)) => wake_job(notice),
        Ok(None) => (),
        Err(dispatch) => drop(dispatch) // Its origin queue retains/restores the actual work.
    }
    deliver_job_returns();
}


/// Ask the outer native service boundary to park and replay this call. Resident
/// adapters return SERVICE_BUSY immediately after this succeeds. Nested drawing
/// and definition calls cannot park while their caller may own a manager guard.
fn defer_current_job(id: u64) -> Result<(), Status>
{
    let context = crate::context::current_thread_context();
    job_pending(context.identity().thread.0, id)?;
    context.traps.defer(crate::context::DeferredIo::DriverJob(id))
}


/// A resident adapter owns a job while its request is parked. Dropping the
/// adapter abandons delivery but retains any live transport ticket until the
/// provider acknowledges cancellation; no borrowed application buffer is pinned.
pub struct OwnedJob
{
    owner:   super::jobs::Owner,
    id:      u64,
    subject: super::jobs::subjects::Subject,
    receipt: mac_object_table::store::CompletionKey
}
impl OwnedJob
{
    pub(crate) fn subject(&self) -> &super::jobs::subjects::Subject
    {
        &self.subject
    }

    /// Run one bounded callback of this job immediately. Pending work remains
    /// owned by this guard; Drop requests cancellation through the same queue.
    pub(crate) fn run_once(&mut self) -> Result<Option<super::jobs::Completed>, Status>
    {
        let dispatch = JOBS.dispatch_completion(self.owner, self.receipt)?;
        if let Some(dispatch) = dispatch
        {
            dispatch_job(dispatch);
        }
        self.poll()
    }

    pub fn poll(&mut self) -> Result<Option<super::jobs::Completed>, Status>
    {
        if self.id == 0
        {
            return Err(NOT_FOUND);
        }
        let result = JOBS.take_completion(self.owner, self.receipt)?;
        if result.is_some()
        {
            self.id = 0;
        }
        Ok(result)
    }

    pub fn defer_current(&self) -> Result<(), Status>
    {
        if self.id == 0
        {
            return Err(NOT_FOUND);
        }
        let thread = crate::context::current_thread();
        if thread == crate::context::BOOT_THREAD || thread.0 == 0
        {
            return Err(WRONG_THREAD);
        }
        JOBS.attach_waiter(self.owner, self.id, thread.0)?;
        let result = defer_current_job(self.id);
        if result.is_err()
        {
            let _ = JOBS.detach_waiter(thread.0, self.id);
        }
        result
    }

    /// Request cancellation while retaining the actual result for the service
    /// that owns it. Dropping a job instead abandons result delivery.
    pub fn cancel(&self) -> Result<(), Status>
    {
        if self.id == 0
        {
            return Err(NOT_FOUND);
        }
        let waiter = {
            let queue = &JOBS;
            queue.cancel_completion(self.owner, self.receipt)?;
            queue.completed_waiter_completion(self.owner, self.receipt)?
        };
        if let Some(thread) = waiter
        {
            crate::machine::scheduler::wake_io(thread,
                                               crate::context::DeferredIo::DriverJob(self.id));
        }
        Ok(())
    }
}
impl Drop for OwnedJob
{
    fn drop(&mut self)
    {
        if self.id == 0
        {
            return;
        }
        let queue = &JOBS;
        let _ = queue.discard_completion(self.owner, self.receipt);
    }
}


/// Queue work on behalf of the actual running OS thread. No application request
/// may supply a different owner or module identity.
pub fn submit_job(class: ClassId, version: u32, operation: u32, input: Vec<u8>,
                  output_bytes: usize)
                  -> Result<OwnedJob, Status>
{
    let thread = crate::context::current_thread();
    if thread == crate::context::BOOT_THREAD || thread.0 == 0
    {
        return Err(WRONG_THREAD);
    }
    let subject = super::jobs::subjects::resident()?.admit_current(
        &crate::toolbox::handles::subject::Authority::Client(crate::context::current_owner()?))?;
    enqueue_job(subject,
                super::jobs::Owner::Thread(thread.0),
                class,
                version,
                operation,
                input,
                output_bytes)
}

/// Submit to the exact provider incarnation retained by a resident mount.
pub(crate) fn submit_background_to(subject: &super::jobs::subjects::Subject, provider: u64,
                                   class: ClassId, version: u32, operation: u32, input: Vec<u8>,
                                   output_bytes: usize)
                                   -> Result<OwnedJob, Status>
{
    let allocation = subject.allocation().clone();
    crate::allocator::with_target(allocation, || {
        RUNTIME.with(|runtime| {
                   let interface = runtime.registry.interface(provider).map_err(status)?;
                   if interface.offer.class != class
                      || interface.offer.version != version
                      || input.len() > interface.offer.max_request as usize
                      || output_bytes > interface.offer.max_reply as usize
                   {
                       return Err(BAD_ARGUMENT);
                   }
                   Ok(())
               })
               .map_err(crate::state::AccessError::status)??;
        let subject = recovery_subject(subject, provider)?;
        with_job_storage(&subject, || {
            let work = super::jobs::Work::new(provider, class, operation, input, output_bytes)?;
            let (id, receipt) = JOBS.submit(&subject, super::jobs::Owner::Kernel, work)
                                    .map_err(|error| error.status)?
                                    .into_parts();
            Ok(OwnedJob { owner: super::jobs::Owner::Kernel,
                          id,
                          receipt,
                          subject: subject.clone() })
        })
    })
}

pub(crate) fn abandon_deferred_job(thread: u64, id: u64)
{
    let queue = &JOBS;
    // A kernel-owned operation still has to observe publication and clean its
    // leases if the outer trap cannot park. Abandon only this wake attachment.
    if queue.discard(super::jobs::Owner::Thread(thread), id).is_err()
    {
        let _ = queue.detach_waiter(thread, id);
    }
}


pub(crate) fn release_request(ticket: mac_object_table::domain::ReleasedResource, storage: bool)
                              -> Result<(), crate::toolbox::handles::cleanup::Failure>
{
    use crate::toolbox::handles::cleanup::Failure;
    RUNTIME.with_owned(ticket, |ticket, runtime| runtime.registry.release_request(ticket, storage))
           .map_err(|(cause, ticket)| Failure::request_registry_unavailable(ticket, cause))?
}
