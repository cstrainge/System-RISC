//! One owned replacement image across allocation refusal and publication.
//! Neither Retry nor report presentation can create another alias or Mapping.
use core::{fmt, sync::atomic::Ordering};

use mac_abi::Status;

use super::{Binding, Loaded, PreparedLoad};
use crate::{allocator::recovery::Permit,
            machine::paging::modules::{Error as MappingError, PublicationState},
            toolbox::resources::loader::{self, LoadError}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryDisposition
{
    Retryable,
    RestartRequired,
    Cancelled
}

#[derive(Debug)]
pub(crate) struct RecoveryImageFailure
{
    pub(crate) cause:       LoadError,
    pub(crate) disposition: RecoveryDisposition
}
impl RecoveryImageFailure
{
    /// Numeric presentation at the existing service boundary never determines
    /// retry. Keep the original typed cause available for the System report.
    pub(crate) fn status(&self) -> Status
    {
        match &self.cause
        {
            LoadError::Format(cause) | LoadError::Mapping(MappingError::Format(cause)) =>
            {
                loader::status(*cause)
            }
            LoadError::Mapping(cause) => cause.status(),
            LoadError::Definition(cause) => cause.status()
        }
    }
}
impl fmt::Display for RecoveryImageFailure
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        write!(out, "{}; {}", self.cause, match self.disposition
        {
            RecoveryDisposition::Retryable => "the same pending image can be retried",
            RecoveryDisposition::RestartRequired =>
                "this image cannot safely resume; restart System",
            RecoveryDisposition::Cancelled =>
                "the image owner is stopping; this admission cannot resume",
        })
    }
}

#[must_use = "retain the same pending image on refusal or release it to its original owner walk"]
pub(crate) struct RecoveryImageFinishFailure
{
    pub(crate) pending: PendingRecoveryImage,
    pub(crate) failure: RecoveryImageFailure
}

/// The new alias, Loading exclusion and Read survive every refused advance.
/// Field order drops Loading/Read before releasing the protected source permit.
/// The canonical Instance, not this wrapper, anchors any prepared real Mapping.
#[must_use = "retain the pending owner through report/handoff and every retry"]
pub(crate) struct PendingRecoveryImage
{
    prepared: PreparedLoad,
    permit:   Permit
}
impl PendingRecoveryImage
{
    // Only PreparedLoad's checked conversion calls this; no Binding-based factory.
    pub(super) fn new(prepared: PreparedLoad, permit: Permit) -> Self
    {
        Self { prepared,
               permit }
    }

    pub(crate) fn binding(&self) -> Binding
    {
        self.prepared.read.binding
    }

    /// Outside Desktop/IMAGES. The original Instance supplies the real account;
    /// the retained Permit supplies protected origin, never ambient caller state.
    /// Success means the same Mapping is Live; finish alone releases Loading.
    pub(crate) fn advance(&mut self) -> Result<(), RecoveryImageFailure>
    {
        let target =
            crate::allocator::Target::Owner { account: self.prepared.read.allocation().clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::context::with_recovery_allocation(self.permit.clone(), || {
            crate::allocator::with_target(target, || self.advance_inner())
        })
    }

    fn advance_inner(&mut self) -> Result<(), RecoveryImageFailure>
    {
        self.prepared.ensure_active().map_err(|cause| self.failure(cause.into()))?;
        if self.prepared.read.instance.mapping.get().is_none()
        {
            let mapping =
                self.prepared
                    .prepare_application_mapping(self.prepared.read.allocation(),
                                                 Some(&self.permit))
                    .map_err(|cause| self.failure(cause))?;
            self.prepared.anchor_mapping(&mapping).map_err(|cause| self.failure(cause.into()))?;
        }
        let Some(mapping) = self.prepared.read.instance.mapping.get()
        else
        {
            return Err(self.failure(MappingError::State.into()));
        };
        match mapping.publication_state()
        {
            PublicationState::Live => return Ok(()),
            PublicationState::Prepared | PublicationState::Mapped =>
            {}
            PublicationState::Blocked => return Err(self.failure(MappingError::State.into()))
        }
        // The unique Loading owner excludes another image publisher. A returned
        // Mapped/Busy refusal has passed the translation ACK and retries only
        // the SAME Mapping's instruction fence. Preparing is never reset.
        self.prepared.publish_anchored().map_err(|cause| self.failure(cause.into()))
    }

    fn failure(&self, cause: LoadError) -> RecoveryImageFailure
    {
        let instance = &self.prepared.read.instance;
        let disposition = if !instance.owner.accepting()
                             || instance.activity.retiring.load(Ordering::Acquire)
                             || matches!(&cause, LoadError::Mapping(MappingError::Closed))
        {
            if let Some(mapping) = instance.mapping.get()
            {
                mapping.request_retirement();
            }
            RecoveryDisposition::Cancelled
        }
        else if let Some(mapping) = instance.mapping.get()
        {
            if mapping.publication_state() == PublicationState::Mapped
               && matches!(&cause, LoadError::Mapping(MappingError::Monitor(crate::machine::monitor::Error::Busy)))
            {
                RecoveryDisposition::Retryable
            }
            else { RecoveryDisposition::RestartRequired }
        }
        else if allocation_refusal(&cause)
        {
            RecoveryDisposition::Retryable
        }
        else
        {
            RecoveryDisposition::RestartRequired
        };
        RecoveryImageFailure { cause,
                               disposition }
    }

    /// Consumes Loading exactly once, only after the actual Mapping is Live.
    /// Even a late stop/refusal returns the original pending owner, not a bare
    /// status which would silently release the alias and report's retry state.
    pub(crate) fn finish(self) -> Result<Loaded, RecoveryImageFinishFailure>
    {
        let Self { prepared,
                   permit } = self;
        match prepared.finish_mapping()
        {
            Ok(loaded) => Ok(loaded),
            Err((cause, prepared)) =>
            {
                let pending = Self { prepared,
                                     permit };
                let failure = pending.failure(cause.into());
                Err(RecoveryImageFinishFailure { pending,
                                                 failure })
            }
        }
    }
}

fn allocation_refusal(cause: &LoadError) -> bool
{
    use mac_formats::Error as FormatError;
    use mac_kernel_model::account::Error as AccountError;

    use crate::allocator::recovery::Error as RecoveryError;
    match cause
    {
        LoadError::Format(FormatError::AllocationFailed(_) | FormatError::BudgetExceeded(_))
        | LoadError::Mapping(MappingError::Format(FormatError::AllocationFailed(_)
                                                | FormatError::BudgetExceeded(_)))
        | LoadError::Mapping(MappingError::Metadata)
        | LoadError::Mapping(MappingError::Recovery(RecoveryError::Exhausted { .. }
                                                  | RecoveryError::BackingRefused { .. }))
        | LoadError::Mapping(MappingError::Account(AccountError::Budget(_)
                                                 | AccountError::Metadata { .. })) => true,
        // Operation/foreign/account lifecycle, malformed format and mapping
        // protocol failures are not inferred transient from numeric Status.
        _ => false
    }
}
