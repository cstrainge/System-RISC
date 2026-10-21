//! Executable module boundary. RAM reservations, entry casts and foreign slice
//! borrows stay here; admission/ownership and all registry logic are safe Rust.
use alloc::{boxed::Box, vec::Vec};

use mac_abi::{ACCESS_DENIED, BAD_ABI, BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, NOT_SUPPORTED, OK,
              SERVICE_BUSY, Status, WRONG_THREAD};
use mac_formats::{module::Package, native::image, resource::Resource};
use mac_module::{ClassId, PackageKind, Target, abi};

use super::{Error, Registry};
use crate::state::TryState;
mod discovery;
pub(crate) use discovery::discover;
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
    module:   u32,
    base:     u64,
    limit:    u64,
    _host:    Box<abi::Host>,
    instance: Option<abi::Instance>
}
/// Class adapters receive checked fresh borrows, never C pointers or registry
/// guards. They may return Pending but cannot retain either borrowed buffer.
pub(crate) struct ResidentCall<'a>
{
    pub caller:    u32,
    pub ticket:    u32,
    pub phase:     u32,
    pub operation: u32,
    pub input:     &'a [u8]
}
pub(crate) enum ResidentCompletion
{
    Complete(usize),
    Pending
}
pub(crate) type ResidentCallback =
    fn(ResidentCall<'_>, &mut [u8]) -> Result<ResidentCompletion, Status>;
struct ResidentBinding
{
    interface: u32,
    callback:  ResidentCallback
}
#[derive(Clone, Copy)]
enum DispatchTarget
{
    Module(abi::Instance),
    Resident(ResidentCallback)
}

pub(crate) fn register_service(offer: mac_module::Offer, callback: ResidentCallback)
                               -> Result<u32, Status>
{
    RUNTIME.with(|runtime| {
               runtime.residents.try_reserve(1).map_err(|_| NO_MEMORY)?;
               let interface = runtime.registry.register_service(offer).map_err(status)?;
               runtime.residents.push(ResidentBinding { interface,
                                                        callback });
               Ok(interface)
           })
           .map_err(crate::state::AccessError::status)?
}

pub(crate) fn device_allowed(module: u32, bus: ClassId, key: [u8; 16]) -> Result<(), Status>
{
    RUNTIME.with(|runtime| runtime.registry.device_allowed(module, bus, key).map_err(status))
           .map_err(crate::state::AccessError::status)?
}

pub(crate) fn provider_module(interface: u32) -> Result<u32, Status>
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

/// Only the admitted module's pinned load reservation can be offered for DMA.
/// The bus adapter still checks device ownership and maps it for that device.
pub(crate) fn owns_memory(module: u32, base: u64, length: u64) -> Result<(), Status>
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
    RUNTIME.publish(Runtime { registry:  Registry::new(target),
                              bindings:  Vec::new(),
                              residents: Vec::new(),
                              next:      arena.start,
                              limit:     arena.end })
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
    let (module, ticket, target) =
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
                   let target = if interface.module == 0
                   {
                       DispatchTarget::Resident(runtime.residents
                                                       .iter()
                                                       .find(|r| r.interface == provider)
                                                       .ok_or(NOT_FOUND)?
                                                       .callback)
                   }
                   else
                   {
                       DispatchTarget::Module(runtime.bindings
                                                     .iter()
                                                     .find(|b| b.module == interface.module)
                                                     .and_then(|b| b.instance)
                                                     .ok_or(NOT_FOUND)?)
                   };
                   runtime.registry.enter_request(caller, ticket).map_err(status)?;
                   Ok((interface.module, ticket, target))
               })
               .map_err(crate::state::AccessError::status)??;
    call.ticket = ticket;
    let original = *call;
    let result = match target
    {
        DispatchTarget::Module(instance) =>
        {
            let dispatch = instance.dispatch.ok_or(NOT_SUPPORTED)?;
            unsafe { dispatch(instance.state, call) }
        }
        DispatchTarget::Resident(callback) =>
        {
            // checked_call validated lengths, non-null pointers and disjoint
            // live borrows. No registry guard survives into this safe callback.
            let input = if call.input_len == 0
            {
                &[]
            }
            else
            {
                unsafe { core::slice::from_raw_parts(call.input, call.input_len as usize) }
            };
            let output = if call.output_capacity == 0
            {
                &mut []
            }
            else
            {
                unsafe {
                    core::slice::from_raw_parts_mut(call.output, call.output_capacity as usize)
                }
            };
            match callback(ResidentCall { caller: u32::try_from(caller).map_err(|_| {
                                                                           ACCESS_DENIED
                                                                       })?,
                                          ticket,
                                          phase: call.phase,
                                          operation: call.operation,
                                          input },
                           output)
            {
                Ok(ResidentCompletion::Complete(length)) if length <= output.len() =>
                {
                    call.output_len = length as u32;
                    OK
                }
                Ok(ResidentCompletion::Complete(_)) => BAD_ABI,
                Ok(ResidentCompletion::Pending) => abi::PENDING,
                Err(status) if status < 0 => status,
                Err(_) => BAD_ABI
            }
        }
    };
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
    // The mutable field denotes a live request, not a historical identifier.
    // In particular, callers can distinguish pre-dispatch contention (retained
    // ticket) from a terminal provider error (cleared ticket).
    call.ticket = 0;
    if malformed || result > 0
    {
        call.output_len = 0;
        return Err(BAD_ABI);
    }
    Ok(result)
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
/// A kernel caller still resolves a live, declared class/version through the
/// same registry as modules. Filesystem adapters never call a code address.
fn resolve_kernel(class: ClassId, version: u32) -> Result<u32, Status>
{
    RUNTIME.with(|runtime| runtime.registry.resolve(0, class, version).map_err(status))
           .map_err(crate::state::AccessError::status)?
}

/// Select a provider by declared capabilities, independently of driver names.
pub(crate) fn find_kernel_provider(class: ClassId, version: u32, capabilities: u64)
                                   -> Result<u32, Status>
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
        let result = unsafe { invoke(caller, request.provider, &mut call) };
        *request.ticket = call.ticket;
        // Provider errors are Ok(negative_status). Err(Busy) is admission or
        // registry contention: the callback was not dispatched at this boundary.
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


// These short queue critical sections run only at the interrupt-masked native
// service/timer boundary. No callback or hardware poll holds this guard. This
// follows the current one-hart scheduler contract; SMP must supply per-hart
// interrupt exclusion before it enables concurrent preemption in the kernel.
static JOBS: spinning_top::Spinlock<super::jobs::Queue> =
    spinning_top::Spinlock::new(super::jobs::Queue::new());
static JOBS_READY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Resident adapters enqueue only owned bytes. The owner comes from trusted
/// execution context, never from the application's request payload.
fn enqueue_job(owner: super::jobs::Owner, class: ClassId, version: u32, operation: u32,
               input: Vec<u8>, output_bytes: usize)
               -> Result<OwnedJob, Status>
{
    use core::sync::atomic::Ordering::Release;
    let provider = match owner
    {
        super::jobs::Owner::Module(module) =>
        {
            RUNTIME.with(|runtime| {
                       runtime.registry.resolve(u64::from(module), class, version).map_err(status)
                   })
                   .map_err(crate::state::AccessError::status)??
        }
        _ => resolve_kernel(class, version)?
    };
    let work = super::jobs::Work::new(provider, class, operation, input, output_bytes)?;
    let mut queue = JOBS.lock();
    let id = queue.submit(owner, work).map_err(|error| error.status)?;
    JOBS_READY.store(true, Release);
    Ok(OwnedJob { owner,
                  id })
}

fn take_job(owner: super::jobs::Owner, id: u32) -> Result<Option<super::jobs::Completed>, Status>
{
    JOBS.lock().take(owner, id)
}

pub(crate) fn job_pending(thread: u64, id: u32) -> Result<bool, Status>
{
    JOBS.lock().pending_waiter(thread, id)
}

pub(crate) fn discard_thread_jobs(thread: u64)
{
    let mut queue = JOBS.lock();
    queue.discard_owner(super::jobs::Owner::Thread(thread));
    JOBS_READY.store(queue.has_ready(), core::sync::atomic::Ordering::Release);
    drop(queue);
    crate::toolbox::files::abandon_provider_thread(thread);
}

/// At most one bounded callback per scheduler sample. Empty queues cost one
/// atomic load, with no registry lookup, allocation or device access.
pub(crate) fn pump_jobs()
{
    use core::sync::atomic::Ordering::{Acquire, Release};
    if !JOBS_READY.load(Acquire)
    {
        return;
    }
    let dispatch = { JOBS.lock().next_dispatch() };
    let Some(mut dispatch) = dispatch
    else
    {
        return;
    };
    dispatch.invoke(&mut NativeBroker);
    let completed = {
        let mut queue = JOBS.lock();
        let completed = queue.finish(dispatch).expect("dispatch belongs to its live job queue");
        JOBS_READY.store(queue.has_ready(), Release);
        completed
    };
    if let Some(super::jobs::CompletionNotice { waiter: Some(thread),
                                                job,
                                                .. }) = completed
    {
        crate::machine::scheduler::wake_io(thread, job);
    }
}


/// Ask the outer native service boundary to park and replay this call. Resident
/// adapters return SERVICE_BUSY immediately after this succeeds. Nested drawing
/// and definition calls cannot park while their caller may own a manager guard.
fn defer_current_job(id: u32) -> Result<(), Status>
{
    let context = crate::context::current_thread_context();
    job_pending(context.identity().thread.0, id)?;
    context.traps.defer(id)
}


/// A resident adapter owns a job while its request is parked. Dropping the
/// adapter abandons delivery but retains any live transport ticket until the
/// provider acknowledges cancellation; no borrowed application buffer is pinned.
pub struct OwnedJob
{
    owner: super::jobs::Owner,
    id:    u32
}
impl OwnedJob
{
    pub fn poll(&mut self) -> Result<Option<super::jobs::Completed>, Status>
    {
        if self.id == 0
        {
            return Err(NOT_FOUND);
        }
        let result = take_job(self.owner, self.id)?;
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
        JOBS.lock().attach_waiter(self.owner, self.id, thread.0)?;
        let result = defer_current_job(self.id);
        if result.is_err()
        {
            let _ = JOBS.lock().detach_waiter(thread.0, self.id);
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
            let mut queue = JOBS.lock();
            queue.cancel(self.owner, self.id)?;
            JOBS_READY.store(queue.has_ready(), core::sync::atomic::Ordering::Release);
            queue.completed_waiter(self.owner, self.id)?
        };
        if let Some(thread) = waiter
        {
            crate::machine::scheduler::wake_io(thread, self.id);
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
        let mut queue = JOBS.lock();
        let _ = queue.discard(self.owner, self.id);
        JOBS_READY.store(queue.has_ready(), core::sync::atomic::Ordering::Release);
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
    enqueue_job(super::jobs::Owner::Thread(thread.0),
                class,
                version,
                operation,
                input,
                output_bytes)
}

/// Background resident services own their own result and cancellation lifetime.
pub fn submit_background_job(class: ClassId, version: u32, operation: u32, input: Vec<u8>,
                             output_bytes: usize)
                             -> Result<OwnedJob, Status>
{
    enqueue_job(super::jobs::Owner::Kernel, class, version, operation, input, output_bytes)
}

/// Submit to the exact provider incarnation retained by a resident mount.
pub(crate) fn submit_background_to(provider: u32, class: ClassId, version: u32, operation: u32,
                                   input: Vec<u8>, output_bytes: usize)
                                   -> Result<OwnedJob, Status>
{
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
    let work = super::jobs::Work::new(provider, class, operation, input, output_bytes)?;
    let mut queue = JOBS.lock();
    let id = queue.submit(super::jobs::Owner::Kernel, work).map_err(|error| error.status)?;
    JOBS_READY.store(true, core::sync::atomic::Ordering::Release);
    Ok(OwnedJob { owner: super::jobs::Owner::Kernel,
                  id })
}

pub(crate) fn abandon_deferred_job(thread: u64, id: u32)
{
    let mut queue = JOBS.lock();
    // A kernel-owned operation still has to observe publication and clean its
    // leases if the outer trap cannot park. Abandon only this wake attachment.
    if queue.discard(super::jobs::Owner::Thread(thread), id).is_err()
    {
        let _ = queue.detach_waiter(thread, id);
    }
    JOBS_READY.store(queue.has_ready(), core::sync::atomic::Ordering::Release);
}
