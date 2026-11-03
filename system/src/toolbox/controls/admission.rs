//! Detached ordinary CTRL payload and canonical Store publication. CDEF
//! validation keeps its existing Desktop turn; no author callback runs here
//! during allocation, capacity refresh, admission or publication.
#![forbid(unsafe_code)]
use core::fmt;

use mac_object_table::{domain::{DomainKind, PoolError},
                       store::{PreparedError, PreparedPublication, PublicationPermit,
                               PublicationPlan, RetiredCapacity, RetiredPublication}};

use super::*;
use crate::{machine::scheduler::accounting::AllocationAuthority, toolbox::handles::InvocationGate};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CreationError
{
    InvalidRequest,
    Authority,
    DefinitionChanged,
    Validation(Status),
    Title(Status),
    Binding(Status),
    Facility(PoolError),
    Preparation(PreparedError)
}
impl CreationError
{
    pub(crate) fn status(self) -> Status
    {
        match self
        {
            Self::InvalidRequest => BAD_ARGUMENT,
            Self::Authority => ACCESS_DENIED,
            Self::DefinitionChanged => SERVICE_BUSY,
            Self::Validation(status) | Self::Title(status) | Self::Binding(status) => status,
            Self::Facility(PoolError::ForeignOwner { .. })
            | Self::Preparation(PreparedError::Pool(PoolError::ForeignOwner { .. })) => ACCESS_DENIED,
            Self::Facility(cause) | Self::Preparation(PreparedError::Pool(cause)) =>
            {
                crate::toolbox::handles::registry_status(&mac_object_table::registry::Error::Pool(cause))
            }
            Self::Preparation(PreparedError::CapacityChanged) => SERVICE_BUSY,
            Self::Preparation(PreparedError::WrongStore | PreparedError::RetiredStore) => BAD_ABI
        }
    }

    pub(crate) fn is_capacity_changed(self) -> bool
    {
        matches!(self, Self::Preparation(PreparedError::CapacityChanged))
    }
}
impl fmt::Display for CreationError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::InvalidRequest => out.write_str("control creation request changed or is invalid"),
            Self::Authority =>
            {
                out.write_str("ordinary control creation requires its original Process recipient \
                               and storage owner")
            }
            Self::DefinitionChanged =>
            {
                out.write_str("control definition changed before publication")
            }
            Self::Validation(status) =>
            {
                write!(out, "control definition validation refused with status {status}")
            }
            Self::Title(status) =>
            {
                write!(out, "control title admission refused with status {status}")
            }
            Self::Binding(status) =>
            {
                write!(out, "control definition binding refused with status {status}")
            }
            Self::Facility(cause) => cause.fmt(out),
            Self::Preparation(cause) => cause.fmt(out)
        }
    }
}

/// Only creation inputs select the payload. IDs, flags, timing and hit-test
/// outputs are ignored by the existing CREATE operations and are not authority.
#[derive(Clone, Copy)]
struct Intent
{
    operation: u32,
    window:    u64,
    spec:      Spec,
    reflow:    u32
}
impl Intent
{
    fn new(call: &ControlCall) -> Self
    {
        Self { operation: call.operation,
               window:    call.window,
               spec:      Spec::from_call(call, call.operation == CREATE_SCROLLBAR),
               reflow:    call.reflow }
    }

    fn call(&self) -> ControlCall
    {
        let mut call = self.spec.draw();
        call.operation = self.operation;
        call.reflow = self.reflow;
        call.window = self.window;
        call
    }

    fn matches(&self, call: &ControlCall) -> bool
    {
        call.surface.is_null()
        && call.reserved == 0
        && self.operation == call.operation
        && self.window == call.window
        && self.reflow == call.reflow
        && self.spec == Spec::from_call(call, call.operation == CREATE_SCROLLBAR)
    }
}

pub(crate) struct ControlPlan
{
    publication:    PublicationPlan<Object, InvocationGate, automatic::Work>,
    cleanup_notice: Arc<crate::toolbox::resources::definitions::callbacks::Notice>,
    cleanup:        Option<super::disposal::ControlCleanup>,
    recipient:      OwnerRef,
    storage:        OwnerRef,
    intent:         Intent,
    validated:      Spec,
    scrollbar:      bool,
    entry:          DefinitionRef,
    gate:           TrapGate,
    allocation:     AllocationAuthority
}
#[must_use = "retain or dispose control preparation outside Desktop"]
pub(crate) struct ControlPreparation
{
    object:     Object,
    cleanup:    Option<Arc<super::disposal::DisposalSlot>>,
    capacity:   ControlCapacity,
    recipient:  OwnerRef,
    storage:    OwnerRef,
    intent:     Intent,
    gate:       TrapGate,
    allocation: AllocationAuthority
}
#[must_use = "retain the original control payload and canonical permit"]
pub(crate) struct PreparedControl
{
    object:     Object,
    cleanup:    Option<Arc<super::disposal::DisposalSlot>>,
    admission:  RetiredCapacity<Object, InvocationGate, automatic::Work>,
    permit:     PublicationPermit<InvocationGate>,
    recipient:  OwnerRef,
    storage:    OwnerRef,
    intent:     Intent,
    gate:       TrapGate,
    allocation: AllocationAuthority
}
pub(crate) struct ControlCapacityPlan
{
    publication: PublicationPlan<Object, InvocationGate, automatic::Work>,
    allocation:  AllocationAuthority
}
#[must_use = "dispose refused or obsolete control capacity outside Desktop"]
pub(crate) struct ControlCapacity
{
    publication: PreparedPublication<Object, InvocationGate, automatic::Work>
}
#[must_use = "dispose control publication metadata outside Desktop"]
pub(crate) struct ControlRetired
{
    admission:   RetiredCapacity<Object, InvocationGate, automatic::Work>,
    publication: RetiredPublication<InvocationGate>,
    recipient:   OwnerRef,
    storage:     OwnerRef,
    allocation:  AllocationAuthority
}
/// Capture retirement before consuming the drawing/status result, on both
/// successful and refused postbirth request completion.
#[must_use = "inspect control result and dispose retired metadata outside Desktop"]
pub(crate) struct ControlCommit
{
    pub(crate) result:  Result<Option<super::super::Drawing>, Status>,
    pub(crate) retired: ControlRetired
}

impl ControlPlan
{
    pub(crate) fn attach_cleanup(&mut self, cleanup: super::disposal::ControlCleanup)
                                 -> Result<(), (Status, super::disposal::ControlCleanup)>
    {
        if self.cleanup.is_some() || !cleanup.matches(&self.storage, self.entry)
        {
            return Err((ACCESS_DENIED, cleanup));
        }
        self.cleanup = Some(cleanup);
        Ok(())
    }

    pub(crate) fn prepare(self) -> Result<ControlPreparation, CreationError>
    {
        validate_authority(&self.recipient, &self.storage, &self.allocation)?;
        crate::allocator::with_target(crate::allocator::Target::Owner {
            account: self.allocation.clone(), class: mac_kernel_model::policy::MemoryClass::Kernel
        }, || {
            let validated = self.validated.draw();
            let title = if !self.scrollbar
                           && !matches!(validated.procedure,
                                        PROGRESS_BAR_PROCEDURE | MONOCHROME_PROGRESS_BAR_PROCEDURE)
            {
                Some(super::super::snapshot::Title::admit(&validated.standard, None)
                     .map_err(CreationError::Title)?)
            }
            else
            {
                None
            };
            let definition = crate::toolbox::resources::definitions::bind(self.entry)
                .map_err(CreationError::Binding)?;
            let capacity = ControlCapacity {
                publication: self.publication.prepare().map_err(CreationError::Facility)?
            };
            let (object,cleanup) = Object::new(self.intent.window, self.recipient.owner().incarnation(),
                                     self.validated, title, definition, self.intent.reflow, 0, false, self.cleanup_notice.clone(),&self.storage,self.cleanup)
                .map_err(CreationError::Validation)?;
            Ok(ControlPreparation { object, cleanup, recipient: self.recipient, storage: self.storage,
                                    allocation: self.allocation.clone(), intent: self.intent,
                                    gate: self.gate, capacity })
        })
    }
}
impl ControlPreparation
{
    pub(crate) fn recipient(&self) -> &OwnerRef
    {
        &self.recipient
    }

    pub(crate) fn storage(&self) -> &OwnerRef
    {
        &self.storage
    }

    pub(crate) fn matches_request(&self, call: &ControlCall) -> bool
    {
        self.intent.matches(call)
    }

    /// Replace only unpublished metadata after capacity contention. Keep the
    /// same real validated Object, immutable title and original CDEF binding.
    pub(crate) fn plan_capacity(&self, manager: &Manager)
                                -> Result<ControlCapacityPlan, CreationError>
    {
        manager.validate_process_intent(&self.recipient, &self.storage, &self.allocation,
                                        &self.intent)?;
        Ok(ControlCapacityPlan { publication: manager.objects
                                                     .plan_publication(&self.storage)
                                                     .map_err(CreationError::Facility)?,
                                 allocation:  self.allocation.clone() })
    }

    pub(crate) fn replace_capacity(&mut self, mut capacity: ControlCapacity) -> ControlCapacity
    {
        core::mem::swap(&mut self.capacity, &mut capacity);
        capacity
    }

    pub(crate) fn dispose(self)
    {
        drop(self);
    }
}
impl PreparedControl
{
    pub(crate) fn recipient(&self) -> &OwnerRef
    {
        &self.recipient
    }

    pub(crate) fn storage(&self) -> &OwnerRef
    {
        &self.storage
    }

    pub(crate) fn matches_request(&self, call: &ControlCall) -> bool
    {
        self.intent.matches(call)
    }

    pub(crate) fn dispose(self)
    {
        drop(self);
    }
}
impl ControlCapacityPlan
{
    pub(crate) fn prepare(self) -> Result<ControlCapacity, CreationError>
    {
        crate::allocator::with_target(crate::allocator::Target::Owner {
            account: self.allocation.clone(), class: mac_kernel_model::policy::MemoryClass::Kernel
        }, || {
            Ok(ControlCapacity {
                publication: self.publication.prepare().map_err(CreationError::Facility)?
            })
        })
    }
}
impl ControlCapacity
{
    pub(crate) fn dispose(self)
    {
        drop(self);
    }
}
impl ControlRetired
{
    pub(crate) fn dispose(self)
    {
        self.admission.dispose();
        self.publication.dispose();
        drop(self.recipient);
        drop(self.storage);
        drop(self.allocation);
    }
}

fn validate_authority(recipient: &OwnerRef, storage: &OwnerRef, allocation: &AllocationAuthority)
                      -> Result<(), CreationError>
{
    if recipient.owner().kind() != DomainKind::Process
       || recipient.owner() != storage.owner()
       || crate::context::recovery_activity().is_some()
    {
        return Err(CreationError::Authority);
    }
    if allocation.owner() != storage.owner()
    {
        return Err(CreationError::Facility(PoolError::ForeignOwner { expected: storage.owner(),
                                                                     actual:   allocation.owner() }));
    }
    if !recipient.accepting()
    {
        return Err(CreationError::Facility(PoolError::Stopping { owner: recipient.owner() }));
    }
    Ok(())
}
impl Manager
{
    fn validate_process_intent(&self, recipient: &OwnerRef, storage: &OwnerRef,
                               allocation: &AllocationAuthority, intent: &Intent)
                               -> Result<(), CreationError>
    {
        validate_authority(recipient, storage, allocation)?;
        if !matches!(intent.operation, CREATE_STANDARD | CREATE_SCROLLBAR)
        {
            return Err(CreationError::InvalidRequest);
        }
        self.preflight_definition(recipient, storage, &intent.call())
            .map_err(CreationError::Validation)?;
        Ok(())
    }

    /// Run in the original Desktop turn. Only this initial planning call invokes
    /// the existing CDEF validation, before detaching the resulting snapshot.
    pub(crate) fn plan_process_create(&self, recipient: &OwnerRef, storage: &OwnerRef,
                                      allocation: &AllocationAuthority, call: &ControlCall,
                                      entry: DefinitionRef, gate: TrapGate)
                                      -> Result<ControlPlan, CreationError>
    {
        if !call.surface.is_null() || call.reserved != 0
        {
            return Err(CreationError::InvalidRequest);
        }
        let intent = Intent::new(call);
        self.validate_process_intent(recipient, storage, allocation, &intent)?;
        let (validated, scrollbar) =
            self.validate_spec(recipient.owner().incarnation(), call, entry, gate)
                .map_err(CreationError::Validation)?;
        self.plan_validated_process(recipient, storage, allocation, intent, validated, scrollbar,
                                    entry, gate)
    }

    pub(crate) fn plan_process_user_create(&self, recipient: &OwnerRef, storage: &OwnerRef,
                                           allocation: &AllocationAuthority, call: &ControlCall,
                                           validated: &ControlCall, entry: DefinitionRef,
                                           gate: TrapGate)
                                           -> Result<ControlPlan, CreationError>
    {
        let intent = Intent::new(call);
        self.validate_process_intent(recipient, storage, allocation, &intent)?;
        if !call.surface.is_null() || call.reserved != 0
           || !crate::toolbox::resources::definitions::is_user(entry).map_err(CreationError::Binding)?
        { return Err(CreationError::InvalidRequest); }
        let id = super::super::preflight(call).map_err(CreationError::Validation)?;
        if super::super::preflight(validated).map_err(CreationError::Validation)? != id
        {
            return Err(CreationError::InvalidRequest);
        }
        let scrollbar = call.operation == CREATE_SCROLLBAR;
        let validated = Spec::from_call(validated, scrollbar);
        self.plan_validated_process(recipient, storage, allocation, intent, validated, scrollbar,
                                    entry, gate)
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_validated_process(&self, recipient: &OwnerRef, storage: &OwnerRef,
                              allocation: &AllocationAuthority, intent: Intent, validated: Spec,
                              scrollbar: bool, entry: DefinitionRef, gate: TrapGate)
                              -> Result<ControlPlan, CreationError>
    {
        let publication =
            self.objects.plan_publication(storage).map_err(CreationError::Facility)?;
        Ok(ControlPlan { cleanup_notice: self.disposal_notice.clone(),
                         cleanup: None,
                         recipient: recipient.clone(),
                         storage: storage.clone(),
                         allocation: allocation.clone(),
                         intent,
                         validated,
                         scrollbar,
                         entry,
                         gate,
                         publication })
    }

    /// Every refusal restores the whole exact preparation, including any
    /// displaced buffers returned by the canonical Store.
    pub(crate) fn admit_process_create(&mut self, input: &mut Option<ControlPreparation>)
                                       -> Result<PreparedControl, CreationError>
    {
        let value = input.as_ref().ok_or(CreationError::InvalidRequest)?;
        self.validate_process_intent(&value.recipient, &value.storage, &value.allocation,
                                     &value.intent)?;
        let Some(ControlPreparation { object,
                                      cleanup,
                                      recipient,
                                      storage,
                                      allocation,
                                      intent,
                                      gate,
                                      capacity: ControlCapacity { publication } }) = input.take()
        else
        {
            return Err(CreationError::InvalidRequest);
        };
        match self.objects.admit_prepared(publication)
        {
            Ok((permit, admission)) =>
            {
                Ok(PreparedControl { object,
                                     cleanup,
                                     recipient,
                                     storage,
                                     allocation,
                                     intent,
                                     gate,
                                     admission,
                                     permit })
            }
            Err(failure) =>
            {
                *input = Some(ControlPreparation { object,
                                                   cleanup,
                                                   recipient,
                                                   storage,
                                                   allocation,
                                                   intent,
                                                   gate,
                                                   capacity:
                                                       ControlCapacity { publication:
                                                                             failure.preparation } });
                Err(CreationError::Preparation(failure.cause))
            }
        }
    }

    /// Caller rechecks actual current owner and live/owned Window in this same
    /// Desktop turn and supplies its freshly resolved original CDEF reference.
    /// Prebirth refusal retains payload and permit; finished always owns the
    /// metadata for outside disposal, even if the shared request tail refuses.
    pub(crate) fn commit_process_create(&mut self, input: &mut Option<PreparedControl>,
                                        call: &mut ControlCall, current_entry: DefinitionRef)
                                        -> Result<ControlCommit, CreationError>
    {
        let value = input.as_ref().ok_or(CreationError::InvalidRequest)?;
        self.validate_process_intent(&value.recipient, &value.storage, &value.allocation,
                                     &value.intent)?;
        if !value.intent.matches(call)
        {
            return Err(CreationError::InvalidRequest);
        }
        if !value.object.definition.matches(current_entry)
        {
            return Err(CreationError::DefinitionChanged);
        }
        let next_order =
            self.next_order.checked_add(1).ok_or(CreationError::Validation(NO_MEMORY))?;
        let Some(PreparedControl { mut object,
                                   cleanup,
                                   recipient,
                                   storage,
                                   allocation,
                                   intent,
                                   gate,
                                   admission,
                                   permit }) = input.take()
        else
        {
            return Err(CreationError::InvalidRequest);
        };
        let entry = object.definition.reference();
        // Presentation order is selected atomically with real canonical birth.
        // It is not an object identity and is never reserved by a stale plan.
        object.tree = tree::Node::new(self.next_order);
        match self.objects.publish_prepared(permit, object)
        {
            Ok((publication, retired)) =>
            {
                self.publish_disposal_slot(cleanup, publication.into_parts().1, call.window);
                call.id = publication.id();
                self.next_order = next_order;
                self.schedule_layout(call.id);
                let result = self.finish_request(call,
                                                 recipient.owner().incarnation(),
                                                 entry,
                                                 gate,
                                                 None,
                                                 None);
                Ok(ControlCommit { result,
                                   retired: ControlRetired { admission,
                                                             publication: retired,
                                                             recipient,
                                                             storage,
                                                             allocation } })
            }
            Err(failure) =>
            {
                *input = Some(PreparedControl { object: failure.value,
                                                cleanup,
                                                recipient,
                                                storage,
                                                allocation,
                                                intent,
                                                gate,
                                                admission,
                                                permit: failure.permit });
                Err(CreationError::Preparation(failure.cause))
            }
        }
    }
}
