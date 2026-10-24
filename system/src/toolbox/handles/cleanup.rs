//! Resident cleanup refusals retain both typed cause and the unique retry ticket.
#![forbid(unsafe_code)]
use mac_object_table::{domain::{CompletionError, PoolError, ReleasedResource},
                       registry::{CleanupFailure, Release}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cause
{
    Facility(PoolError),
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
