//! Correlation/cancellation tests at the real module dependency API shape.
use std::{collections::VecDeque,
          future::Future,
          pin::pin,
          task::{Context, Poll, Waker}};

use mac_driver::{Completion, Error, Phase};
use mac_host_protocol::{self as wire, Arguments, Header, Operation};
use mac_object_table::{Kind, Table};
use vhost_filesystem::{Cause, Rpc,
                       transport::{Cancellation, Channel, DirectoryRpc}};
#[derive(Clone, Copy)]
enum Step
{
    BusyStart,
    Start,
    BusyPoll,
    FailPoll,
    CancelPending,
    Complete,
    WrongTicket,
    ZeroPending,
    TerminalError
}
struct Script
{
    steps:  VecDeque<Step>,
    ids:    Table<()>,
    header: Option<Header>,
    trace:  Vec<Phase>
}
impl Script
{
    fn new(steps: &[Step]) -> Self
    {
        Self { steps:  steps.iter().copied().collect(),
               ids:    Table::new(Kind::DriverRequests),
               header: None,
               trace:  Vec::new() }
    }
}
impl Channel for Script
{
    fn invoke(&mut self, ticket: &mut u32, phase: Phase, operation: u32, input: &[u8],
              output: &mut [u8])
              -> mac_driver::Result<Completion>
    {
        self.trace.push(phase);
        let step = self.steps.pop_front().expect("unexpected extra transport callback");
        if phase != Phase::Start
        {
            assert!(input.is_empty());
            assert_eq!(*ticket, self.header.unwrap().request);
        }
        match step
        {
            Step::BusyStart =>
            {
                assert_eq!(phase, Phase::Start);
                assert_eq!(*ticket, 0);
                Err(Error(mac_abi::SERVICE_BUSY))
            }
            Step::Start =>
            {
                assert_eq!(phase, Phase::Start);
                assert_eq!(*ticket, 0);
                let args = Arguments::decode(input).unwrap();
                assert_eq!(args.payload, b"commit bytes");
                *ticket = self.ids.insert(()).unwrap();
                self.header = Some(args.header(Operation::try_from(operation as u16).unwrap(),
                                               *ticket)
                                       .unwrap());
                Ok(Completion::Pending)
            }
            Step::BusyPoll =>
            {
                assert_eq!(phase, Phase::Poll);
                Err(Error(mac_abi::SERVICE_BUSY))
            }
            Step::FailPoll =>
            {
                assert_eq!(phase, Phase::Poll);
                Err(Error(mac_abi::IO_ERROR))
            }
            Step::CancelPending =>
            {
                assert_eq!(phase, Phase::Cancel);
                Ok(Completion::Pending)
            }
            Step::Complete | Step::WrongTicket =>
            {
                let mut h = self.header.unwrap().response();
                h.payload = 4;
                h.object = 0x4444_0001;
                if matches!(step, Step::WrongTicket)
                {
                    h.request = h.request.wrapping_add(1);
                }
                output[..wire::HEADER_BYTES].copy_from_slice(&h.encode().unwrap());
                output[wire::HEADER_BYTES..wire::HEADER_BYTES + 4].copy_from_slice(b"done");
                self.ids.remove(*ticket);
                *ticket = 0;
                Ok(Completion::Complete(wire::HEADER_BYTES + 4))
            }
            Step::ZeroPending =>
            {
                assert_eq!(*ticket, 0);
                Ok(Completion::Pending)
            }
            Step::TerminalError =>
            {
                self.ids.remove(*ticket);
                *ticket = 0;
                Err(Error(mac_abi::IO_ERROR))
            }
        }
    }
}
fn args() -> Arguments<'static>
{
    Arguments { session: 22,
                object:  0x4443_0001,
                offset:  0,
                value:   0,
                payload: b"commit bytes" }
}
fn run<T>(future: impl Future<Output = T>) -> T
{
    let mut f = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    for _ in 0..100
    {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx)
        {
            return v;
        }
    }
    panic!("bounded transport did not complete")
}
#[test]
fn not_ready_start_is_retried_but_live_contention_never_restarts_a_mutation()
{
    use Step::*;
    let mut rpc = DirectoryRpc::new(Script::new(&[BusyStart, Start, BusyPoll, Complete]),
                                    Cancellation::default());
    let reply = run(rpc.invoke(Operation::Commit, args())).unwrap();
    assert_eq!(reply.body, b"done");
    assert!(!rpc.is_pending());
    assert_eq!(rpc.channel().trace, [Phase::Start, Phase::Start, Phase::Poll, Phase::Poll]);
}
#[test]
fn abandoned_future_keeps_its_ticket_and_cancel_delivers_the_actual_acknowledgement()
{
    use Step::*;
    let cancellation = Cancellation::default();
    let mut rpc =
        DirectoryRpc::new(Script::new(&[Start, CancelPending, Complete]), cancellation.clone());
    {
        let mut f = pin!(rpc.invoke(Operation::Commit, args()));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(f.as_mut().poll(&mut cx).is_pending());
        assert!(f.as_mut().poll(&mut cx).is_pending());
    }
    assert!(rpc.is_pending());
    assert_eq!(run(rpc.invoke(Operation::Commit, args())).unwrap_err().cause, Cause::Busy);
    cancellation.cancel();
    let reply = run(rpc.drain()).unwrap().unwrap();
    assert_eq!(reply.header.object, 0x4444_0001);
    assert_eq!(reply.body, b"done");
    assert!(!rpc.is_pending());
    assert!(run(rpc.drain()).unwrap().is_none());
    assert_eq!(rpc.channel().trace, [Phase::Start, Phase::Cancel, Phase::Cancel]);
    assert_eq!(run(rpc.invoke(Operation::Commit, args())).unwrap_err().cause, Cause::Cancelled);
}
#[test]
fn live_provider_error_drains_before_reporting_and_does_not_erase_a_host_commit()
{
    use Step::*;
    let mut rpc =
        DirectoryRpc::new(Script::new(&[Start, FailPoll, Complete]), Cancellation::default());
    assert_eq!(run(rpc.invoke(Operation::Commit, args())).unwrap().body, b"done");
    assert_eq!(rpc.channel().trace, [Phase::Start, Phase::Poll, Phase::Cancel]);
    assert!(!rpc.is_pending());
    let mut rpc = DirectoryRpc::new(Script::new(&[Start, FailPoll, CancelPending, TerminalError]),
                                    Cancellation::default());
    assert_eq!(run(rpc.invoke(Operation::Commit, args())).unwrap_err().cause,
               Cause::Transport(mac_abi::IO_ERROR));
    assert!(!rpc.is_pending());
}
#[test]
fn malformed_correlation_and_pending_without_a_ticket_fail_closed()
{
    use Step::*;
    let mut rpc = DirectoryRpc::new(Script::new(&[Start, WrongTicket]), Cancellation::default());
    assert_eq!(run(rpc.invoke(Operation::Commit, args())).unwrap_err().cause, Cause::Invalid);
    assert!(!rpc.is_pending());
    let mut rpc = DirectoryRpc::new(Script::new(&[ZeroPending]), Cancellation::default());
    assert_eq!(run(rpc.invoke(Operation::Commit, args())).unwrap_err().cause, Cause::Invalid);
    assert!(!rpc.is_pending());
}
