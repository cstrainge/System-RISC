//! Ordinary receiver publication into the same native Store and classic index.
//! Prepared receiver authority is independent of its final real Window target;
//! desktop zero is a distinct target, never a provisional Window identity.
#![forbid(unsafe_code)]

use core::fmt;

use mac_object_table::{domain::DomainKind,
                       projected::{self, PreparedError}};

use super::*;
use crate::{context::ThreadId, machine::scheduler::accounting::AllocationAuthority,
            toolbox::windows::WindowId};

#[derive(Clone, Copy)]
pub(crate) enum ReceiverTarget
{
    Window(WindowId),
    Toolbox
}
impl ReceiverTarget
{
    fn window(self) -> Result<u64, ReceiverError>
    {
        match self
        {
            Self::Window(id) if id.0 != 0 => Ok(id.0),
            Self::Toolbox => Ok(0),
            Self::Window(_) => Err(ReceiverError::InvalidTarget)
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum ReceiverError
{
    InvalidTarget,
    Toolbox(crate::state::AccessError),
    Binding(Status),
    Publication(PreparedError)
}
impl ReceiverError
{
    pub(crate) fn status(self) -> Status
    {
        match self
        {
            Self::InvalidTarget => BAD_ARGUMENT,
            Self::Toolbox(cause) => cause.status(),
            Self::Binding(status) => status,
            Self::Publication(cause) => super::prepared_receiver_status(cause)
        }
    }
}
impl fmt::Display for ReceiverError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::InvalidTarget =>
            {
                out.write_str("drag receiver requires a real nonzero WindowId or the explicit \
                               Finder desktop target")
            }
            Self::Toolbox(cause) => write!(out, "drag receiver Desktop access refused: {cause:?}"),
            Self::Binding(status) =>
            {
                write!(out,
                       "drag receiver original owner/thread/target binding refused with status \
                        {status}")
            }
            Self::Publication(cause) => cause.fmt(out)
        }
    }
}
pub(crate) struct ReceiverPlan
{
    publication: projected::PublicationPlan<Receiver, InvocationGate>,
    owner:       OwnerRef,
    thread:      ThreadId,
    allocation:  AllocationAuthority
}
#[must_use]
pub(crate) struct ReceiverPreparation
{
    publication: projected::PreparedPublication<Receiver, InvocationGate>,
    owner:       OwnerRef,
    thread:      ThreadId,
    allocation:  AllocationAuthority
}
#[must_use]
pub(crate) struct PreparedReceiver
{
    payload:    Option<Receiver>,
    admission:  projected::RetiredCapacity<Receiver, InvocationGate>,
    permit:     projected::PublicationPermit<InvocationGate>,
    owner:      OwnerRef,
    thread:     ThreadId,
    allocation: AllocationAuthority
}
#[must_use]
pub(crate) struct ReceiverRetired
{
    admission:   projected::RetiredCapacity<Receiver, InvocationGate>,
    publication: projected::RetiredPublication<InvocationGate>,
    owner:       OwnerRef,
    allocation:  AllocationAuthority
}
#[must_use]
pub(crate) struct ReceiverCapacity(projected::PreparedCapacity<InvocationGate>);
impl ReceiverRetired
{
    pub(crate) fn dispose(self)
    {
        self.admission.dispose();
        self.publication.dispose();
        drop(self.owner);
        drop(self.allocation);
    }
}
fn authority(owner: &OwnerRef, thread: ThreadId, allocation: &AllocationAuthority)
             -> Result<(), ReceiverError>
{
    let caller = crate::context::current_thread_context().identity();
    if owner.owner().kind() != DomainKind::Process
       || caller.app.0 != owner.owner().incarnation()
       || caller.thread != thread
       || allocation.owner() != owner.owner()
       || crate::context::recovery_activity().is_some()
    {
        return Err(ReceiverError::Binding(ACCESS_DENIED));
    }
    if !owner.accepting()
    {
        return Err(ReceiverError::Binding(SERVICE_BUSY));
    }
    Ok(())
}
impl ReceiverPlan
{
    pub(crate) fn prepare(self) -> Result<ReceiverPreparation, ReceiverError>
    {
        authority(&self.owner, self.thread, &self.allocation)?;
        crate::allocator::with_target(crate::allocator::Target::Owner {
            account: self.allocation.clone(), class: mac_kernel_model::policy::MemoryClass::Kernel
        }, || {
            let publication = self.publication.prepare().map_err(ReceiverError::Publication)?;
            Ok(ReceiverPreparation { publication, owner: self.owner, thread: self.thread, allocation: self.allocation.clone() })
        })
    }
}
impl ReceiverPreparation
{
    pub(crate) fn prepare_capacity(&self) -> Result<ReceiverCapacity, ReceiverError>
    {
        authority(&self.owner, self.thread, &self.allocation)?;
        self.publication
            .prepare_capacity()
            .map(ReceiverCapacity)
            .map_err(ReceiverError::Publication)
    }

    pub(crate) fn replace_capacity(&mut self, capacity: &mut ReceiverCapacity)
                                   -> Result<(), ReceiverError>
    {
        self.publication.replace_capacity(&mut capacity.0).map_err(ReceiverError::Publication)
    }
}
impl PreparedReceiver
{
    pub(crate) fn prepare_capacity(&self) -> Result<ReceiverCapacity, ReceiverError>
    {
        authority(&self.owner, self.thread, &self.allocation)?;
        self.permit.prepare_capacity().map(ReceiverCapacity).map_err(ReceiverError::Publication)
    }
}
impl Manager
{
    pub(crate) fn receiver_binding(&self, owner: &OwnerRef, thread: ThreadId,
                                   target: ReceiverTarget)
                                   -> Result<bool, ReceiverError>
    {
        let Some(id) = self.receiver_at(target.window()?)
        else
        {
            return Ok(false);
        };
        let receiver = self.receivers.get(id).ok_or(ReceiverError::Binding(NOT_FOUND))?;
        if receiver.owner != owner.owner().incarnation() || receiver.thread != thread
        {
            return Err(ReceiverError::Binding(ACCESS_DENIED));
        }
        Ok(true)
    }

    pub(crate) fn plan_receiver(&self, owner: &OwnerRef, thread: ThreadId,
                                allocation: &AllocationAuthority)
                                -> Result<ReceiverPlan, ReceiverError>
    {
        authority(owner, thread, allocation)?;
        let publication =
            self.receivers.plan_publication(owner).map_err(ReceiverError::Publication)?;
        Ok(ReceiverPlan { publication,
                          owner: owner.clone(),
                          thread,
                          allocation: allocation.clone() })
    }

    pub(crate) fn replan_receiver(&self, preparation: &ReceiverPreparation)
                                  -> Result<ReceiverPlan, ReceiverError>
    {
        self.plan_receiver(&preparation.owner, preparation.thread, &preparation.allocation)
    }

    pub(crate) fn admit_receiver(&mut self, input: &mut Option<ReceiverPreparation>)
                                 -> Result<PreparedReceiver, ReceiverError>
    {
        let value = input.as_ref().ok_or(ReceiverError::Binding(BAD_ABI))?;
        authority(&value.owner, value.thread, &value.allocation)?;
        let Some(ReceiverPreparation { publication,
                                       owner,
                                       thread,
                                       allocation }) = input.take()
        else
        {
            return Err(ReceiverError::Binding(BAD_ABI));
        };
        match self.receivers.admit_prepared(publication)
        {
            Ok((permit, admission)) =>
            {
                Ok(PreparedReceiver { payload: None,
                                      permit,
                                      admission,
                                      owner,
                                      thread,
                                      allocation })
            }
            Err(failure) =>
            {
                *input = Some(ReceiverPreparation { publication: failure.preparation,
                                                    owner,
                                                    thread,
                                                    allocation });
                Err(ReceiverError::Publication(failure.cause))
            }
        }
    }

    pub(crate) fn admit_receiver_projection(&mut self, receiver: &mut PreparedReceiver)
                                            -> Result<(), ReceiverError>
    {
        authority(&receiver.owner, receiver.thread, &receiver.allocation)?;
        self.receivers
            .admit_projection(&mut receiver.permit)
            .map(|_| ())
            .map_err(ReceiverError::Publication)
    }

    pub(crate) fn admit_receiver_capacity(&mut self, receiver: &PreparedReceiver,
                                          capacity: &mut ReceiverCapacity)
                                          -> Result<(), ReceiverError>
    {
        authority(&receiver.owner, receiver.thread, &receiver.allocation)?;
        self.receivers
            .admit_capacity(&receiver.permit, &mut capacity.0)
            .map_err(ReceiverError::Publication)
    }

    /// The caller proves the actual live/owned window in this same Desktop turn.
    /// Existing equal binding returns without consuming the unused affine owner.
    pub(crate) fn commit_receiver(&mut self, input: &mut Option<PreparedReceiver>,
                                  target: ReceiverTarget)
                                  -> Result<Option<ReceiverRetired>, ReceiverError>
    {
        let value = input.as_ref().ok_or(ReceiverError::Binding(BAD_ABI))?;
        authority(&value.owner, value.thread, &value.allocation)?;
        if self.receiver_binding(&value.owner, value.thread, target)?
        {
            return Ok(None);
        }
        let window = target.window()?;
        if matches!(target, ReceiverTarget::Toolbox)
           && value.owner.owner().incarnation() != crate::context::finder_app().0
        {
            return Err(ReceiverError::Binding(ACCESS_DENIED));
        }
        if let Some(payload) = &value.payload
           && (payload.window != window
               || payload.owner != value.owner.owner().incarnation()
               || payload.thread != value.thread)
        {
            return Err(ReceiverError::Binding(ACCESS_DENIED));
        }
        let Some(PreparedReceiver { mut payload,
                                    admission,
                                    permit,
                                    owner,
                                    thread,
                                    allocation }) = input.take()
        else
        {
            return Err(ReceiverError::Binding(BAD_ABI));
        };
        let receiver = payload.take().unwrap_or(Receiver { owner: owner.owner().incarnation(),
                                                           thread,
                                                           window,
                                                           closing: false });
        match self.receivers.publish_prepared(permit, receiver)
        {
            Ok((_, publication)) =>
            {
                Ok(Some(ReceiverRetired { admission,
                                          publication,
                                          owner,
                                          allocation }))
            }
            Err(failure) =>
            {
                *input = Some(PreparedReceiver { payload: Some(failure.value),
                                                 admission,
                                                 permit: failure.permit,
                                                 owner,
                                                 thread,
                                                 allocation });
                Err(ReceiverError::Publication(failure.cause))
            }
        }
    }
}
