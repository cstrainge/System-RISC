//! Child creation prepares all metadata/backing before making an author runnable.
//! Inert publications remain owned by the original Process checkpoint on refusal.
use mac_object_table::store::PublicationPermit;

use super::*;
use crate::toolbox::handles::InvocationGate;
struct Buffers
{
    threads: Vec<Option<CompletionKey>>,
    waiters: Vec<EventWaiter>
}
impl Buffers
{
    fn prepare() -> Result<Self, Error>
    {
        let required = SCHEDULER.lock()
                                .threads
                                .len()
                                .checked_add(1)
                                .ok_or(Error::Invalid("thread directory length overflows"))?;
        crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
            let mut threads = Vec::new();
            threads.try_reserve_exact(required).map_err(|_| Error::Metadata("shared run queue"))?;
            let mut waiters = Vec::new();
            waiters.try_reserve_exact(required)
                   .map_err(|_| Error::Metadata("shared event snapshot"))?;
            Ok(Self { threads,
                      waiters })
        })
    }

    fn install(&mut self, scheduler: &mut Scheduler) -> Result<usize, Error>
    {
        let slot =
            scheduler.threads.iter().position(Option::is_none).unwrap_or(scheduler.threads.len());
        let required = scheduler.threads
                                .len()
                                .checked_add(usize::from(slot == scheduler.threads.len()))
                                .ok_or(Error::Invalid("thread directory length overflows"))?;
        if scheduler.threads.capacity() < required && self.threads.capacity() < required
           || scheduler.waiters.capacity() < required && self.waiters.capacity() < required
        {
            return Err(Error::Publication(PreparedError::CapacityChanged));
        }
        if scheduler.threads.capacity() < required
        {
            self.threads.extend(scheduler.threads.drain(..));
            core::mem::swap(&mut scheduler.threads, &mut self.threads);
        }
        if scheduler.waiters.capacity() < required
        {
            self.waiters.extend(scheduler.waiters.drain(..));
            core::mem::swap(&mut scheduler.waiters, &mut self.waiters);
        }
        Ok(slot)
    }
}
fn thread_permit(owner: &OwnerRef) -> Result<PublicationPermit<InvocationGate>, Error>
{
    loop
    {
        let plan = {
            let scheduler = SCHEDULER.lock();
            scheduler.records
                     .as_ref()
                     .ok_or(Error::Admission { status: mac_abi::BAD_ABI,
                                               stage:  "canonical THRD Store" })?
                     .plan_publication(owner)?
        };
        let preparation = plan.prepare()?;
        let result = {
            let mut scheduler = SCHEDULER.lock();
            scheduler.records
                     .as_mut()
                     .ok_or(Error::Admission { status: mac_abi::BAD_ABI,
                                               stage:  "canonical THRD Store" })?
                     .admit_prepared(preparation)
        };
        match result
        {
            Ok((permit, old)) =>
            {
                old.dispose();
                return Ok(permit);
            }
            Err(failure) =>
            {
                let cause = failure.cause;
                drop(failure.preparation);
                if cause != PreparedError::CapacityChanged
                {
                    return Err(Error::Publication(cause));
                }
            }
        }
    }
}
struct InertChild
{
    id:     u64,
    right:  u64,
    owner:  OwnerRef,
    active: bool
}
impl Drop for InertChild
{
    fn drop(&mut self)
    {
        if !self.active
        {
            return;
        }
        {
            let scheduler = SCHEDULER.lock();
            if let Some((_, thread)) = scheduler.iter().find(|(_, thread)| thread.id == self.id)
            {
                if !thread.state.get().retired
                {
                    if let Err(cause) = target_retire(&scheduler, thread, &self.owner, false)
                    {
                        // The common walk may own invalidation already. Keep its
                        // original row/backing; never make this author runnable.
                        thread.update(|state| state.live = false);
                        if let Some(completion) = thread.core.application.as_ref()
                        {
                            completion.phase.store(wire::ABORTED, Ordering::Release);
                        }
                        drop(cause);
                    }
                }
            }
        }
        if self.right != 0
        {
            let _ = manager::consume(&self.owner, self.right);
        }
        if !self.owner.accepting()
        {
            crate::toolbox::processes::retirement::resources_released();
        }
    }
}
fn prepare_child(executor: &crate::context::ApplicationExecutor<'_>,
                 gate: &crate::toolbox::native_threads::RuntimeAdmission, entry: usize,
                 data: usize, requested: u64, completion: Arc<Completion>)
                 -> Result<PreparedThread, Error>
{
    let code=crate::kernel::application_execution(executor.owner(),executor.image()).map_err(|status|Error::Admission{status,stage:"shared Process executable image"})?;
    code.validate(executor.owner(), entry).map_err(|status| {
                                               Error::Admission { status,
                                                                  stage: "child entry \
                                                                          executable range" }
                                           })?;
    let charges =
        accounting::ThreadCharges::admit_application(executor.owner(),
                                                     executor.allocation(),
                                                     requested).map_err(Error::Accounting)?;
    let context = Arc::try_new(ThreadContext::new()).map_err(|_| Error::Metadata("child context"))?;
    let slots = gate.prepare_slots().map_err(Error::Tls)?;
    context.install_tls(slots).map_err(|(cause, slots)| {
                                   drop(slots);
                                   Error::Tls(cause)
                               })?;
    let (stack, cpu, allocation) = charges.allocate(executor.owner(), |top| {
                                              let mut frame = TrapFrame::default();
                                              frame.pc = child_start as *const () as usize;
                                              frame.status =
                                                  super::super::super::traps::INITIAL_STATUS;
                                              frame.stack_pointer = top;
                                              frame.registers[8] = entry;
                                              frame.registers[9] = data;
                                              frame
                                          })
                                          .map_err(Error::Accounting)?;
    let top = stack.top();
    let stack =
        super::super::stack::StackBacking::prepare(executor.owner(), stack).map_err(|status| {
            Error::Admission { status,
                               stage: "child independent stack owner" }
        })?;
    let core=Arc::try_new(ThreadCore{
        address:Mutex::new(None),backing:Mutex::new(Some(ThreadBacking{cpu,stack,image:Some(ApplicationImageUse::Child(code))})),
        launch:Mutex::new(None),context,release:Mutex::new(None),binding:executor.image(),top,cleanup_refusal:Mutex::new(None),service_quarantine:spin::Once::new(),name:Mutex::new(None),
        application:Some(completion),pending_join:Mutex::new(None)
    }).map_err(|_|Error::Metadata("child Core"))?;
    Ok(PreparedThread { owner: executor.owner().clone(),
                        core: mac_object_table::retained::Read::new(core, context_released),
                        allocation,
                        recovery: None,
                        heap: executor.heap().clone() })
}
fn publish_inert(prepared: PreparedThread, mut permit: PublicationPermit<InvocationGate>)
                 -> Result<InertChild, Error>
{
    let id = permit.id();
    let mut record = Record::Scheduled(Thread { id,
                                                core: prepared.core.share(),
                                                state: Cell::new(RunState::new()),
                                                worker: None,
                                                scope: None });
    loop
    {
        let mut buffers = Buffers::prepare()?;
        let result = {
            let mut scheduler = SCHEDULER.lock();
            match buffers.install(&mut scheduler)
            {
                Err(cause) => Err((cause, permit, record)),
                Ok(slot) =>
                {
                    let store =
                        scheduler.records.as_mut().ok_or(Error::Admission { status:
                                                                                 mac_abi::BAD_ABI,
                                                                             stage:
                                                                                 "canonical THRD \
                                                                                  Store" })?;
                    match store.publish_prepared(permit, record)
                    {
                        Err(failure) =>
                        {
                            Err((Error::Publication(failure.cause), failure.permit, failure.value))
                        }
                        Ok((publication, old)) =>
                        {
                            let (_, receipt) = publication.into_parts();
                            if slot == scheduler.threads.len()
                            {
                                scheduler.threads.push(Some(receipt));
                            }
                            else
                            {
                                scheduler.threads[slot] = Some(receipt);
                            }
                            let assigned =
                                prepared.core
                                        .backing
                                        .lock()
                                        .as_ref()
                                        .map_or(Err(mac_abi::BAD_ABI), |backing| {
                                            backing.stack.bind(receipt)
                                        })
                                        .and_then(|()| {
                                            prepared.core.context.assign(prepared.owner.clone(),
                                                                         id,
                                                                         prepared.core.binding,
                                                                         &prepared.allocation,
                                                                         &prepared.heap)
                                        });
                            Ok((assigned, old))
                        }
                    }
                }
            }
        };
        // Obsolete buffers/permits/readers never drop under SCHEDULER.
        drop(buffers);
        match result
        {
            Ok((assigned, old)) =>
            {
                old.dispose();
                let guard = InertChild { id,
                                         right: 0,
                                         owner: prepared.owner.clone(),
                                         active: true };
                if let Err(status) = assigned
                {
                    drop(guard);
                    return Err(Error::Admission { status,
                                                  stage: "actual child context/stack binding" });
                }
                return Ok(guard);
            }
            Err((cause, returned_permit, returned_record)) =>
            {
                permit = returned_permit;
                record = returned_record;
                if !matches!(cause, Error::Publication(PreparedError::CapacityChanged))
                {
                    return Err(cause);
                }
            }
        }
    }
}
struct CreateProgress(OwnerRef);
impl Drop for CreateProgress
{
    fn drop(&mut self)
    {
        if !self.0.accepting()
        {
            crate::toolbox::processes::retirement::resources_released();
        }
    }
}
pub(crate) fn create(executor: &crate::context::ApplicationExecutor<'_>, call: &mut wire::Call)
                     -> Result<(), Error>
{
    // Declared first: every actual preparation and root pin drops before this
    // coalesced cleanup hint, including failure before any THRD is published.
    let _progress = CreateProgress(executor.owner().clone());
    let gate = executor.with_tls(|slots| slots.runtime_admission())
                       .map_err(Error::Tls)?
                       .map_err(Error::Tls)?;
    if !gate.accepting()
    {
        return Err(Error::Stopped);
    }
    crate::allocator::with_target(crate::allocator::Target::Owner{account:executor.allocation().clone(),class:mac_kernel_model::policy::MemoryClass::Kernel},||{
        let completion=Arc::try_new(Completion::new(executor.owner())).map_err(|_|Error::Metadata("child completion"))?;
        let (right,right_permit)=manager::prepare(executor.owner(),completion.clone())?;
        let prepared=prepare_child(executor,&gate,call.entry,call.data,call.stack_bytes,completion.clone())?;
        let thread_permit=thread_permit(executor.owner())?;
        let mut guard=publish_inert(prepared,thread_permit)?;
        completion.thread.store(guard.id,Ordering::Release);
        guard.right=manager::publish(right_permit,right.clone())?;
        {
            let scheduler=SCHEDULER.lock();
            let thread=scheduler.iter().find_map(|(_,thread)|(thread.id==guard.id).then_some(thread)).ok_or(Error::Missing)?;
            if !gate.accepting()||thread.state.get().retired||thread.core.context.identity().app.0!=executor.owner().owner().incarnation()
               ||!manager::live(guard.right,&right)?||*right.claimant.lock()!=Claim::Open{return Err(Error::Stopped);}
            // Final transfer: every owning resource exists and no fallible step
            // follows the actual runnable transition. Child may run before OK.
            call.thread=guard.id;call.right=guard.right;call.state=wire::RUNNING;
            completion.phase.store(wire::RUNNING,Ordering::Release);thread.update(|state|state.live=true);
            guard.active=false;
        }
        Ok(())
    })
}
extern "C" fn child_start(entry: usize, data: usize) -> !
{
    let start: unsafe extern "C" fn(*mut u8) = unsafe { core::mem::transmute(entry) };
    // SAFETY: actual same-image ApplicationExecution stays in this THRD's
    // backing. Successful CREATE transferred this opaque data exactly once.
    unsafe {
        start(data as *mut u8);
    }
    let mut call = wire::Call { operation: wire::EXIT,
                                ..Default::default() };
    let status = unsafe {
        super::super::super::traps::gateway(mac_abi::traps::NATIVE_THREAD,
                                            (&mut call as *mut wire::Call).cast(),
                                            size_of::<wire::Call>())
    };
    let detail = call.detail
                     .get(..call.detail_length as usize)
                     .and_then(|bytes| core::str::from_utf8(bytes).ok())
                     .unwrap_or("invalid bounded child EXIT detail");
    app_return_log(format_args!("NATIVE_CHILD_EXIT_RETURNED entry={entry:#x} status={status}; \
                                 {detail}; child TLS/exit contract refused\n"));
    // This actual System-owned process failure service is supplied by the
    // coordinated NativeExit integration; never fall back to old Finder EXIT.
    let mut abort = mac_abi::processes::NativeExitCall { code:     101,
                                                         flags:
                                                             mac_abi::processes::NATIVE_EXIT_ABORT,
                                                         reserved: 0 };
    let returned = unsafe {
        super::super::super::traps::gateway(mac_abi::traps::PROCESS,(&mut abort as *mut mac_abi::processes::NativeExitCall).cast(),size_of::<mac_abi::processes::NativeExitCall>())
    };
    app_return_log(format_args!("NATIVE_PROCESS_ABORT_RETURNED status={returned}; nonreturning \
                                 platform contract lost\n"));
    unsafe {
        core::arch::asm!("unimp", options(noreturn, nostack));
    }
}
