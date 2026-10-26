//! An invocation borrows its origin queue and returns owned work on every exit.
use core::fmt;

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReturnCause
{
    Interrupted,
    MissingOrForeign,
    RejectedCompletion,
    MissingPayload,
    CleanupRefused(mac_object_table::domain::PoolError),
    CleanupStoreMissing
}
impl fmt::Display for ReturnCause
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.write_str(match self
                    {
                        Self::Interrupted =>
                        {
                            "driver callback did not return; payload and ticket remain \
                             quarantined; arbitrary privileged-fault containment is unproven \
                             (M-D04), restart System"
                        }
                        Self::MissingOrForeign =>
                        {
                            "dispatch no longer matches its originating job; owned payload and \
                             ticket remain quarantined"
                        }
                        Self::RejectedCompletion =>
                        {
                            "job rejected its dispatch completion; owned payload and ticket remain \
                             quarantined"
                        }
                        Self::MissingPayload =>
                        {
                            "dispatch has no admitted payload; job remains quarantined rather than \
                             acknowledging completion"
                        }
                        Self::CleanupStoreMissing =>
                        {
                            "driver job cleanup storage is unavailable; exact cleanup ticket \
                             remains retained for retry"
                        }
                        Self::CleanupRefused(cause) =>
                        {
                            return write!(f,
                                          "driver job cleanup refused: {cause}; exact cleanup \
                                           ticket and payload remain retained for retry");
                        }
                    })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReturnFailure
{
    pub job:   u32,
    pub owner: Owner,
    pub cause: ReturnCause
}

/// The queue cannot be destroyed or moved while an invocation borrows it.
/// No queue lock or app buffer crosses the provider callback.
///
/// ```compile_fail
/// use mac_system::drivers::jobs::Queue;
/// fn destroy_during_dispatch(queue: Queue) {
///     let invocation = queue.next_dispatch();
///     drop(queue);
///     drop(invocation);
/// }
/// ```
#[must_use = "dropping a dispatch safely returns it to its originating queue"]
pub struct Dispatch<'q>
{
    pub(super) queue:       &'q Queue,
    pub(super) node:        Option<Box<Node>>,
    pub(super) in_callback: bool
}
impl fmt::Debug for Dispatch<'_>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.debug_struct("Dispatch")
         .field("job", &self.node.as_ref().map(|n| n.id))
         .field("in_callback", &self.in_callback)
         .finish_non_exhaustive()
    }
}
impl Dispatch<'_>
{
    pub fn invoke(&mut self, broker: &mut impl Broker) -> Result<(), Status>
    {
        let node = self.node.as_mut().ok_or(NOT_FOUND)?;
        if node.outcome.is_some() || self.in_callback
        {
            return Err(BAD_ABI);
        }
        if node.failure.is_some()
        {
            return Err(mac_abi::ROLLBACK_FAILED);
        }
        let Some(work) = node.work.as_mut()
        else
        {
            node.failure = Some(ReturnCause::MissingPayload);
            return Err(BAD_ABI);
        };
        let phase = if work.ticket == 0
        {
            Phase::Start
        }
        else if node.cancel || work.forced_error.is_some()
        {
            Phase::Cancel
        }
        else
        {
            Phase::Poll
        };
        let input = if phase == Phase::Start { work.input.as_slice() } else { &[] };
        self.in_callback = true;
        let outcome = broker.invoke(Call { owner: node.owner,
                                           provider: work.provider,
                                           class: work.class,
                                           operation: work.operation,
                                           ticket: &mut work.ticket,
                                           phase,
                                           input,
                                           output: &mut work.output });
        self.in_callback = false;
        node.outcome = Some(outcome);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn ticket(&self) -> Option<u32>
    {
        self.node.as_ref()?.work.as_ref().map(|work| work.ticket)
    }
}
impl Drop for Dispatch<'_>
{
    fn drop(&mut self)
    {
        if let Some(node) = self.node.take()
        {
            self.queue.return_owned(node, self.in_callback);
        }
    }
}
