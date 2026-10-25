//! Invalidate-first owner cleanup. No manager list, global phase ordering,
//! allocation, callback under a ledger lock, or privileged unwind promise.
use core::fmt;

use crate::{domain::{CompletionError, PoolError, ResourceKey},
            registry::{CleanupFailure, Invocation},
            shared::{InvocationGate, OwnerRef}};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report
{
    pub invalidated: usize,
    pub released:    usize,
    /// Accepted callbacks whose resource was still pending on return. Later
    /// child cleanup may complete them; the ledger is the final death barrier.
    pub deferred:    usize,
    pub quarantined: usize
}
#[derive(Debug)]
pub enum Failure<E, Cause = PoolError>
{
    Resolve(E),
    Cleanup(Cause),
    Completion(crate::completion::Fault)
}
impl<E: fmt::Display, Cause: fmt::Display> fmt::Display for Failure<E, Cause>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Resolve(error) => write!(f, "cleanup provider resolution failed: {error}"),
            Self::Cleanup(error) => write!(f, "registered cleanup refused: {error}"),
            Self::Completion(error) => write!(f, "retained cleanup completion: {error}")
        }
    }
}
#[derive(Debug)]
pub struct Stopped
{
    pub report: Report,
    pub error:  CompletionError
}
impl fmt::Display for Stopped
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        write!(f,
               "owner cleanup could not retain its failed ticket: {}; death barrier remains closed",
               self.error)
    }
}
impl core::error::Error for Stopped
{
}

/// `resolve` borrows context only long enough to obtain an owned invocation
/// pin. Invocation then receives the mutable context with every facility/host
/// lookup lock released. Children may be released recursively; invalidation
/// makes those entries disappear from this walk naturally.
pub fn drain<C: ?Sized, G: InvocationGate, E, F: CleanupFailure>(context: &mut C,
                                                                 owner: &OwnerRef<G>,
                                                                 mut resolve: impl FnMut(&C,
                                            ResourceKey)
                                            -> Result<Invocation<C, G, F>, E>,
                                                                 mut failed: impl FnMut(ResourceKey,
                                                               &Failure<E, F::Cause>))
                                                                 -> Result<Report, Stopped>
{
    owner.stop_admission();
    run(context, owner, false, &mut resolve, &mut failed)
}

/// Retry each currently quarantined ticket at most once. Rotating pool/slot
/// cursors prevent a permanently failing entry from starving its neighbours.
pub fn retry<C: ?Sized, G: InvocationGate, E, F: CleanupFailure>(context: &mut C,
                                                                 owner: &OwnerRef<G>,
                                                                 mut resolve: impl FnMut(&C,
                                            ResourceKey)
                                            -> Result<Invocation<C, G, F>, E>,
                                                                 mut failed: impl FnMut(ResourceKey,
                                                               &Failure<E, F::Cause>))
                                                                 -> Result<Report, Stopped>
{
    run(context, owner, true, &mut resolve, &mut failed)
}
fn run<C: ?Sized, G: InvocationGate, E, F: CleanupFailure>(context: &mut C, owner: &OwnerRef<G>,
                                                           retry: bool,
                                                           resolve: &mut impl FnMut(&C,
                                                              ResourceKey)
                                                              -> Result<Invocation<C,
                                                                                   G,
                                                                                   F>,
                                                                        E>,
                                                           failed: &mut impl FnMut(ResourceKey,
                                                                      &Failure<E,
                                                                               F::Cause>))
                                                           -> Result<Report, Stopped>
{
    let mut report = Report::default();
    // Completion refusals already own their unique tickets. Retry only their
    // metadata acknowledgement, never an interrupted payload destructor.
    for _ in 0..owner.summary().completion_faults
    {
        match owner.retry_completion()
        {
            crate::completion::Retry::Empty => break,
            crate::completion::Retry::Complete(record) =>
            {
                drop(record);
                report.released += 1;
            }
            crate::completion::Retry::Retained(fault)
                if fault.cause == crate::completion::Cause::AwaitingTicket =>
            {
                report.deferred += 1;
            }
            crate::completion::Retry::Retained(fault) =>
            {
                report.quarantined += 1;
                failed(fault.key, &Failure::Completion(fault));
            }
        }
    }
    let attempts = if retry { owner.summary().quarantined } else { usize::MAX };
    for _ in 0..attempts
    {
        let ticket = if retry { owner.resume_quarantined() } else { owner.invalidate_next() };
        let Some(ticket) = ticket
        else
        {
            break;
        };
        report.invalidated += usize::from(!retry);
        let ticket = match owner.route_completion(ticket)
        {
            crate::completion::Routed::Continue(ticket) => ticket,
            crate::completion::Routed::Complete(record) =>
            {
                drop(record);
                report.released += 1;
                continue;
            }
            crate::completion::Routed::Retained(fault) =>
            {
                report.quarantined += 1;
                failed(fault.key, &Failure::Completion(fault));
                continue;
            }
        };
        let key = ticket.key();
        let (ticket, error) = match resolve(context, key)
        {
            Ok(invocation) =>
            {
                match invocation.invoke(context, ticket)
                {
                    Ok(()) =>
                    {
                        if owner.pending(key)
                        {
                            report.deferred += 1;
                        }
                        else
                        {
                            report.released += 1;
                        }
                        continue;
                    }
                    Err(error) =>
                    {
                        let (ticket, cause) = error.into_parts();
                        (ticket, Failure::Cleanup(cause))
                    }
                }
            }
            Err(error) => (ticket, Failure::Resolve(error))
        };
        // Transfer ticket ownership before reporting: even reporting cannot
        // accidentally drop the only recovery capability on its error path.
        if let Err(error) = owner.quarantine(ticket)
        {
            return Err(Stopped { report,
                                 error });
        }
        report.quarantined += 1;
        failed(key, &error);
    }
    Ok(report)
}
