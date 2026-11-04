//! Preadmitted one-shot Window scalar work. Manager birth owners prepare their
//! genuine rollback/close carrier before author NEW can have effects.
use mac_abi::{BAD_ABI,
              definitions::{CALCULATE, DISPOSE, NEW, WDEF, WindowDefinitionState}};
use packet::{Arguments, Object};

use super::*;
use crate::machine::scheduler::callbacks::CallbackThread;

pub(crate) struct PreparedWindowCallback
{
    // Job owns no Queue/Thread. Only this not-yet-published outer carrier holds
    // the canonical weak-Core worker; it disappears after activation.
    worker: CallbackThread,
    job:    Arc<Job>,
    source: super::super::QuiescencePlan,
    entry:  u64
}
impl PreparedWindowCallback
{
    pub(crate) fn prepare(reference: super::super::DefinitionRef, id: i16, variation: i16,
                          message: i16, state: WindowDefinitionState, completion: CompletionTarget)
                          -> Result<Self, Status>
    {
        if !matches!(message, NEW | CALCULATE | DISPOSE)
        {
            return Err(BAD_ARGUMENT);
        }
        let (invocation, entry) = super::super::pin(reference, WDEF, id, false)?;
        let super::super::Entry::User(entry) = entry
        else
        {
            return Err(BAD_ARGUMENT);
        };
        let definition = invocation.definition.as_ref().ok_or(BAD_ARGUMENT)?;
        let super::super::SourceBinding::Image(source) = &definition.source
        else
        {
            return Err(ACCESS_DENIED);
        };
        let worker = crate::machine::scheduler::callbacks::prepare(source)?;
        if worker.owner().owner() != definition.owner.owner()
           || worker.image_id() != source.binding().id()
        {
            return Err(ACCESS_DENIED);
        }
        let identity = PanicIdentity { reference,
                                       kind: definition.identity.kind,
                                       id: definition.identity.id,
                                       owner: definition.owner.owner(),
                                       image_id: source.binding().id() };
        let source_hold = super::super::QuiescencePlan { definition: definition.clone(),
                                                         reference,
                                                         _released:
                                                             super::super::BindingReleased };
        let allocation = definition.source.allocation_target();
        let job = crate::allocator::with_target(allocation, || {
            let mut packet = Request { kind: WDEF,
                                       id,
                                       variation,
                                       message,
                                       reflow: 0,
                                       object: Object::Window(state),
                                       arguments: Arguments::Scalar(0),
                                       surface: None }.prepare(entry)?;
            packet.replace_window(&state)?;
            let packet = Arc::try_new(packet).map_err(|_| NO_MEMORY)?;
            Arc::try_new(Job { identity,
                               completion,
                               complete: AtomicBool::new(false),
                               state: Spinlock::new(JobState { phase:          Phase::New,
                                                               next:           None,
                                                               abandoned:      false,
                                                               packet:         Some(packet),
                                                               invocation:     None,
                                                               reply:          None,
                                                               return_storage:
                                                                   ReturnStorage::Ordinary }),
                               custody: None }).map_err(|_| NO_MEMORY)
        })?;
        // Actual code execution pin is temporary: parked cleanup must not hold
        // it and prevent genuine stopped-source Loading/quiescence admission.
        drop(invocation);
        Ok(Self { worker,
                  job,
                  source: source_hold,
                  entry })
    }

    pub(crate) fn activate(mut self, latest: WindowDefinitionState, completion: CompletionTarget)
                           -> Result<Pending, (Status, Self)>
    {
        let admitted = (|| {
            crate::machine::scheduler::callbacks::validate_worker(&self.worker)?;
            if crate::machine::scheduler::callbacks::authenticate_current()
                .is_ok_and(|current| Arc::ptr_eq(current.queue(), self.worker.queue()))
            {
                return Err(SERVICE_BUSY);
            }
            let identity = self.job.identity;
            let (invocation, entry) =
                super::super::pin(identity.reference, WDEF, identity.id, false)?;
            let super::super::Entry::User(entry) = entry
            else
            {
                return Err(ACCESS_DENIED);
            };
            let definition = invocation.definition.as_ref().ok_or(BAD_ARGUMENT)?;
            if entry != self.entry
               || !Arc::ptr_eq(definition, &self.source.definition)
               || definition.owner.owner() != self.worker.owner().owner()
            {
                return Err(ACCESS_DENIED);
            }
            let super::super::SourceBinding::Image(source) = &definition.source
            else
            {
                return Err(ACCESS_DENIED);
            };
            if source.binding().id() != self.worker.image_id()
            {
                return Err(ACCESS_DENIED);
            }
            // Only this private prepared carrier owns Job/Packet before the
            // first link. No pending reader or callback execution can see them.
            let job = Arc::get_mut(&mut self.job).ok_or(SERVICE_BUSY)?;
            let cell = job.state.get_mut();
            if cell.phase != Phase::New
               || cell.invocation.is_some()
               || cell.reply.is_some()
               || cell.next.is_some()
               || cell.abandoned
            {
                return Err(BAD_ABI);
            }
            let packet = Arc::get_mut(cell.packet.as_mut().ok_or(BAD_ABI)?).ok_or(SERVICE_BUSY)?;
            packet.replace_window(&latest)?;
            // This is a ready hint only, not author execution or an ACK. On a
            // final queue refusal the SAME carrier keeps its latest target.
            let retired_completion = core::mem::replace(&mut job.completion, completion);
            drop(retired_completion);
            Ok(invocation)
        })();
        let invocation = match admitted
        {
            Ok(invocation) => invocation,
            Err(status) => return Err((status, self))
        };
        let mut invocation = Some(invocation);
        if let Err(status) = enqueue_job(self.worker.queue(), &self.job, &mut invocation)
        {
            // Refused commit never moved this real execution pin into Job.
            drop(invocation);
            return Err((status, self));
        }
        crate::machine::scheduler::callbacks::wake(&self.worker);
        let Self { worker,
                   job,
                   source,
                   entry: _ } = self;
        drop(worker);
        drop(source);
        Ok(Pending { job })
    }
}

/// One actual CDEF DISPOSE carrier, fully admitted before linking author work.
/// Activation is allocation-free and consumes this carrier only on success.
pub(crate) struct PreparedControlDisposal
{
    worker:    CallbackThread,
    pending:   Arc<Pending>,
    source:    Arc<super::super::Definition>,
    entry:     u64,
    // The real source Arc must end before the conditional reader-return edge.
    _released: super::super::BindingReleased
}
impl PreparedControlDisposal
{
    pub(crate) fn prepare(reference: super::super::DefinitionRef, request: &Request,
                          completion: CompletionTarget,
                          allocation: &crate::machine::scheduler::accounting::AllocationAuthority)
                          -> Result<Self, Status>
    {
        if request.kind != mac_abi::definitions::CDEF
           || request.message != DISPOSE
           || request.surface.is_some()
           || !matches!(request.arguments, Arguments::Scalar(0))
           || !matches!(request.object, Object::Control(_) | Object::Scrollbar(_))
        {
            return Err(BAD_ARGUMENT);
        }
        let object = match &request.object
        {
            Object::Control(state) => Object::Control(*state),
            Object::Scrollbar(state) => Object::Scrollbar(*state),
            _ => return Err(BAD_ARGUMENT)
        };
        let request = Request { kind: request.kind,
                                id: request.id,
                                variation: request.variation,
                                message: request.message,
                                reflow: request.reflow,
                                object,
                                arguments: Arguments::Scalar(0),
                                surface: None };
        let (invocation, entry) = super::super::pin(reference, request.kind, request.id, false)?;
        let super::super::Entry::User(entry) = entry
        else
        {
            return Err(BAD_ARGUMENT);
        };
        let definition = invocation.definition.as_ref().ok_or(BAD_ARGUMENT)?;
        let super::super::SourceBinding::Image(source) = &definition.source
        else
        {
            return Err(ACCESS_DENIED);
        };
        let worker = crate::machine::scheduler::callbacks::prepare(source)?;
        if worker.owner().owner() != definition.owner.owner()
           || worker.image_id() != source.binding().id()
        {
            return Err(ACCESS_DENIED);
        }
        let identity = PanicIdentity { reference,
                                       kind: definition.identity.kind,
                                       id: definition.identity.id,
                                       owner: definition.owner.owner(),
                                       image_id: source.binding().id() };
        let source_hold = definition.clone();
        let target =
            crate::allocator::Target::Owner { account: allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        let pending = crate::allocator::with_target(target, || {
            let packet = Arc::try_new(request.prepare(entry)?).map_err(|_| NO_MEMORY)?;
            let return_storage = ReturnStorage::prepare(packet.bytes().len())?;
            let job = Arc::try_new(Job { identity,
                                         completion,
                                         complete: AtomicBool::new(false),
                                         state: Spinlock::new(JobState { phase: Phase::New,
                                                                         next: None,
                                                                         abandoned: false,
                                                                         packet:
                                                                             Some(packet),
                                                                         invocation: None,
                                                                         reply: None,
                                                                         return_storage }),
                                         custody: None }).map_err(|_| NO_MEMORY)?;
            Arc::try_new(Pending { job }).map_err(|_| NO_MEMORY)
        })?;
        drop(invocation);
        Ok(Self { worker,
                  pending,
                  source: source_hold,
                  entry,
                  _released: super::super::BindingReleased })
    }

    pub(crate) fn matches(&self, reference: super::super::DefinitionRef) -> bool
    {
        self.pending.job.identity.reference.same(reference)
    }

    pub(crate) fn activate(mut self, latest: &Request, completion: CompletionTarget)
                           -> Result<Arc<Pending>, (Status, Self)>
    {
        let admission = (|| {
            crate::machine::scheduler::callbacks::validate_worker(&self.worker)?;
            if crate::machine::scheduler::callbacks::authenticate_current()
                .is_ok_and(|current| Arc::ptr_eq(current.queue(), self.worker.queue()))
            { return Err(SERVICE_BUSY); }
            let identity = self.pending.job.identity;
            let (invocation, entry) = super::super::pin(identity.reference,
                                                        mac_abi::definitions::CDEF,
                                                        identity.id,
                                                        false)?;
            let super::super::Entry::User(entry) = entry
            else
            {
                return Err(ACCESS_DENIED);
            };
            let definition = invocation.definition.as_ref().ok_or(BAD_ARGUMENT)?;
            if entry != self.entry
               || !Arc::ptr_eq(definition, &self.source)
               || definition.owner.owner() != self.worker.owner().owner()
            {
                return Err(ACCESS_DENIED);
            }
            let super::super::SourceBinding::Image(source) = &definition.source
            else
            {
                return Err(ACCESS_DENIED);
            };
            if source.binding().id() != self.worker.image_id()
            {
                return Err(ACCESS_DENIED);
            }
            let pending = Arc::get_mut(&mut self.pending).ok_or(SERVICE_BUSY)?;
            let job = Arc::get_mut(&mut pending.job).ok_or(SERVICE_BUSY)?;
            let state = job.state.get_mut();
            if state.phase != Phase::New
               || state.invocation.is_some()
               || state.reply.is_some()
               || state.next.is_some()
               || state.abandoned
            {
                return Err(BAD_ABI);
            }
            let packet = Arc::get_mut(state.packet.as_mut().ok_or(BAD_ABI)?).ok_or(SERVICE_BUSY)?;
            packet.replace_control_disposal(latest)?;
            let old = core::mem::replace(&mut job.completion, completion);
            drop(old);
            Ok(invocation)
        })();
        let mut invocation = match admission
        {
            Ok(value) => Some(value),
            Err(status) => return Err((status, self))
        };
        if let Err(status) = enqueue_job(self.worker.queue(), &self.pending.job, &mut invocation)
        {
            drop(invocation);
            return Err((status, self));
        }
        crate::machine::scheduler::callbacks::wake(&self.worker);
        let Self { worker,
                   pending,
                   source,
                   entry: _,
                   _released } = self;
        drop(worker);
        drop(source);
        drop(_released);
        Ok(pending)
    }
}
