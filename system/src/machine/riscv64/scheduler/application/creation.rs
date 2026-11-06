//! Child creation prepares all metadata/backing before making an author runnable.
//! Inert publications remain owned by the original Process checkpoint on refusal.
use super::*;
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
    let runtime = super::super::user::runtime_for_executor(executor).map_err(|status| {
                      Error::Admission { status,
                                         stage: "child shared user runtime" }
                  })?;
    let space = super::super::application_space(executor).map_err(|status| {
                    Error::Admission { status,
                                       stage: "child canonical Process space" }
                })?;
    let (stack, mut cpu, allocation) =
        charges.allocate(executor.owner(), |_| TrapFrame::default()).map_err(Error::Accounting)?;
    let stack = super::super::stack::StackBacking::prepare_user(executor.owner(), stack, &space)
        .map_err(|status| Error::Admission { status, stage: "child guarded user stack" })?;
    let top = stack.user_top().map_err(|status| {
                                   Error::Admission { status,
                                                      stage: "child user stack geometry" }
                               })?;
    let mut frame = TrapFrame::default();
    frame.pc = entry;
    frame.status = crate::machine::traps::USER_STATUS;
    frame.stack_pointer = top;
    frame.registers[0] = runtime.thread_return();
    frame.registers[8] = data;
    cpu.initialize(frame).map_err(|cause| {
                              Error::Admission { status: cause.status(),
                                                 stage:  "child unpublished CPU initialization" }
                          })?;
    let core=Arc::try_new(ThreadCore{
        address:Mutex::new(None),backing:Mutex::new(Some(ThreadBacking{cpu,stack,image:Some(ApplicationImageUse::Child { _execution: code })})),
        launch:Mutex::new(None),context,release:Mutex::new(None),binding:executor.image(),space:Some(space),top,cleanup_refusal:Mutex::new(None),service_quarantine:spin::Once::new(),user_fault:Mutex::new(None),runtime:Some(runtime),callback_queue:None,name:Mutex::new(None),
        application:Some(completion),pending_join:Mutex::new(None),pending_pipe:Mutex::new(None)
    }).map_err(|_|Error::Metadata("child Core"))?;
    Ok(PreparedThread { owner: executor.owner().clone(),
                        core: mac_object_table::retained::Read::new(core, context_released),
                        allocation,
                        recovery: None,
                        heap: executor.heap().clone() })
}
fn publish_inert(prepared: PreparedThread) -> Result<InertChild, Error>
{
    let published =
        super::super::admission::publish(&prepared, false).map_err(Error::ThreadAdmission)?;
    let guard = InertChild { id:     published.id,
                             right:  0,
                             owner:  prepared.owner.clone(),
                             active: true };
    if let Err(status) = published.assigned
    {
        drop(guard);
        return Err(Error::Admission { status,
                                      stage: "actual child context/stack binding" });
    }
    prepared.core.publish_user_backing().map_err(|status| {
                                             Error::Admission { status,
                                                                stage: "child user mappings \
                                                                        before runnable \
                                                                        publication" }
                                         })?;
    Ok(guard)
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
        let mut guard=publish_inert(prepared)?;
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
