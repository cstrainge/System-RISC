//! User-memory authority comes from the canonical running Process thread.
//! A page-table address or a caller-supplied process identifier is not a grant.
#![forbid(unsafe_code)]

use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{ACCESS_DENIED, OPERATION_CANCELLED, Status, WRONG_THREAD};

use super::{SCHEDULER, current, selected_hart};
mod runtime;
pub(super) mod retirement;
pub(super) use runtime::Runtime;

pub(crate) fn coroutine_stack(call: &mut mac_abi::coroutine_stack::Call, sp: usize)
                              -> Result<(), Status>
{
    use mac_abi::{BAD_ARGUMENT, coroutine_stack as wire};
    if call.reserved != 0 || call.limit != 0
    {
        return Err(BAD_ARGUMENT);
    }
    crate::context::without_recovery(|| {
        let space = current_user_space()?;
        crate::context::with_application_executor(|executor| {
            let backing = {
                let scheduler = SCHEDULER.lock();
                let thread = scheduler.thread(current()).ok_or(WRONG_THREAD)?;
                if thread.id != executor.thread() || space.owner() != executor.owner().owner()
                {
                    return Err(ACCESS_DENIED);
                }
                let backing = thread.core.backing.lock();
                backing.as_ref().map(|entry| entry.stack.clone()).ok_or(WRONG_THREAD)?
            };
            match call.operation
            {
                wire::ALLOCATE if call.base == 0 && call.top == 0 =>
                {
                    (call.base, call.top) =
                        backing.allocate_coroutine(&space, executor.allocation(), call.bytes)?;
                    call.limit = call.base - crate::machine::memory::PAGE_BYTES as usize;
                    Ok(())
                }
                wire::RELEASE if call.bytes == 0 && call.base != 0 && call.base < call.top =>
                {
                    if (call.base..=call.top).contains(&sp)
                    {
                        return Err(WRONG_THREAD);
                    }
                    backing.release_coroutine(call.base, call.top)
                }
                _ => Err(BAD_ARGUMENT)
            }
        }).map_err(|cause| cause.status())?
    })
}

pub(super) fn runtime_for_executor(executor: &crate::context::ApplicationExecutor<'_>)
                                   -> Result<alloc::sync::Arc<Runtime>, Status>
{
    let scheduler = SCHEDULER.lock();
    let thread = scheduler.thread(current()).ok_or(WRONG_THREAD)?;
    if thread.id != executor.thread()
       || thread.core.binding != executor.image()
       || thread.state.get().claimed_by != Some(selected_hart())
       || !thread.state.get().live
       || thread.state.get().retired
       || thread.state.get().failure_pending
       || thread.state.get().failure_parked
       || !executor.owner().accepting()
    {
        return Err(ACCESS_DENIED);
    }
    let runtime = thread.core.runtime.as_ref().ok_or(ACCESS_DENIED)?;
    if !runtime.matches(executor.owner(), executor.image())
    {
        return Err(ACCESS_DENIED);
    }
    Ok(runtime.clone())
}

/// The broker captures this identity from its actual running invocation. The
/// message is the original bounded ABI record, never a pointer into user RAM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DefinitionFault
{
    reference: crate::toolbox::resources::definitions::DefinitionRef,
    kind:      [u8; 4],
    id:        i16,
    image_id:  u64,
    panic:     mac_abi::definitions::DefinitionPanic
}

/// Captured CPU scalars are diagnostic data, never pointers to dereference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FaultSnapshot
{
    pub(crate) cause:     usize,
    pub(crate) pc:        usize,
    pub(crate) address:   usize,
    pub(crate) stack:     usize,
    pub(crate) status:    usize,
    pub(crate) registers: [usize; 30],
    definition:           Option<DefinitionFault>
}

impl core::fmt::Display for FaultSnapshot
{
    fn fmt(&self, output: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        let cause = match self.cause
        {
            0 => "misaligned instruction address",
            1 => "instruction access fault",
            2 => "illegal or privileged instruction",
            3 => "breakpoint",
            4 => "misaligned load",
            5 => "load access fault",
            6 => "misaligned store",
            7 => "store access fault",
            12 => "instruction page fault",
            13 => "load page fault",
            15 => "store page fault",
            _ => "unhandled user exception"
        };
        if let Some(definition) = self.definition
        {
            let kind = core::str::from_utf8(&definition.kind).unwrap_or("????");
            writeln!(output,
                     "User definition panic: {kind} {}, DDEF {:?}, image {}.",
                     definition.id, definition.reference, definition.image_id)?;
            let bytes = definition.panic
                                  .message
                                  .get(..definition.panic.length as usize)
                                  .ok_or(core::fmt::Error)?;
            let message = core::str::from_utf8(bytes).map_err(|_| core::fmt::Error)?;
            writeln!(output, "{message}")?;
            if definition.panic.flags & mac_abi::definitions::DefinitionPanic::TRUNCATED != 0
            {
                writeln!(output, "The definition panic formatter truncated its message.")?;
            }
            if definition.panic.flags & mac_abi::definitions::DefinitionPanic::FORMAT_FAILED != 0
            {
                writeln!(output, "The definition panic formatter reported a formatting failure.")?;
            }
        }
        else
        {
            writeln!(output, "User fault: {cause} (cause {}).", self.cause)?;
        }
        writeln!(output, "PC: {:#018x}  address: {:#018x}", self.pc, self.address)?;
        writeln!(output, "SP: {:#018x}  status: {:#018x}", self.stack, self.status)?;
        for (name, value) in ["ra", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2",
                              "a3", "a4", "a5", "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7",
                              "s8", "s9", "s10", "s11", "t3", "t4", "t5", "t6"].into_iter()
                                                                               .zip(self.registers)
        {
            writeln!(output, "{name}: {value:#018x}")?;
        }
        Ok(())
    }
}

impl FaultSnapshot
{
    /// Architecture-owned classification of captured exception identity; UI
    /// code must not classify the human diagnostic text.
    pub(crate) fn is_access_fault(&self) -> bool
    {
        self.definition.is_none() && matches!(self.cause, 0 | 1 | 4 | 5 | 6 | 7 | 12 | 13 | 15)
    }

    pub(crate) fn is_definition_panic(&self) -> bool
    {
        self.definition.is_some()
    }
}

/// Only capture of a genuine current application claim can mint this target.
/// It owns the original Process authority, not the current ThreadCore itself.
pub(crate) struct Fault
{
    owner:    crate::toolbox::handles::OwnerRef,
    image:    crate::images::Binding,
    thread:   crate::context::ThreadId,
    snapshot: FaultSnapshot
}

impl Fault
{
    pub(crate) fn owner(&self) -> &crate::toolbox::handles::OwnerRef
    {
        &self.owner
    }

    pub(crate) fn image(&self) -> crate::images::Binding
    {
        self.image
    }

    pub(crate) fn thread(&self) -> crate::context::ThreadId
    {
        self.thread
    }

    pub(crate) fn snapshot(&self) -> FaultSnapshot
    {
        self.snapshot
    }
}

static FAULT_WAKE: AtomicBool = AtomicBool::new(false);
static FAULT_RETAINED: AtomicBool = AtomicBool::new(false);
static FAULT_RUNNING: AtomicBool = AtomicBool::new(false);

/// A real resource-release/maintenance edge may retry a retained admission.
pub(crate) fn faults_changed()
{
    if FAULT_RETAINED.load(Ordering::Acquire)
    {
        FAULT_WAKE.store(true, Ordering::Release);
        super::wake::request();
    }
}

/// The caller has authenticated a hardware U-origin outer frame. Capture its
/// bounded report in prepaid THRD storage, then prevent all sibling authors
/// from resuming. No user pointer, allocation or Desktop borrow is required.
pub(crate) fn capture_fault(frame: &super::TrapFrame) -> Result<(), Status>
{
    capture(frame, None)
}

pub(crate) fn capture_definition_panic(frame: &super::TrapFrame,
                                       execution: &super::callbacks::CallbackExecution,
                                       identity: &crate::toolbox::resources::definitions::callbacks::PanicIdentity,
                                       panic: &mac_abi::definitions::DefinitionPanic)
                                       -> Result<(), Status>
{
    let message = panic.message.get(..panic.length as usize).ok_or(mac_abi::BAD_ARGUMENT)?;
    if panic.flags
       & !(mac_abi::definitions::DefinitionPanic::TRUNCATED
           | mac_abi::definitions::DefinitionPanic::FORMAT_FAILED)
       != 0
       || core::str::from_utf8(message).is_err()
    {
        return Err(mac_abi::BAD_ARGUMENT);
    }
    if identity.owner != execution.owner().owner()
       || identity.image_id != execution.image_id()
       || execution.thread_id() != crate::context::current_thread_context().identity().thread
    {
        return Err(ACCESS_DENIED);
    }
    capture(frame,
            Some(DefinitionFault { reference: identity.reference,
                                   kind:      identity.kind,
                                   id:        identity.id,
                                   image_id:  identity.image_id,
                                   panic:     *panic }))
}

fn capture(frame: &super::TrapFrame, definition: Option<DefinitionFault>) -> Result<(), Status>
{
    // Fault containment authenticates the actual hardware execution claim,
    // including one that is already stopping. Ordinary user-memory/service
    // admission remains stricter; rejection there cannot classify this fault
    // as privileged or permit a dying author to kill the System.
    if frame.prior_depth != 0 || frame.status & (1 << 8) != 0
    {
        return Err(ACCESS_DENIED);
    }
    crate::context::with_application_executor(|executor| {
        let scheduler = SCHEDULER.lock();
        let thread = scheduler.thread(current()).ok_or(WRONG_THREAD)?;
        let runtime = thread.core.runtime.as_ref().ok_or(mac_abi::BAD_ABI)?;
        if thread.id != executor.thread()
           || thread.state.get().claimed_by != Some(selected_hart())
           || thread.core.context.identity().thread.0 != executor.thread()
           || thread.core.context.identity().app.0 != executor.owner().owner().incarnation()
           || thread.core.binding != executor.image()
           || thread.core.context.is_module_worker()
           || thread.core.cpu_frame() != Some(core::ptr::from_ref(frame).cast_mut())
           || !runtime.matches(executor.owner(), executor.image())
           || !thread.core
                     .space
                     .as_ref()
                     .is_some_and(|space| space.matches(executor.owner(), executor.image()))
        {
            return Err(ACCESS_DENIED);
        }
        let mut pending = thread.core.user_fault.lock();
        if runtime.claim_fault()
        {
            *pending = Some(Fault { owner:    executor.owner().clone(),
                                    image:    executor.image(),
                                    thread:   crate::context::ThreadId(executor.thread()),
                                    snapshot: FaultSnapshot { cause: frame.cause,
                                                              pc: frame.pc,
                                                              address: frame.value,
                                                              stack: frame.stack_pointer,
                                                              status: frame.status,
                                                              registers: frame.registers,
                                                              definition } });
        }
        drop(pending);
        for (_, sibling) in scheduler.iter().filter(|(_, sibling)| {
                                                sibling.core.binding.owner()
                                                == executor.owner().owner().incarnation()
                                                && !sibling.core.context.is_module_worker()
                                            })
        {
            sibling.update(|state| {
                       state.failure_pending = true;
                       if !state.service.quiescent()
                       {
                           state.waiting = None;
                           state.woken = state.woken.max(1);
                       }
                   });
        }
        FAULT_RETAINED.store(true, Ordering::Release);
        FAULT_WAKE.store(true, Ordering::Release);
        super::wake::request();
        Ok(())
    }).map_err(|cause| cause.status())?
}

/// Queue trusted termination outside scheduler/manager guards. Refusal keeps
/// the same bounded fault and authority with its original parked thread.
pub(crate) fn pump_faults()
{
    if !FAULT_WAKE.swap(false, Ordering::AcqRel)
    {
        return;
    }
    if FAULT_RUNNING.swap(true, Ordering::AcqRel)
    {
        FAULT_WAKE.store(true, Ordering::Release);
        return;
    }
    struct Running;
    impl Drop for Running
    {
        fn drop(&mut self)
        {
            FAULT_RUNNING.store(false, Ordering::Release);
        }
    }
    let _running = Running;
    let mut after = 0;
    let mut retained = false;
    loop
    {
        let core = {
            let scheduler = SCHEDULER.lock();
            scheduler.iter().find_map(|(slot, thread)| {
                                (slot >= after && thread.core.user_fault.lock().is_some()).then(|| {
                    after = slot + 1;
                    mac_object_table::retained::Read::new(thread.core.clone(), super::context_released)
                })
                            })
        };
        let Some(core) = core
        else
        {
            break;
        };
        let pending = core.user_fault.lock().take();
        let Some(fault) = pending
        else
        {
            continue;
        };
        if let Err(status) = crate::toolbox::processes::termination::queue_user_fault(&fault)
        {
            // The single pump owns this detached value; the faulted author
            // cannot execute again or construct a second pending value here.
            *core.user_fault.lock() = Some(fault);
            retained = true;
            serial!("USER_FAULT_ADMISSION_RETAINED status={status}; author remains parked");
        }
    }
    FAULT_RETAINED.store(retained, Ordering::Release);
}

/// Retain the actual outgoing thread's independently owned address space.
/// Context validation rejects System, module and definition execution before
/// consulting the scheduler; the scheduler then authenticates the live claim.
pub(crate) fn current_user_space() -> Result<crate::machine::paging::process::Pin, Status>
{
    crate::context::with_application_executor(|executor| {
        if !executor.owner().accepting()
        {
            return Err(OPERATION_CANCELLED);
        }
        let scheduler = SCHEDULER.lock();
        let thread = scheduler.thread(current()).ok_or(WRONG_THREAD)?;
        let state = thread.state.get();
        if thread.id != executor.thread()
           || thread.core.binding != executor.image()
           || thread.core.context.identity().thread.0 != executor.thread()
           || thread.core.context.identity().app.0 != executor.owner().owner().incarnation()
           || thread.core.context.is_module_worker()
           || state.claimed_by != Some(selected_hart())
        {
            return Err(ACCESS_DENIED);
        }
        if !state.live || state.retired || state.failure_parked
        {
            return Err(OPERATION_CANCELLED);
        }
        let space = thread.core.space.as_ref().ok_or(ACCESS_DENIED)?;
        if !space.matches(executor.owner(), executor.image())
        {
            return Err(ACCESS_DENIED);
        }
        Ok(space.clone())
    }).map_err(|cause| cause.status())?
}
