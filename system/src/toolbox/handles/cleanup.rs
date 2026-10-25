//! Resident cleanup refusals retain both typed cause and the unique retry ticket.
#![forbid(unsafe_code)]
use mac_object_table::{domain::{CompletionError, PoolError, ReleasedResource},
                       registry::{CleanupFailure, Release}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cause
{
    Facility(PoolError),
    Alias(mac_object_table::aliases::ReleaseCause),
    FileRetirementStorage
    {
        owner:    mac_object_table::domain::DomainId,
        kind:     mac_object_table::domain::KindCode,
        pending:  usize,
        capacity: usize
    },
    DragEffectStorage
    {
        pending:   usize,
        requested: usize
    },
    #[cfg(target_arch = "riscv64")]
    MemoryManagerUnavailable(crate::state::AccessError),
    #[cfg(target_arch = "riscv64")]
    FileManagerUnavailable(crate::state::AccessError),
    WindowDefinitionDisposal
    {
        window:    u64,
        procedure: i16,
        reply:     i64
    }
}
impl core::fmt::Display for Cause
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Facility(error) => error.fmt(f),
            Self::Alias(error) => error.fmt(f),
            Self::FileRetirementStorage { owner,
                                          kind,
                                          pending,
                                          capacity } =>
            {
                write!(f,
                       "File Manager {kind} cleanup for {owner} cannot retain another \
                        asynchronous object: {pending} pending, reserved capacity {capacity}; \
                        payload, provider token and cleanup ticket remain owned for retry")
            }
            Self::DragEffectStorage { pending,
                                      requested } =>
            {
                write!(f,
                       "Drag Manager could not reserve {requested} notification slots with \
                        {pending} pending; protocol state and cleanup ticket remain retained for \
                        retry")
            }
            #[cfg(target_arch = "riscv64")]
            Self::MemoryManagerUnavailable(error) =>
            {
                write!(f,
                       "Memory Manager cleanup could not enter its resident state ({error:?}); \
                        payload, backing and cleanup ticket remain retained for retry")
            }
            #[cfg(target_arch = "riscv64")]
            Self::FileManagerUnavailable(error) =>
            {
                write!(f,
                       "File Manager cleanup could not enter its resident state ({error:?}); file \
                        payload, backing and cleanup ticket remain retained for retry")
            }
            Self::WindowDefinitionDisposal { window,
                                             procedure,
                                             reply } =>
            {
                write!(f,
                       "WDEF {} variation {} refused disposal of window {window} with reply \
                        {reply}; definition state and cleanup ticket remain retained for retry",
                       procedure >> 4,
                       procedure & 15)
            }
        }
    }
}
impl core::error::Error for Cause
{
}
#[derive(Debug)]
pub(crate) struct Failure
{
    ticket: ReleasedResource,
    cause:  Cause
}
impl Failure
{
    pub(crate) fn file_retirement_storage(ticket: ReleasedResource, pending: usize,
                                          capacity: usize)
                                          -> Self
    {
        let key = ticket.key();
        Self { ticket,
               cause: Cause::FileRetirementStorage { owner: key.owner(),
                                                     kind: key.kind(),
                                                     pending,
                                                     capacity } }
    }

    pub(crate) fn drag_effect_storage(ticket: ReleasedResource, pending: usize) -> Self
    {
        Self { ticket,
               cause: Cause::DragEffectStorage { pending,
                                                 requested: 4 } }
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn files_unavailable(ticket: ReleasedResource, error: crate::state::AccessError)
                                    -> Self
    {
        Self { ticket,
               cause: Cause::FileManagerUnavailable(error) }
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn memory_unavailable(ticket: ReleasedResource, error: crate::state::AccessError)
                                     -> Self
    {
        Self { ticket,
               cause: Cause::MemoryManagerUnavailable(error) }
    }

    pub(crate) fn window_definition(ticket: ReleasedResource, window: u64, procedure: i16,
                                    reply: i64)
                                    -> Self
    {
        Self { ticket,
               cause: Cause::WindowDefinitionDisposal { window,
                                                        procedure,
                                                        reply } }
    }
}
impl From<CompletionError> for Failure
{
    fn from(error: CompletionError) -> Self
    {
        Self { cause:  Cause::Facility(error.cause()),
               ticket: error.into_ticket() }
    }
}
impl From<mac_object_table::aliases::ReleaseError> for Failure
{
    fn from(error: mac_object_table::aliases::ReleaseError) -> Self
    {
        let (ticket, cause) = error.into_parts();
        Self { ticket,
               cause: Cause::Alias(cause) }
    }
}
impl CleanupFailure for Failure
{
    type Cause = Cause;

    fn into_parts(self) -> (ReleasedResource, Cause)
    {
        (self.ticket, self.cause)
    }

    fn registration_mismatch(ticket: ReleasedResource) -> Self
    {
        Self::from(<CompletionError as CleanupFailure>::registration_mismatch(ticket))
    }
}
impl core::fmt::Display for Failure
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        self.cause.fmt(f)
    }
}
impl core::error::Error for Failure
{
}

/// Adapt facility-only resident providers without discarding their typed cause.
pub(crate) struct Complete<R>(pub R);
impl<C, R: Release<C>> Release<C, Failure> for Complete<R>
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        self.0.release(context, ticket).map_err(Failure::from)
    }
}

#[cfg(any(target_arch = "riscv64", test))]
mod checkpoint;
#[cfg(target_arch = "riscv64")]
pub(crate) use checkpoint::Checkpoint;
