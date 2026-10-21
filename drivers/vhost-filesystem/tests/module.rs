//! Real host side effects behind split native module callbacks. Host-only
//! coverage: this deliberately does not claim resident/guest mount acceptance.
use std::{path::PathBuf,
          sync::{Arc, Mutex,
                 atomic::{AtomicU64, Ordering}}};

use mac_driver::{ClassId, Completion, Error, Phase, Request};
use mac_filesystem_interface::{self as fs, Arguments, Operation};
use mac_host_protocol::{self as wire, Header, Operation as HostOp};
use mac_object_table::{Kind, Table};
use risc_forks::Volume;
use risc_host::{Export, Session};
use vhost_filesystem::{module::Service, transport::Channel};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Drop for Temp
{
    fn drop(&mut self)
    {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[derive(Clone, Debug)]
struct Trace
{
    operation: HostOp,
    phase:     Phase,
    payload:   Vec<u8>
}
struct Host
{
    session:     Session,
    ids:         Table<()>,
    pending:     Option<(Header, Vec<u8>)>,
    trace:       Arc<Mutex<Vec<Trace>>>,
    synchronous: bool
}
impl Channel for Host
{
    fn invoke(&mut self, ticket: &mut u32, phase: Phase, operation: u32, input: &[u8],
              output: &mut [u8])
              -> mac_driver::Result<Completion>
    {
        let op = HostOp::try_from(operation as u16).unwrap();
        let payload = if phase == Phase::Start
        {
            assert_eq!(*ticket, 0);
            assert!(self.pending.is_none());
            let args = wire::Arguments::decode(input).unwrap();
            *ticket = self.ids.insert(()).unwrap();
            let header = args.header(op, *ticket).unwrap();
            // Mutate at START, with acknowledgement delayed until POLL/CANCEL.
            // This models a real host commit racing cancellation.
            self.pending = Some(self.session.respond(header, args.payload));
            args.payload.to_vec()
        }
        else
        {
            assert!(input.is_empty());
            assert!(self.ids.get(*ticket).is_some());
            assert_eq!(self.pending.as_ref().unwrap().0.request, *ticket);
            Vec::new()
        };
        self.trace.lock().unwrap().push(Trace { operation: op,
                                                phase,
                                                payload });
        if phase == Phase::Start && !self.synchronous
        {
            return Ok(Completion::Pending);
        }
        let (header, body) = self.pending.take().unwrap();
        let count = wire::HEADER_BYTES + body.len();
        output[..wire::HEADER_BYTES].copy_from_slice(&header.encode().unwrap());
        output[wire::HEADER_BYTES..count].copy_from_slice(&body);
        self.ids.remove(*ticket).unwrap();
        *ticket = 0;
        Ok(Completion::Complete(count))
    }
}
struct Harness
{
    service:  Service<Host>,
    temp:     Temp,
    trace:    Arc<Mutex<Vec<Trace>>>,
    requests: Table<()>,
    volumes:  Table<()>,
    writes:   Table<()>,
    files:    Table<()>
}
impl Harness
{
    fn new(writable: bool, synchronous: bool) -> Self
    {
        let path = std::env::temp_dir().join(format!("fs-module-{}-{}",
                                                     std::process::id(),
                                                     NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&path).unwrap();
        let temp = Temp(path);
        Volume::initialize(&temp.0, "Shared Files").unwrap();
        let trace = Arc::new(Mutex::new(Vec::new()));
        let service = Service::new();
        service.initialize(Host {
            session: Session::new(Arc::new(vec![Export::open("Share", &temp.0, writable).unwrap()])).unwrap(),
            ids: Table::new(Kind::DriverRequests), pending: None, trace: trace.clone(), synchronous
        }).unwrap();
        Self { service,
               temp,
               trace,
               requests: Table::new(Kind::DriverRequests),
               volumes: Table::new(Kind::Volumes),
               writes: Table::new(Kind::FileWrites),
               files: Table::new(Kind::Files) }
    }

    fn bytes(args: Arguments<'_>) -> Vec<u8>
    {
        let mut out = vec![0; fs::HEADER_BYTES + args.payload.len()];
        args.encode(&mut out).unwrap();
        out
    }

    fn step(&self, ticket: u32, phase: Phase, op: Operation, input: &[u8], output: &mut [u8])
            -> mac_driver::Result<Completion>
    {
        let before = self.trace.lock().unwrap().len();
        let result = self.service.request(Request { ticket,
                                                    phase,
                                                    class: ClassId(fs::CLASS),
                                                    operation: op as u32,
                                                    input },
                                          output);
        assert!(self.trace.lock().unwrap().len() - before <= 1,
                "callback did more than one lower invocation");
        result
    }

    fn execute(&mut self, op: Operation, args: Arguments<'_>) -> mac_driver::Result<Vec<u8>>
    {
        let ticket = self.requests.insert(()).unwrap();
        let input = Self::bytes(args);
        let result = self.finish(ticket, Phase::Start, op, &input);
        self.requests.remove(ticket).unwrap();
        result
    }

    fn finish(&self, ticket: u32, first: Phase, op: Operation, input: &[u8])
              -> mac_driver::Result<Vec<u8>>
    {
        for i in 0..10_000
        {
            // Every callback receives fresh, poisoned caller storage.
            let mut out = vec![0xa5; fs::MAX_REPLY];
            let phase = if i == 0
            {
                first
            }
            else if first == Phase::Cancel
            {
                Phase::Cancel
            }
            else
            {
                Phase::Poll
            };
            match self.step(ticket, phase, op, if i == 0 { input } else { &[] }, &mut out)?
            {
                Completion::Complete(n) =>
                {
                    out.truncate(n);
                    return Ok(out);
                }
                Completion::Pending =>
                {}
            }
        }
        panic!("bounded filesystem request did not finish")
    }

    fn mount(&mut self) -> u32
    {
        let volume = self.volumes.insert(()).unwrap();
        let out = self.execute(Operation::Mount,
                               Arguments { volume,
                                           ..Default::default() })
                      .unwrap();
        assert_eq!(fs::Mount::decode(&out).unwrap().root.name, "Shared Files");
        volume
    }

    fn begin(&mut self, volume: u32, name: &str, data: usize, resource: usize) -> (u32, u32)
    {
        let token = self.writes.insert(()).unwrap();
        let out = self.execute(Operation::BeginCreate,
                               Arguments { volume,
                                           token,
                                           parent: 2,
                                           length: data as u64,
                                           auxiliary: resource as u64,
                                           when: 3_800_000_000_000_000,
                                           payload: name.as_bytes(),
                                           ..Default::default() })
                      .unwrap();
        (token, u32::from_le_bytes(out.try_into().unwrap()))
    }

    fn until_host_start(&self, ticket: u32, op: Operation, input: &[u8], target: HostOp)
    {
        for i in 0..10_000
        {
            let mut out = vec![0; fs::MAX_REPLY];
            assert_eq!(self.step(ticket,
                                 if i == 0 { Phase::Start } else { Phase::Poll },
                                 op,
                                 if i == 0 { input } else { &[] },
                                 &mut out)
                           .unwrap(),
                       Completion::Pending);
            if self.trace
                   .lock()
                   .unwrap()
                   .last()
                   .is_some_and(|t| t.operation == target && t.phase == Phase::Start)
            {
                return;
            }
        }
        panic!("never reached {target:?}");
    }
}

#[test]
fn fresh_callback_buffers_round_trip_both_forks_and_quiesce_releases_mounts()
{
    // A dependency may complete START immediately. It must still be bounded.
    let mut h = Harness::new(true, true);
    let volume = h.mount();
    let (token, node) = h.begin(volume, "Hello", 5, 4);
    for (flags, payload) in [(fs::DATA_FORK, &b"hello"[..]), (fs::RESOURCE_FORK, &b"rsrc"[..])]
    {
        h.execute(Operation::WriteChunk,
                  Arguments { volume,
                              token,
                              flags,
                              payload,
                              ..Default::default() })
         .unwrap();
    }
    let out = h.execute(Operation::FinishWrite,
                        Arguments { volume,
                                    token,
                                    ..Default::default() })
               .unwrap();
    assert_eq!(out, node.to_le_bytes());
    let open = h.files.insert(()).unwrap();
    h.execute(Operation::Open,
              Arguments { volume,
                          node,
                          token: open,
                          ..Default::default() })
     .unwrap();
    for (flags, expected) in [(fs::DATA_FORK, &b"hello"[..]), (fs::RESOURCE_FORK, &b"rsrc"[..])]
    {
        assert_eq!(h.execute(Operation::Read,
                             Arguments { volume,
                                         token: open,
                                         flags,
                                         length: 20,
                                         ..Default::default() })
                    .unwrap(),
                   expected);
    }
    assert_eq!(h.service.quiesce(), Err(Error(mac_abi::SERVICE_BUSY)));
    h.execute(Operation::Close,
              Arguments { volume,
                          token: open,
                          ..Default::default() })
     .unwrap();
    h.execute(Operation::Unmount,
              Arguments { volume,
                          ..Default::default() })
     .unwrap();
    assert_eq!(h.service.quiesce(), Ok(()));
    assert_eq!(Volume::open(&h.temp.0).unwrap().check().unwrap(), 2);
}

#[test]
fn cancellation_after_host_commit_returns_actual_identity_without_republishing()
{
    let mut h = Harness::new(true, false);
    let volume = h.mount();
    let (token, node) = h.begin(volume, "Committed", 0, 0);
    h.trace.lock().unwrap().clear();
    let ticket = h.requests.insert(()).unwrap();
    let input = Harness::bytes(Arguments { volume,
                                           token,
                                           ..Default::default() });
    h.until_host_start(ticket, Operation::FinishWrite, &input, HostOp::Commit);
    let out = h.finish(ticket, Phase::Cancel, Operation::FinishWrite, &[]).unwrap();
    assert_eq!(out, node.to_le_bytes());
    let trace = h.trace.lock().unwrap();
    assert_eq!(trace.iter()
                    .filter(|t| {
                        t.operation == HostOp::Commit
                        && t.phase == Phase::Start
                        && t.payload.starts_with(b"f-")
                    })
                    .count(),
               1);
    assert!(trace.iter().any(|t| t.operation == HostOp::Commit && t.phase == Phase::Cancel));
    drop(trace);
    assert_eq!(Volume::open(&h.temp.0).unwrap().resolve("/Committed").unwrap().metadata.catalog_id,
               node);
    h.execute(Operation::Unmount,
              Arguments { volume,
                          ..Default::default() })
     .unwrap();
    h.service.quiesce().unwrap();
}

#[test]
fn repeated_cancel_before_commit_drains_cleanup_and_discards_unpublished_stage()
{
    let mut h = Harness::new(true, false);
    let volume = h.mount();
    let (token, _) = h.begin(volume, "Cancelled", 0, 0);
    h.trace.lock().unwrap().clear();
    let ticket = h.requests.insert(()).unwrap();
    let input = Harness::bytes(Arguments { volume,
                                           token,
                                           ..Default::default() });
    h.until_host_start(ticket, Operation::FinishWrite, &input, HostOp::Flush);
    assert_eq!(h.finish(ticket, Phase::Cancel, Operation::FinishWrite, &[]),
               Err(Error(mac_abi::OPERATION_CANCELLED)));
    assert!(Volume::open(&h.temp.0).unwrap().resolve("/Cancelled").is_err());
    assert!(!h.trace.lock().unwrap().iter().any(|t| t.operation == HostOp::Commit));
    h.execute(Operation::Unmount,
              Arguments { volume,
                          ..Default::default() })
     .unwrap();
    h.service.quiesce().unwrap();
    Volume::open(&h.temp.0).unwrap().check().unwrap();
}

#[test]
fn small_completed_reply_and_foreign_ticket_cannot_lose_or_replace_live_result()
{
    let mut h = Harness::new(true, false);
    let ticket = h.requests.insert(()).unwrap();
    let mut input = Harness::bytes(Arguments::default());
    assert_eq!(h.step(ticket, Phase::Start, Operation::Exports, &input, &mut [0; 4]).unwrap(),
               Completion::Pending);
    input.fill(0xff); // The module copied START; it cannot retain this slice.
    let foreign = h.requests.insert(()).unwrap();
    assert_eq!(h.step(foreign, Phase::Poll, Operation::Exports, &[], &mut [0; 4]),
               Err(Error(mac_abi::NOT_FOUND)));
    assert_eq!(h.step(foreign,
                      Phase::Start,
                      Operation::Exports,
                      &Harness::bytes(Arguments::default()),
                      &mut [0; 4]),
               Err(Error(mac_abi::SERVICE_BUSY)));
    for _ in 0..4
    {
        assert_eq!(h.step(ticket, Phase::Poll, Operation::Exports, &[], &mut []).unwrap(),
                   Completion::Pending);
    }
    assert_eq!(h.finish(ticket, Phase::Cancel, Operation::Exports, &[]).unwrap(),
               1u32.to_le_bytes());
    assert_eq!(h.step(ticket, Phase::Poll, Operation::Exports, &[], &mut [0; 4]),
               Err(Error(mac_abi::NOT_FOUND)));
    // A CANCEL against an already-ready reply cannot poison the next START.
    assert_eq!(h.execute(Operation::Exports, Arguments::default()).unwrap(), 1u32.to_le_bytes());
    h.service.quiesce().unwrap();
}

#[test]
fn invalid_start_capacity_and_read_only_mutations_fail_before_host_side_effects()
{
    let mut h = Harness::new(false, false);
    let volume = h.volumes.insert(()).unwrap();
    let ticket = h.requests.insert(()).unwrap();
    let input = Harness::bytes(Arguments { volume,
                                           ..Default::default() });
    assert_eq!(h.step(ticket, Phase::Start, Operation::Mount, &input, &mut []),
               Err(Error(mac_abi::BAD_ARGUMENT)));
    assert!(h.trace.lock().unwrap().is_empty());
    h.execute(Operation::Mount,
              Arguments { volume,
                          ..Default::default() })
     .unwrap();
    let before = h.trace.lock().unwrap().len();
    let token = h.writes.insert(()).unwrap();
    assert_eq!(h.execute(Operation::BeginCreate,
                         Arguments { volume,
                                     parent: 2,
                                     token,
                                     payload: b"Forbidden",
                                     ..Default::default() }),
               Err(Error(mac_abi::READ_ONLY)));
    assert_eq!(h.trace.lock().unwrap().len(), before);
    h.execute(Operation::Unmount,
              Arguments { volume,
                          ..Default::default() })
     .unwrap();
    h.service.quiesce().unwrap();
}
