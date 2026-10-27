use mac_abi::{BAD_ARGUMENT, IO_ERROR, SERVICE_BUSY};
use mac_byte_channel::{self as wire, State, Status};
use mac_driver::{Completion, Error, Phase, Request, Result};
use mac_virtio_console::{Channel, Diagnostic};
use vdagent_channel::engine::{Connection, request};
struct Peer
{
    ready:      bool,
    error:      Option<i32>,
    epoch:      u64,
    calls:      usize,
    bytes:      Vec<u8>,
    diagnostic: Diagnostic,
    limit:      usize
}
impl Peer
{
    fn new() -> Self
    {
        Self { ready:      true,
               error:      None,
               epoch:      1,
               calls:      0,
               bytes:      Vec::new(),
               diagnostic: Diagnostic::Ready,
               limit:      2 }
    }
}
impl Channel for Peer
{
    fn monotonic_microseconds(&self) -> u64
    {
        0
    }

    fn poll(&mut self) -> Result<bool>
    {
        self.calls += 1;
        self.error.map_or(Ok(self.ready), |code| Err(Error(code)))
    }

    fn send(&mut self, bytes: &[u8]) -> Result<usize>
    {
        let n = bytes.len().min(self.limit);
        self.bytes.extend_from_slice(&bytes[..n]);
        Ok(n)
    }

    fn receive(&mut self, output: &mut [u8]) -> Result<usize>
    {
        let n = output.len().min(self.bytes.len());
        output[..n].copy_from_slice(&self.bytes[..n]);
        self.bytes.drain(..n);
        Ok(n)
    }
}
impl Connection for Peer
{
    fn generation(&self) -> u64
    {
        self.epoch
    }

    fn diagnostic(&self) -> Diagnostic
    {
        self.diagnostic
    }

    fn unavailable(&self) -> bool
    {
        self.diagnostic == Diagnostic::Profile
    }
}
fn call(operation: u32, input: &[u8]) -> Request<'_>
{
    Request { ticket: 19,
              phase: Phase::Start,
              class: wire::CLASS,
              operation,
              input }
}
#[test]
fn partial_transfers_backpressure_and_short_reads_preserve_stream_order()
{
    let mut peer = Peer::new();
    let mut reply = [0; 16];
    assert_eq!(request(&mut peer, call(wire::WRITE, b"abcdef"), &mut reply),
               Ok(Completion::Complete(4)));
    assert_eq!(wire::accepted(&reply[..4], 6), Ok(2));
    assert_eq!(peer.bytes, b"ab");
    peer.limit = 0;
    request(&mut peer, call(wire::WRITE, b"cdef"), &mut reply).unwrap();
    assert_eq!(wire::accepted(&reply[..4], 4), Ok(0));
    assert_eq!(request(&mut peer, call(wire::READ, &[]), &mut reply[..1]),
               Ok(Completion::Complete(1)));
    assert_eq!(reply[0], b'a');
    assert_eq!(peer.bytes, b"b");
    assert_eq!(request(&mut peer, call(wire::READ, &[]), &mut reply), Ok(Completion::Complete(1)));
    assert_eq!(request(&mut peer, call(wire::READ, &[]), &mut reply), Ok(Completion::Complete(0)));
    assert_eq!(peer.calls, 5);
}
#[test]
fn connection_generations_and_refusal_causes_are_visible_without_byte_io()
{
    let mut peer = Peer::new();
    let mut output = [0; 256];
    request(&mut peer, call(wire::STATUS, &[]), &mut output).unwrap();
    assert_eq!(Status::decode(&output[..16]).unwrap().state, State::Open);
    peer.error = Some(IO_ERROR);
    peer.diagnostic = Diagnostic::Disconnected;
    request(&mut peer, call(wire::STATUS, &[]), &mut output).unwrap();
    let status = Status::decode(&output[..16]).unwrap();
    assert_eq!(status.state, State::Disconnected);
    assert_eq!(status.generation, 1);
    assert_eq!(request(&mut peer, call(wire::WRITE, b"old"), &mut output), Err(Error(IO_ERROR)));
    assert!(peer.bytes.is_empty());
    peer.error = None;
    peer.epoch = 2;
    request(&mut peer, call(wire::STATUS, &[]), &mut output).unwrap();
    assert_eq!(Status::decode(&output[..16]).unwrap().generation, 2);
    peer.error = Some(mac_abi::NOT_SUPPORTED);
    peer.diagnostic = Diagnostic::Profile;
    request(&mut peer, call(wire::STATUS, &[]), &mut output).unwrap();
    assert_eq!(Status::decode(&output[..16]).unwrap().state, State::Unavailable);
    let Completion::Complete(n) =
        request(&mut peer, call(wire::ERROR_TEXT, &[]), &mut output).unwrap()
    else
    {
        panic!("synchronous diagnostic invariant");
    };
    assert!(std::str::from_utf8(&output[..n]).unwrap().contains("max_ports=2"));
    assert!(std::str::from_utf8(&output[..n]).unwrap().contains("com.redhat.spice.0"));
}
#[test]
fn invalid_requests_do_not_touch_hardware_and_cancel_is_immediate()
{
    let mut peer = Peer::new();
    let mut output = [0; 16];
    for request in [call(wire::STATUS, b"x"), call(wire::READ, b"x"), call(wire::WRITE, &[])]
    {
        assert_eq!(vdagent_channel::engine::request(&mut peer, request, &mut output),
                   Err(Error(BAD_ARGUMENT)));
    }
    assert_eq!(peer.calls, 0);
    let mut cancel = call(wire::WRITE, b"ignored");
    cancel.phase = Phase::Cancel;
    assert_eq!(request(&mut peer, cancel, &mut output), Ok(Completion::Complete(0)));
    assert_eq!(peer.calls, 0);
    peer.ready = false;
    assert_eq!(request(&mut peer, call(wire::WRITE, b"later"), &mut output),
               Err(Error(SERVICE_BUSY)));
    assert!(peer.bytes.is_empty());
}
#[test]
fn broker_contended_first_dispatch_can_poll_with_the_same_owned_write_bytes()
{
    let mut peer = Peer::new();
    let mut reply = [0; 4];
    let mut retried = call(wire::WRITE, b"owned");
    retried.phase = Phase::Poll;
    assert_eq!(request(&mut peer, retried, &mut reply), Ok(Completion::Complete(4)));
    assert_eq!(wire::accepted(&reply, 5), Ok(2));
    assert_eq!(peer.bytes, b"ow");
}
