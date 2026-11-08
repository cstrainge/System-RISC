//! Real Process callback threads share their original runtime and address space.
//! Queue capabilities retain only Weak Core identity; no Queue/THRD strong cycle.
mod quiescence;
use alloc::sync::{Arc, Weak};

use mac_abi::{ACCESS_DENIED, BAD_ABI, NOT_FOUND, OPERATION_CANCELLED, Status, WRONG_THREAD};
use mac_kernel_model::policy::MemoryClass;
use mac_object_table::retained::Read;
pub(crate) use quiescence::source_quiescence;

use super::*;
use crate::toolbox::resources::definitions::callbacks::{Pending, Queue};

#[derive(Clone)]
pub(crate) struct CallbackThread
{
    owner:  OwnerRef,
    image:  crate::images::Binding,
    thread: crate::context::ThreadId,
    core:   Weak<ThreadCore>,
    queue:  Arc<Queue>
}
impl CallbackThread
{
    pub(crate) fn owner(&self) -> &OwnerRef
    {
        &self.owner
    }

    pub(crate) fn image_id(&self) -> u64
    {
        self.image.id()
    }

    pub(crate) fn queue(&self) -> &Arc<Queue>
    {
        &self.queue
    }
}

/// Only authenticate_current constructs this genuine current execution view.
/// A local retained Core reader ends before the outer trap may schedule/reap.
pub(crate) struct CallbackExecution
{
    owner:  OwnerRef,
    image:  crate::images::Binding,
    thread: crate::context::ThreadId,
    queue:  Arc<Queue>,
    core:   Read<ThreadCore>
}
impl CallbackExecution
{
    pub(crate) fn owner(&self) -> &OwnerRef
    {
        &self.owner
    }

    pub(crate) fn image_id(&self) -> u64
    {
        self.image.id()
    }

    pub(crate) fn thread_id(&self) -> crate::context::ThreadId
    {
        self.thread
    }

    pub(crate) fn queue(&self) -> &Arc<Queue>
    {
        &self.queue
    }
}

fn live(thread: &Thread, owner: &OwnerRef, image: crate::images::Binding) -> bool
{
    let state = thread.state.get();
    owner.accepting()
    && thread.core.binding == image
    && thread.core.context.identity().app.0 == owner.owner().incarnation()
    && !thread.core.context.is_module_worker()
    && state.live
    && !state.retired
    && !state.failure_pending
    && !state.failure_parked
}
fn handle(thread: &Thread, owner: &OwnerRef) -> Option<CallbackThread>
{
    Some(CallbackThread { owner:  owner.clone(),
                          image:  thread.core.binding,
                          thread: crate::context::ThreadId(thread.id),
                          core:   Arc::downgrade(&thread.core),
                          queue:  thread.core.callback_queue.as_ref()?.clone() })
}
fn existing(scheduler: &Scheduler, owner: &OwnerRef, image: crate::images::Binding)
            -> Option<CallbackThread>
{
    scheduler.iter().find_map(|(_, thread)| {
                        (live(thread,owner,image) && thread.core.callback_queue.is_some())
            .then(|| handle(thread,owner)).flatten()
                    })
}

pub(crate) fn authenticate_current() -> Result<CallbackExecution, Status>
{
    crate::context::with_application_executor(|executor| {
        let scheduler = SCHEDULER.lock();
        let thread = scheduler.thread(current()).ok_or(WRONG_THREAD)?;
        if thread.id != executor.thread()
           || !live(thread, executor.owner(), executor.image())
           || thread.state.get().claimed_by != Some(selected_hart())
           || thread.core.context.identity().thread.0 != executor.thread()
        {
            return Err(ACCESS_DENIED);
        }
        let queue = thread.core.callback_queue.as_ref().ok_or(ACCESS_DENIED)?.clone();
        let space = thread.core.space.as_ref().ok_or(ACCESS_DENIED)?;
        if !space.matches(executor.owner(), executor.image())
        {
            return Err(ACCESS_DENIED);
        }
        Ok(CallbackExecution { owner: executor.owner().clone(),
                               image: executor.image(),
                               thread: crate::context::ThreadId(thread.id),
                               queue,
                               core: Read::new(thread.core.clone(), context_released) })
    }).map_err(|cause| cause.status())?
}

/// Revalidate an already prepaid worker without creating a thread or allocating.
/// No queue lock is taken under SCHEDULER; the actual broker activation repeats
/// its own queue admission after this canonical worker/claim observation.
pub(crate) fn validate_worker(worker: &CallbackThread) -> Result<(), Status>
{
    let core = Read::new(worker.core.upgrade().ok_or(OPERATION_CANCELLED)?, context_released);
    let result =
        {
            let scheduler = SCHEDULER.lock();
            match scheduler.iter().find_map(|(_, thread)| {
            (thread.id == worker.thread.0).then_some(thread)
        })
        {
            Some(thread) if live(thread, &worker.owner, worker.image)
                && core::ptr::eq(thread.core.as_ref(), &*core)
                && thread.core.callback_queue.as_ref()
                    .is_some_and(|queue| Arc::ptr_eq(queue, &worker.queue))
                && thread.core.space.as_ref()
                    .is_some_and(|space| space.matches(&worker.owner, worker.image)) => Ok(()),
            _ => Err(OPERATION_CANCELLED)
        }
        };
    // A final genuine Core owner is never destroyed under the scheduler guard.
    drop(core);
    result
}

pub(crate) fn prepare(source: &crate::images::Read) -> Result<CallbackThread, Status>
{
    let owner = source.owner();
    let image = source.binding();
    if !owner.accepting()
       || owner.owner().kind() != mac_object_table::domain::DomainKind::Process
       || image.owner() != owner.owner().incarnation()
    {
        return Err(ACCESS_DENIED);
    }
    {
        let scheduler = SCHEDULER.lock();
        if let Some(worker) = existing(&scheduler, owner, image)
        {
            return Ok(worker);
        }
    }
    let entries = source.runtime_entries()?;
    let (runtime, space) = {
        let scheduler = SCHEDULER.lock();
        if let Some(worker) = existing(&scheduler, owner, image)
        {
            return Ok(worker);
        }
        let thread = scheduler.iter()
                              .find_map(|(_, thread)| live(thread, owner, image).then_some(thread))
                              .ok_or(NOT_FOUND)?;
        let runtime = thread.core.runtime.as_ref().ok_or(BAD_ABI)?;
        let space = thread.core.space.as_ref().ok_or(ACCESS_DENIED)?;
        if !runtime.matches(owner, image)
           || !space.matches(owner, image)
           || runtime.callback_loop() != entries.callback_loop as usize
           || runtime.thread_return() != entries.thread_return as usize
        {
            return Err(ACCESS_DENIED);
        }
        (runtime.clone(), space.clone())
    };
    // The real directory captures charges/heap/TLS from this exact source owner,
    // never from the caller's selected Process or a copied numeric owner label.
    let (actual, charges, heap, tls) = crate::kernel::with_toolbox(|toolbox| {
                                           toolbox.callback_thread_preflight(source)
                                       }).map_err(|cause| cause.status())??;
    if actual.owner() != owner.owner()
       || heap.owner() != owner.owner()
       || charges.allocation().owner() != owner.owner()
    {
        return Err(ACCESS_DENIED);
    }
    let gate = crate::toolbox::native_threads::runtime_admission(&tls, owner).map_err(|cause| {
                                                                                 cause.status()
                                                                             })?;
    if !gate.accepting()
    {
        return Err(OPERATION_CANCELLED);
    }
    let target = crate::allocator::Target::Owner { account: charges.allocation().clone(),
                                                   class:   MemoryClass::Kernel };
    let prepared = crate::context::without_recovery(|| {
        crate::allocator::with_target(target, || {
            let code = crate::kernel::application_execution(owner, image)?;
            code.validate(owner, runtime.callback_loop())?;
            code.validate(owner, runtime.thread_return())?;
            let queue = Queue::new()?;
            let context = Arc::try_new(ThreadContext::new()).map_err(|_| mac_abi::NO_MEMORY)?;
            context.install_callback_stock()?;
            let slots =
                crate::toolbox::native_threads::prepare_slots(owner, &tls).map_err(|cause| {
                                                                              cause.status()
                                                                          })?;
            context.install_tls(slots).map_err(|(cause, slots)| {
                                           drop(slots);
                                           cause.status()
                                       })?;
            let (stack, mut cpu, allocation) = charges.allocate(owner, |_| TrapFrame::default())
                                                      .map_err(|cause| cause.status())?;
            let stack = stack::StackBacking::prepare_user(owner, stack, &space)?;
            let top = stack.user_top()?;
            let mut frame = TrapFrame::default();
            frame.pc = runtime.callback_loop();
            frame.status = crate::machine::traps::USER_STATUS;
            frame.stack_pointer = top;
            frame.registers[0] = runtime.thread_return();
            frame.registers[8] = runtime.descriptor();
            cpu.initialize(frame).map_err(|cause| cause.status())?;
            let core = Arc::try_new(ThreadCore { address:Mutex::new(None),
            backing:Mutex::new(Some(ThreadBacking {cpu,stack,image:Some(ApplicationImageUse::Child { _execution: code })})),
            launch:Mutex::new(None),context,release:Mutex::new(None),binding:image,space:Some(space),top,
            cleanup_refusal:Mutex::new(None),service_quarantine:spin::Once::new(),name:Mutex::new(None),
            application:None,pending_join:Mutex::new(None),pending_pipe:Mutex::new(None),readiness:Mutex::new(readiness::State::default()),user_fault:Mutex::new(None),
            runtime:Some(runtime),callback_queue:Some(queue) }).map_err(|_|mac_abi::NO_MEMORY)?;
            Ok::<_, Status>(PreparedThread { owner: actual,
                                             core: Read::new(core, context_released),
                                             allocation,
                                             recovery: None,
                                             heap })
        })
    })?;
    // No live PTE precedes canonical THRD ownership. Any later refusal retires
    // the same inert record; its real reaper retains mapping/translation ACK.
    let core = Read::new(prepared.core.share(), context_released);
    let id = match spawn_prepared(prepared, false)
    {
        Ok(id) => id,
        Err(failure) =>
        {
            let status = failure.status;
            drop(failure.prepared);
            return Err(status);
        }
    };
    let mut inert = InertCallback { id:     crate::context::ThreadId(id),
                                    core:   &core,
                                    active: true };
    let stack = {
        let backing = core.backing.lock();
        backing.as_ref().ok_or(BAD_ABI)?.stack.clone()
    };
    stack.publish_user().map_err(|cause| {
                             serial!("THREAD_USER_PUBLISH_RETAINED stage=callback_stack \
                                      cause={cause}");
                             user::retirement::refuse(&core,
                                                      user::retirement::Stage::StackPublish,
                                                      cause)
                         })?;
    let mut started = false;
    let worker = {
        let scheduler = SCHEDULER.lock();
        if let Some(worker) = existing(&scheduler, owner, image)
        {
            worker
        }
        else
        {
            let thread =
                scheduler.iter().find_map(|(_, t)| (t.id == id).then_some(t)).ok_or(NOT_FOUND)?;
            let state = thread.state.get();
            if !owner.accepting()
               || !gate.accepting()
               || state.retired
               || state.failure_pending
               || !core::ptr::eq(thread.core.as_ref(), &*core)
               || thread.core.binding != image
            {
                return Err(OPERATION_CANCELLED);
            }
            let worker = handle(thread, owner).ok_or(BAD_ABI)?;
            thread.update(|state| {
                      state.live = true;
                      state.waiting = None;
                      state.woken = 1;
                  });
            inert.active = false;
            started = true;
            worker
        }
    };
    drop(stack);
    drop(inert);
    drop(core);
    if started
    {
        super::wake::runnable(true);
    }
    Ok(worker)
}

struct InertCallback<'a>
{
    id:     crate::context::ThreadId,
    core:   &'a Read<ThreadCore>,
    active: bool
}
impl Drop for InertCallback<'_>
{
    fn drop(&mut self)
    {
        if !self.active
        {
            return;
        }
        let scheduler = SCHEDULER.lock();
        if let Some(thread) =
            scheduler.iter().find_map(|(_, thread)| (thread.id == self.id.0).then_some(thread))
           && core::ptr::eq(thread.core.as_ref(), &**self.core)
        {
            retire(thread);
        }
    }
}

/// The closure must be the broker's atomic readiness read, never a broker lock.
pub(crate) fn park_current(execution: &CallbackExecution, ready: impl FnOnce() -> bool)
                           -> Result<bool, Status>
{
    let scheduler = SCHEDULER.lock();
    let thread = scheduler.thread(current()).ok_or(WRONG_THREAD)?;
    if thread.id != execution.thread.0
       || !live(thread, &execution.owner, execution.image)
       || thread.state.get().claimed_by != Some(selected_hart())
       || !core::ptr::eq(thread.core.as_ref(), &*execution.core)
       || thread.core.callback_queue.as_ref().is_none_or(|q| !Arc::ptr_eq(q, &execution.queue))
    {
        return Err(ACCESS_DENIED);
    }
    service::suspension_allowed(thread.state.get())?;
    if ready()
    {
        return Ok(false);
    }
    thread.update(|state| {
              state.waiting = Some(Wait::Callback);
              state.woken = 0;
          });
    Ok(true)
}

pub(crate) fn wait_call(pending: &Pending) -> Result<bool, Status>
{
    crate::context::with_application_executor(|executor| {
        let scheduler = SCHEDULER.lock();
        let thread = scheduler.thread(current()).ok_or(WRONG_THREAD)?;
        if thread.id != executor.thread()
           || !live(thread, executor.owner(), executor.image())
           || thread.state.get().claimed_by != Some(selected_hart())
        {
            return Err(ACCESS_DENIED);
        }
        service::suspension_allowed(thread.state.get())?;
        if pending.ready()
        {
            return Ok(false);
        }
        thread.update(|state| {
                  state.waiting = Some(Wait::Callback);
                  state.woken = 0;
              });
        Ok(true)
    }).map_err(|cause| cause.status())?
}

/// Commit a native Window caller's wait using only the job's atomic hints.
/// The original actual thread is sealed into the job before author execution;
/// inspecting readiness here never takes a manager/job lock under SCHEDULER.
pub(crate) fn wait_window_call(job: &crate::toolbox::windows::manager::operations::WindowJob)
                               -> Result<bool, Status>
{
    crate::context::with_application_executor(|executor| {
        let scheduler = SCHEDULER.lock();
        let thread = scheduler.thread(current()).ok_or(WRONG_THREAD)?;
        if job.waiter() != Some(crate::context::ThreadId(executor.thread()))
           || thread.id != executor.thread()
           || !live(thread, executor.owner(), executor.image())
           || thread.state.get().claimed_by != Some(selected_hart())
        {
            return Err(ACCESS_DENIED);
        }
        service::suspension_allowed(thread.state.get())?;
        if job.wait_ready()
        {
            return Ok(false);
        }
        thread.update(|state| {
                  state.waiting = Some(Wait::Callback);
                  state.woken = 0;
              });
        Ok(true)
    }).map_err(|cause| cause.status())?
}

/// The trusted close header exposes the same authenticated native wait using
/// only immutable identity and its atomic actual-completion flag under SCHED.
pub(crate) fn wait_trusted_window_call(job: &crate::toolbox::windows::manager::trusted::TrustedCloseHandle)
                                       -> Result<bool, Status>
{
    crate::context::with_application_executor(|executor| {
        let scheduler = SCHEDULER.lock();
        let thread = scheduler.thread(current()).ok_or(WRONG_THREAD)?;
        if job.waiter() != Some(crate::context::ThreadId(executor.thread()))
           || thread.id != executor.thread()
           || !live(thread, executor.owner(), executor.image())
           || thread.state.get().claimed_by != Some(selected_hart())
        {
            return Err(ACCESS_DENIED);
        }
        service::suspension_allowed(thread.state.get())?;
        if job.wait_ready()
        {
            return Ok(false);
        }
        thread.update(|state| {
                  state.waiting = Some(Wait::Callback);
                  state.woken = 0;
              });
        Ok(true)
    }).map_err(|cause| cause.status())?
}

/// Same canonical wait mechanism for an original Control disposal observer.
/// Only sealed immutable waiter identity and atomic hints are read under SCHED.
pub(crate) fn wait_control_disposal(job: &crate::toolbox::controls::objects::ControlDisposal)
                                    -> Result<bool, Status>
{
    crate::context::with_application_executor(|executor| {
        let scheduler = SCHEDULER.lock();
        let thread = scheduler.thread(current()).ok_or(WRONG_THREAD)?;
        if job.waiter() != Some(crate::context::ThreadId(executor.thread()))
           || thread.id != executor.thread()
           || !live(thread, executor.owner(), executor.image())
           || thread.state.get().claimed_by != Some(selected_hart())
        {
            return Err(ACCESS_DENIED);
        }
        service::suspension_allowed(thread.state.get())?;
        if job.wait_ready()
        {
            return Ok(false);
        }
        thread.update(|state| {
                  state.waiting = Some(Wait::Callback);
                  state.woken = 0;
              });
        Ok(true)
    }).map_err(|cause| cause.status())?
}

/// Native Window continuations capture this actual availability epoch before
/// Desktop admission and retain it on refusal. A return before park is observed
/// here; a return after park wakes canonical Desktop waiters in maintenance.
pub(crate) fn wait_desktop(before: crate::kernel::DesktopEpoch) -> Result<bool, Status>
{
    crate::context::with_application_executor(|executor| {
        let scheduler = SCHEDULER.lock();
        let thread = scheduler.thread(current()).ok_or(WRONG_THREAD)?;
        if thread.id != executor.thread()
           || !live(thread, executor.owner(), executor.image())
           || thread.state.get().claimed_by != Some(selected_hart())
        {
            return Err(ACCESS_DENIED);
        }
        service::suspension_allowed(thread.state.get())?;
        if !before.unchanged()?
        {
            return Ok(false);
        }
        thread.update(|state| {
                  state.waiting = Some(Wait::Desktop);
                  state.woken = 0;
              });
        Ok(true)
    }).map_err(|cause| cause.status())?
}

/// Only a genuine armed Desktop holder return requests this bounded scheduler
/// pass. There is no Window registry and no callback/result is manufactured.
pub(crate) fn desktop_returned()
{
    let mut changed = false;
    {
        let scheduler = SCHEDULER.lock();
        for (_, thread) in scheduler.iter()
        {
            let state = thread.state.get();
            if state.live
               && !state.retired
               && !state.failure_pending
               && !state.failure_parked
               && matches!(state.waiting, Some(Wait::Desktop))
            {
                thread.update(|state| {
                          state.waiting = None;
                          state.woken = 1;
                      });
                changed = true;
            }
        }
    }
    if changed
    {
        super::wake::runnable(true);
    }
}

pub(crate) fn wake(worker: &CallbackThread)
{
    let Some(core) = worker.core.upgrade()
    else
    {
        return;
    };
    let core = Read::new(core, context_released);
    let changed = {
        let scheduler = SCHEDULER.lock();
        if let Some(thread) =
            scheduler.iter()
                     .find_map(|(_, thread)| (thread.id == worker.thread.0).then_some(thread))
           && core::ptr::eq(thread.core.as_ref(), &*core)
           && live(thread, &worker.owner, worker.image)
           && thread.core.callback_queue.as_ref().is_some_and(|q| Arc::ptr_eq(q, &worker.queue))
           && matches!(thread.state.get().waiting, Some(Wait::Callback))
        {
            thread.update(|state| {
                      state.waiting = None;
                      state.woken = 1;
                  });
            true
        }
        else
        {
            false
        }
    };
    drop(core);
    if changed
    {
        super::wake::runnable(true);
    }
}

pub(crate) fn wake_waiter(id: crate::context::ThreadId)
{
    let changed = {
        let scheduler = SCHEDULER.lock();
        if let Some(thread) =
            scheduler.iter().find_map(|(_, thread)| (thread.id == id.0).then_some(thread))
           && thread.state.get().live
           && !thread.state.get().retired
           && matches!(thread.state.get().waiting, Some(Wait::Callback))
        {
            thread.update(|state| {
                      state.waiting = None;
                      state.woken = 1;
                  });
            true
        }
        else
        {
            false
        }
    };
    if changed
    {
        super::wake::runnable(true);
    }
}

/// Authenticated ordinary native caller, not a borrowed maintenance/callback role.
/// The wake target retains no Core, Context, source or account strongly.
pub(crate) struct ControlWaitTarget
{
    thread: crate::context::ThreadId,
    owner:  mac_object_table::domain::DomainId,
    image:  crate::images::Binding,
    core:   Weak<ThreadCore>
}
impl ControlWaitTarget
{
    fn matches(&self, thread: &Thread) -> bool
    {
        thread.id == self.thread.0
        && thread.core.context.identity().thread == self.thread
        && thread.core.binding == self.image
        && thread.core.context.identity().app.0 == self.owner.incarnation()
        && Arc::as_ptr(&thread.core) == self.core.as_ptr()
    }
}
pub(crate) struct ControlWaitCaller
{
    target:     ControlWaitTarget,
    allocation: super::accounting::AllocationAuthority
}
impl ControlWaitCaller
{
    pub(crate) fn owner(&self) -> mac_object_table::domain::DomainId
    {
        self.target.owner
    }

    pub(crate) fn allocation(&self) -> &super::accounting::AllocationAuthority
    {
        &self.allocation
    }

    pub(crate) fn target(&self) -> ControlWaitTarget
    {
        ControlWaitTarget { thread: self.target.thread,
                            owner:  self.target.owner,
                            image:  self.target.image,
                            core:   self.target.core.clone() }
    }
}
pub(crate) fn control_wait_caller() -> Result<ControlWaitCaller, Status>
{
    use mac_object_table::shared::InvocationGate as _;
    crate::context::with_application_executor(|executor| {
        crate::toolbox::handles::InvocationGate::run(|| {
            let scheduler = SCHEDULER.lock();
            let thread = scheduler.thread(current()).ok_or(WRONG_THREAD)?;
            if thread.id != executor.thread()
               || !live(thread, executor.owner(), executor.image())
               || thread.state.get().claimed_by != Some(selected_hart())
               || !thread.core
                         .space
                         .as_ref()
                         .is_some_and(|space| space.matches(executor.owner(), executor.image()))
            {
                return Err(ACCESS_DENIED);
            }
            // A running author may itself have to RETURN to release the blocker.
            // Keep the original Busy contract; do not invent a new nested wait.
            if thread.core.callback_queue.is_some()
            {
                return Err(mac_abi::SERVICE_BUSY);
            }
            service::suspension_allowed(thread.state.get())?;
            Ok(ControlWaitCaller { target:
                                       ControlWaitTarget { thread:
                                                               crate::context::ThreadId(thread.id),
                                                           owner:  executor.owner().owner(),
                                                           image:  executor.image(),
                                                           core:   Arc::downgrade(&thread.core) },
                                   allocation: executor.allocation().clone() })
        })
    }).map_err(|cause| cause.status())?
}
pub(crate) fn wait_control_claim(ticket: &crate::toolbox::controls::objects::ClaimWaitTicket)
                                 -> Result<bool, Status>
{
    use mac_object_table::shared::InvocationGate as _;
    crate::context::with_application_executor(|executor| {
        crate::toolbox::handles::InvocationGate::run(|| {
            let scheduler = SCHEDULER.lock();
            let thread = scheduler.thread(current()).ok_or(WRONG_THREAD)?;
            if ticket.node().target().owner != executor.owner().owner()
               || !ticket.node().target().matches(thread)
               || thread.id != executor.thread()
               || !live(thread, executor.owner(), executor.image())
               || thread.state.get().claimed_by != Some(selected_hart())
            {
                return Err(ACCESS_DENIED);
            }
            service::suspension_allowed(thread.state.get())?;
            if !ticket.node().active_for(ticket.epoch())
            {
                return Err(OPERATION_CANCELLED);
            }
            if ticket.ready()
            {
                return Ok(false);
            }
            thread.update(|state| {
                      state.waiting = Some(Wait::Callback);
                      state.woken = 0;
                  });
            Ok(true)
        })
    }).map_err(|cause| cause.status())?
}
/// An immutable registration epoch prevents an old return from waking a reused node.
/// Direct full-key Store lookup avoids adding a scheduler scan per claim return.
pub(crate) fn wake_control_claim(node: &crate::toolbox::controls::objects::WaitNode, epoch: u64)
{
    use mac_object_table::shared::InvocationGate as _;
    let changed = crate::toolbox::handles::InvocationGate::run(|| {
        let scheduler = SCHEDULER.lock();
        let target = node.target();
        if let Some(thread) = scheduler.records
                                       .as_ref()
                                       .and_then(|store| store.get(target.thread.0))
                                       .and_then(Record::scheduled)
           && target.matches(thread)
           && node.ready_for(epoch)
           && thread.state.get().live
           && !thread.state.get().retired
           && matches!(thread.state.get().waiting, Some(Wait::Callback))
        {
            thread.update(|state| {
                      state.waiting = None;
                      state.woken = 1;
                  });
            true
        }
        else
        {
            false
        }
    });
    if changed
    {
        super::wake::runnable(true);
    }
}
