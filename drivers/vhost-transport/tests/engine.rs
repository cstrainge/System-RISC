use std::collections::VecDeque;

use mac_abi::{BAD_ABI, IO_ERROR, SERVICE_BUSY};
use mac_driver::{ClassId, Completion, Error, Phase, Request, Result};
use mac_host_protocol::{self as wire, Arguments, Header, Operation};
use vhost_transport::engine::{Buffers, Channel, Engine, FRAGMENT, MAX_PACKET};
struct Peer
{
    ready:         bool,
    lost:          bool,
    tx:            Vec<u8>,
    rx:            VecDeque<u8>,
    send_limit:    usize,
    receive_limit: usize,
    polls:         usize,
    sends:         usize,
    receives:      usize,
    stale:         bool,
    replied:       bool,
    now:           u64
}
impl Peer
{
    fn new() -> Self
    {
        Self { ready:         true,
               lost:          false,
               tx:            Vec::new(),
               rx:            VecDeque::new(),
               send_limit:    FRAGMENT,
               receive_limit: FRAGMENT,
               polls:         0,
               sends:         0,
               receives:      0,
               stale:         false,
               replied:       false,
               now:           0 }
    }

    fn reset_counts(&mut self)
    {
        self.polls = 0;
        self.sends = 0;
        self.receives = 0;
    }
}
impl Channel for Peer
{
    fn monotonic_microseconds(&self) -> u64
    {
        self.now
    }

    fn poll(&mut self) -> Result<bool>
    {
        self.polls += 1;
        if self.lost { Err(Error(IO_ERROR)) } else { Ok(self.ready) }
    }

    fn send(&mut self, bytes: &[u8]) -> Result<usize>
    {
        self.sends += 1;
        assert!(bytes.len() <= FRAGMENT);
        let count = bytes.len().min(self.send_limit);
        self.tx.extend_from_slice(&bytes[..count]);
        if !self.replied && self.tx.len() >= wire::HEADER_BYTES
        {
            let header = Header::decode(&self.tx[..wire::HEADER_BYTES]).unwrap();
            if self.tx.len() == wire::HEADER_BYTES + header.payload as usize
            {
                let mut reply = header.response();
                reply.payload = 9000;
                if self.stale
                {
                    reply.request += 1;
                }
                self.rx.extend(reply.encode().unwrap());
                self.rx.extend((0..9000).map(|i| (i * 17) as u8));
                self.replied = true;
            }
        }
        Ok(count)
    }

    fn receive(&mut self, out: &mut [u8]) -> Result<usize>
    {
        self.receives += 1;
        let count = out.len().min(self.rx.len()).min(self.receive_limit);
        for byte in &mut out[..count]
        {
            *byte = self.rx.pop_front().unwrap();
        }
        Ok(count)
    }
}
fn call<'a>(ticket: u32, phase: Phase, input: &'a [u8]) -> Request<'a>
{
    Request { ticket,
              phase,
              class: ClassId(wire::DIRECTORY_CLASS),
              operation: Operation::Write as u32,
              input }
}
fn arguments(payload: &[u8]) -> Vec<u8>
{
    let mut bytes = vec![0; wire::ARGUMENT_BYTES + payload.len()];
    Arguments { session: 0x10001,
                object: 0x20001,
                offset: 0x1000_0001,
                value: 0,
                payload }.encode(&mut bytes)
                         .unwrap();
    bytes
}
#[test]
fn calls_are_bounded_and_replies_never_retain_borrows_to_the_callers_input()
{
    let mut engine = Engine::new();
    let mut buffers = Box::new(Buffers::new());
    let mut peer = Peer::new();
    peer.send_limit = 512;
    peer.receive_limit = 317;
    let payload = vec![0xab; wire::MAX_PAYLOAD];
    let mut input = arguments(&payload);
    let mut output = vec![0; MAX_PACKET];
    assert_eq!(engine.request(&mut peer,
                              call(0x10001, Phase::Start, &input),
                              &mut output,
                              &mut buffers),
               Ok(Completion::Pending));
    input.fill(0); // The started operation owns its full transmitted frame.
    let mut terminal = false;
    for _ in 0..200
    {
        peer.reset_counts();
        let result =
            engine.request(&mut peer, call(0x10001, Phase::Poll, &[]), &mut output, &mut buffers)
                  .unwrap();
        assert!(peer.polls <= 1 && peer.sends <= 2 && peer.receives <= 2);
        if let Completion::Complete(length) = result
        {
            assert_eq!(length, wire::HEADER_BYTES + 9000);
            terminal = true;
            break;
        }
    }
    assert!(terminal);
    let header = Header::decode(&peer.tx[..wire::HEADER_BYTES]).unwrap();
    assert_eq!(header.request, 0x10001);
    assert_eq!(&peer.tx[wire::HEADER_BYTES..], payload);
    let header = Header::decode(&output[..wire::HEADER_BYTES]).unwrap();
    assert!(header.reply);
    assert_eq!(header.request, 0x10001);
    assert_eq!(&output[wire::HEADER_BYTES..wire::HEADER_BYTES + 9000],
               (0..9000).map(|i| (i * 17) as u8).collect::<Vec<_>>());
}
#[test]
fn a_silent_peer_times_out_and_poisoned_incarnations_never_reuse_late_replies()
{
    for phase in [Phase::Poll, Phase::Cancel]
    {
        let mut engine = Engine::new();
        let mut buffers = Box::new(Buffers::new());
        let mut peer = Peer::new();
        peer.receive_limit = 0;
        let input = arguments(b"data");
        let mut output = vec![0; MAX_PACKET];
        assert_eq!(engine.request(&mut peer,
                                  call(7, Phase::Start, &input),
                                  &mut output,
                                  &mut buffers),
                   Ok(Completion::Pending));
        peer.now = vhost_transport::engine::STALL_MICROSECONDS - 1;
        assert_eq!(engine.request(&mut peer, call(7, phase, &[]), &mut output, &mut buffers),
                   Ok(Completion::Pending));
        peer.now += 1;
        assert_eq!(engine.request(&mut peer, call(7, phase, &[]), &mut output, &mut buffers),
                   Err(Error(IO_ERROR)));
        peer.receive_limit = FRAGMENT;
        assert_eq!(engine.request(&mut peer,
                                  call(8, Phase::Start, &input),
                                  &mut output,
                                  &mut buffers),
                   Err(Error(IO_ERROR)));
        assert!(!peer.rx.is_empty(), "a late reply must never be consumed by another request");
    }
}

#[test]
fn pending_connection_does_not_consume_a_request_or_poison_the_transport()
{
    let mut engine = Engine::new();
    let mut buffers = Box::new(Buffers::new());
    let mut peer = Peer::new();
    peer.ready = false;
    let input = arguments(b"data");
    let mut output = vec![0; MAX_PACKET];
    assert_eq!(engine.request(&mut peer, call(1, Phase::Start, &input), &mut output, &mut buffers),
               Err(Error(SERVICE_BUSY)));
    assert!(peer.tx.is_empty());
    peer.ready = true;
    assert_eq!(engine.request(&mut peer, call(2, Phase::Start, &input), &mut output, &mut buffers),
               Ok(Completion::Pending));
}
#[test]
fn cancellation_drains_the_sent_operation_and_preserves_its_actual_result()
{
    let mut engine = Engine::new();
    let mut buffers = Box::new(Buffers::new());
    let mut peer = Peer::new();
    peer.send_limit = 128;
    let input = arguments(&[0xac; 4096]);
    let mut output = vec![0; MAX_PACKET];
    engine.request(&mut peer, call(7, Phase::Start, &input), &mut output, &mut buffers).unwrap();
    let mut done = false;
    for _ in 0..100
    {
        if matches!(engine.request(&mut peer,
                                   call(7, Phase::Cancel, &[]),
                                   &mut output,
                                   &mut buffers)
                          .unwrap(),
                    Completion::Complete(_))
        {
            done = true;
            break;
        }
    }
    assert!(done);
    assert_eq!(Header::decode(&output[..wire::HEADER_BYTES]).unwrap().status, 0);
    assert_eq!(peer.tx.len(), wire::HEADER_BYTES + 4096);
}
#[test]
fn a_foreign_reply_or_connection_loss_poison_the_incarnation_instead_of_aliasing_new_work()
{
    for stale in [false, true]
    {
        let mut engine = Engine::new();
        let mut buffers = Box::new(Buffers::new());
        let mut peer = Peer::new();
        peer.stale = stale;
        let input = arguments(b"data");
        let mut output = vec![0; MAX_PACKET];
        let result =
            engine.request(&mut peer, call(1, Phase::Start, &input), &mut output, &mut buffers);
        if stale
        {
            assert_eq!(result, Err(Error(BAD_ABI)));
        }
        else
        {
            assert_eq!(result, Ok(Completion::Pending));
            peer.lost = true;
            assert_eq!(engine.request(&mut peer,
                                      call(1, Phase::Poll, &[]),
                                      &mut output,
                                      &mut buffers),
                       Err(Error(IO_ERROR)));
        }
        assert_eq!(engine.request(&mut peer,
                                  call(2, Phase::Start, &input),
                                  &mut output,
                                  &mut buffers),
                   Err(Error(IO_ERROR)));
    }
}
