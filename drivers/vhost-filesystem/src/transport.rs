//! Bounded asynchronous adapter over the independently loaded HDIR provider.
//! The operation and correlation survive future cancellation. A native module
//! must keep this client pinned and drain any live ticket before releasing it.
use alloc::{sync::Arc, vec::Vec};
use core::{future::poll_fn,
           sync::atomic::{AtomicU8, Ordering},
           task::{Context, Poll}};

use mac_driver::{Completion, Phase, Provider};
use mac_host_protocol::{self as wire, Arguments, Header, Operation};

use crate::{Cause, Reply, Result, Rpc, rpc::buffer};
/// Small testable boundary; the production implementation is the checked module
/// dependency capability, not a code pointer or a global transport instance.
pub trait Channel: Send
{
    fn invoke(&mut self, ticket: &mut u32, phase: Phase, operation: u32, input: &[u8],
              output: &mut [u8])
              -> mac_driver::Result<Completion>;
}
impl Channel for Provider
{
    fn invoke(&mut self, ticket: &mut u32, phase: Phase, operation: u32, input: &[u8],
              output: &mut [u8])
              -> mac_driver::Result<Completion>
    {
        Provider::invoke(*self, ticket, phase, operation, input, output)
    }
}
#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicU8>);
impl Cancellation
{
    pub fn cancel(&self)
    {
        // Cleanup is irrevocable once entered. Repeated CANCEL callbacks must
        // not cancel the close/unlock requests which acknowledge cancellation.
        let _ = self.0.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    }

    pub fn reset(&self)
    {
        self.0.store(0, Ordering::Release)
    }

    pub fn requested(&self) -> bool
    {
        self.0.load(Ordering::Acquire) == 1
    }

    pub(crate) fn begin_cleanup(&self) -> bool
    {
        self.0.swap(2, Ordering::AcqRel) == 1
    }
}
struct Flight
{
    ticket:    u32,
    operation: Operation,
    forced:    Option<Cause>
}
pub struct DirectoryRpc<C: Channel>
{
    channel:      C,
    cancellation: Cancellation,
    input:        Vec<u8>,
    output:       Vec<u8>,
    flight:       Option<Flight>
}
impl<C: Channel> DirectoryRpc<C>
{
    pub fn new(channel: C, cancellation: Cancellation) -> Self
    {
        Self { channel,
               cancellation,
               input: Vec::new(),
               output: Vec::new(),
               flight: None }
    }

    pub fn is_pending(&self) -> bool
    {
        self.flight.is_some()
    }

    pub fn channel(&mut self) -> &mut C
    {
        &mut self.channel
    }

    fn begin(&mut self, op: Operation, args: Arguments<'_>) -> Result<()>
    {
        if self.flight.is_some()
        {
            return Err(Cause::Busy.into());
        }
        if self.cancellation.requested()
        {
            return Err(Cause::Cancelled.into());
        }
        let length = wire::ARGUMENT_BYTES.checked_add(args.payload.len()).ok_or(Cause::Invalid)?;
        if args.payload.len() > wire::MAX_PAYLOAD
        {
            return Err(Cause::Invalid.into());
        }
        if length > self.input.len()
        {
            self.input.try_reserve_exact(length - self.input.len()).map_err(|_| Cause::NoMemory)?;
        }
        self.input.resize(length, 0);
        args.encode(&mut self.input).map_err(|_| Cause::Invalid)?;
        if self.output.is_empty()
        {
            self.output = buffer(wire::HEADER_BYTES + wire::MAX_PAYLOAD)?;
        }
        self.flight = Some(Flight { ticket:    0,
                                    operation: op,
                                    forced:    None });
        Ok(())
    }

    fn poll_reply(&mut self, cx: &mut Context<'_>, draining: bool) -> Poll<Result<Reply>>
    {
        let Some(flight) = self.flight.as_mut()
        else
        {
            return Poll::Ready(Err(Cause::Invalid.into()));
        };
        let cancel = draining || self.cancellation.requested() || flight.forced.is_some();
        if cancel && flight.ticket == 0
        {
            let cause = flight.forced.unwrap_or(Cause::Cancelled);
            self.flight = None;
            return Poll::Ready(Err(cause.into()));
        }
        let phase = if flight.ticket == 0
        {
            Phase::Start
        }
        else if cancel
        {
            Phase::Cancel
        }
        else
        {
            Phase::Poll
        };
        let expected_ticket = flight.ticket;
        let result = self.channel.invoke(&mut flight.ticket,
                                         phase,
                                         flight.operation as u32,
                                         if phase == Phase::Start { &self.input } else { &[] },
                                         &mut self.output);
        match result
        {
            Ok(Completion::Pending) if flight.ticket != 0 =>
            {}
            Err(mac_driver::Error(mac_abi::SERVICE_BUSY))
                if phase == Phase::Start && flight.ticket == 0 =>
            {
                // HDIR specifically guarantees a disconnected START has made
                // no progress and consumed no operation. Never retry a POLL or
                // an arbitrary provider's terminal mutation this way.
            }
            Ok(Completion::Complete(count)) if flight.ticket == 0 =>
            {
                let operation = flight.operation;
                self.flight = None;
                // Cancellation/admission failure cannot erase an acknowledged
                // host mutation or acquired capability. Deliver its real reply
                // so the filesystem can publish or clean up the actual result.
                let result = (|| {
                    let bytes = self.output.get(..count).ok_or(Cause::Invalid)?;
                    let header = Header::decode(bytes.get(..wire::HEADER_BYTES)
                                                     .ok_or(Cause::Invalid)?).map_err(|error| {
                                     if error == wire::Error::Version
                                     {
                                         Cause::Transport(mac_abi::BAD_ABI)
                                     }
                                     else
                                     {
                                         Cause::Invalid
                                     }
                                 })?;
                    if !header.reply
                       || header.operation != operation
                       || expected_ticket != 0 && header.request != expected_ticket
                       || bytes.len() != wire::HEADER_BYTES + header.payload as usize
                    {
                        return Err(Cause::Invalid.into());
                    }
                    let mut body = buffer(header.payload as usize)?;
                    body.copy_from_slice(&bytes[wire::HEADER_BYTES..]);
                    Ok(Reply { header,
                               body })
                })();
                return Poll::Ready(result);
            }
            Err(mac_driver::Error(status)) =>
            {
                if flight.ticket == 0
                {
                    let cause = flight.forced.unwrap_or(Cause::Transport(status));
                    self.flight = None;
                    return Poll::Ready(Err(cause.into()));
                }
                // Admission may contend while a live request already exists.
                // Keep its ticket and retry; other failures force cancellation
                // and remain owned until the lower provider acknowledges it.
                if status != mac_abi::SERVICE_BUSY
                {
                    flight.forced.get_or_insert(Cause::Transport(status));
                }
            }
            Ok(_) =>
            {
                if flight.ticket == 0
                {
                    self.flight = None;
                    return Poll::Ready(Err(Cause::Invalid.into()));
                }
                flight.forced.get_or_insert(Cause::Invalid);
            }
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    }

    /// Finish cancellation of a future abandoned while its lower request was
    /// live. A successful reply is the actual outcome, including mutations that
    /// had already reached the host; cancellation is never an undo guarantee.
    pub async fn drain(&mut self) -> Result<Option<Reply>>
    {
        if self.flight.is_none()
        {
            return Ok(None);
        }
        poll_fn(|cx| self.poll_reply(cx, true)).await.map(Some)
    }
}
impl<C: Channel> Rpc for DirectoryRpc<C>
{
    async fn invoke(&mut self, operation: Operation, args: Arguments<'_>) -> Result<Reply>
    {
        self.begin(operation, args)?;
        // Even a dependency that completes START synchronously gets only one
        // invocation per upper callback. Yield before dispatch, while the
        // owned flight is still cancellable without a host side effect.
        crate::rpc::yield_once().await;
        poll_fn(|cx| self.poll_reply(cx, false)).await
    }
}
