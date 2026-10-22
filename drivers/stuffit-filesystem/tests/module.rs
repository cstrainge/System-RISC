//! Split callbacks over a real classic archive. Resident guest tests separately
//! prove the actual module loader, lease broker and public Toolbox boundary.
use std::sync::{Arc, Mutex};

use mac_driver::{ClassId, Completion, Error, Phase, Request, Result};
use mac_filesystem_interface::{self as fs, Arguments, Metadata, Operation};
use mac_object_table::{Kind, Table};
use stuffit_filesystem::{Sources, service::Service};
const NESTED: &[u8] = include_bytes!("../../../crates/mac-sit/tests/fixtures/nested-v1.sit");
const FORKS: &[u8] = include_bytes!("../../../crates/mac-sit/tests/fixtures/forks-v1.sit");
struct Lower
{
    sources: Table<Vec<u8>>,
    tickets: Table<()>,
    flight:  Option<(u32, u64, usize)>,
    trace:   Vec<Phase>,
    cancels: usize
}
#[derive(Clone)]
struct Memory(Arc<Mutex<Lower>>);
impl Sources for Memory
{
    fn length(&self, source: u32) -> Result<u64>
    {
        self.0
            .lock()
            .unwrap()
            .sources
            .get(source)
            .map(|v| v.len() as u64)
            .ok_or(Error(mac_abi::NOT_FOUND))
    }

    fn read(&mut self, source: u32, offset: u64, ticket: &mut u32, phase: Phase, out: &mut [u8])
            -> Result<Completion>
    {
        let mut state = self.0.lock().unwrap();
        state.trace.push(phase);
        if phase == Phase::Start
        {
            assert_eq!(*ticket, 0);
            assert!(state.flight.is_none());
            assert!(state.sources.get(source).is_some());
            *ticket = state.tickets.insert(()).unwrap();
            state.flight = Some((source, offset, out.len()));
            return Ok(Completion::Pending);
        }
        assert!(state.tickets.get(*ticket).is_some());
        assert_eq!(state.flight, Some((source, offset, out.len())));
        if phase == Phase::Cancel
        {
            state.cancels += 1;
            if state.cancels % 2 == 1
            {
                return Ok(Completion::Pending);
            }
            state.flight = None;
            state.tickets.remove(*ticket).unwrap();
            *ticket = 0;
            return Err(Error(mac_abi::OPERATION_CANCELLED));
        }
        let data = state.sources.get(source).unwrap();
        let at = offset as usize;
        let count = out.len().min(37).min(data.len().saturating_sub(at));
        out[..count].copy_from_slice(&data[at..at + count]);
        state.flight = None;
        state.tickets.remove(*ticket).unwrap();
        *ticket = 0;
        Ok(Completion::Complete(count))
    }
}
struct Harness
{
    service:  Service<Memory>,
    lower:    Memory,
    source:   u32,
    requests: Table<()>,
    volumes:  Table<()>,
    files:    Table<()>
}
fn metadata() -> Vec<u8>
{
    let mut out = vec![0; fs::MAX_METADATA_BYTES];
    let n = Metadata { id:              20,
                       parent:          2,
                       directory:       false,
                       data_length:     NESTED.len() as u64,
                       resource_length: 0,
                       created:         3_875_171_696_000_000,
                       modified:        3_875_171_700_000_000,
                       backup:          None,
                       child_count:     0,
                       attributes:      0,
                       finder_info:     [0; 32],
                       revision:        1,
                       name:            "Examples.sit",
                       comment:         "" }.encode(&mut out)
                                            .unwrap();
    out.truncate(n);
    out
}
impl Harness
{
    fn new(bytes: &[u8]) -> Self
    {
        let mut sources = Table::new(Kind::FileSources);
        let source = sources.insert(bytes.to_vec()).unwrap();
        let lower = Memory(Arc::new(Mutex::new(Lower { sources,
                                                       tickets:
                                                           Table::new(Kind::DriverRequests),
                                                       flight: None,
                                                       trace: Vec::new(),
                                                       cancels: 0 })));
        let service = Service::new();
        service.initialize(lower.clone()).unwrap();
        Self { service,
               lower,
               source,
               requests: Table::new(Kind::DriverRequests),
               volumes: Table::new(Kind::Volumes),
               files: Table::new(Kind::Files) }
    }

    fn bytes(args: Arguments<'_>) -> Vec<u8>
    {
        let mut bytes = vec![0; fs::HEADER_BYTES + args.payload.len()];
        args.encode(&mut bytes).unwrap();
        bytes
    }

    fn step(&self, ticket: u32, op: Operation, phase: Phase, input: &[u8], out: &mut [u8])
            -> Result<Completion>
    {
        let before = self.lower.0.lock().unwrap().trace.len();
        let result = self.service.request(Request { ticket,
                                                    class: ClassId(fs::CLASS),
                                                    operation: op as u32,
                                                    phase,
                                                    input },
                                          out);
        let after = self.lower.0.lock().unwrap().trace.len();
        assert!(after - before <= 1, "more than one lower read in a callback");
        result
    }

    fn finish(&self, ticket: u32, op: Operation, phase: Phase, input: &[u8]) -> Result<Vec<u8>>
    {
        for i in 0..100_000
        {
            let mut out = vec![0xa5; fs::MAX_REPLY];
            let phase = if i == 0 || phase == Phase::Cancel { phase } else { Phase::Poll };
            match self.step(ticket, op, phase, if i == 0 { input } else { &[] }, &mut out)?
            {
                Completion::Pending => (),
                Completion::Complete(n) =>
                {
                    out.truncate(n);
                    return Ok(out);
                }
            }
        }
        panic!("operation did not finish within bounded fixture workload")
    }

    fn execute(&mut self, op: Operation, args: Arguments<'_>) -> Result<Vec<u8>>
    {
        let ticket = self.requests.insert(()).unwrap();
        let result = self.finish(ticket, op, Phase::Start, &Self::bytes(args));
        self.requests.remove(ticket).unwrap();
        result
    }

    fn mount(&mut self) -> u32
    {
        let volume = self.volumes.insert(()).unwrap();
        let out = self.execute(Operation::Mount,
                               Arguments { volume,
                                           auxiliary: self.source.into(),
                                           payload: &metadata(),
                                           ..Default::default() })
                      .unwrap();
        let descriptor = fs::Mount::decode(&out).unwrap();
        assert_eq!(descriptor.filesystem, *b"SIT ");
        assert_eq!(descriptor.filesystem_version, 1);
        assert_eq!(descriptor.capabilities, fs::READ);
        assert_eq!(descriptor.root.name, "Examples.sit");
        volume
    }

    fn open(&mut self, volume: u32, node: u32) -> u32
    {
        let token = self.files.insert(()).unwrap();
        self.execute(Operation::Open,
                     Arguments { volume,
                                 node,
                                 token,
                                 ..Default::default() })
            .unwrap();
        token
    }

    fn read(&mut self, volume: u32, token: u32, flags: u32, offset: u64, length: u64)
            -> Result<Vec<u8>>
    {
        self.execute(Operation::Read,
                     Arguments { volume,
                                 token,
                                 flags,
                                 offset,
                                 length,
                                 ..Default::default() })
    }
}

#[test]
fn nested_catalog_uses_cached_names_and_no_payload_reads_after_mount()
{
    let mut h = Harness::new(NESTED);
    let volume = h.mount();
    let before = h.lower.0.lock().unwrap().trace.len();
    let out = h.execute(Operation::Lookup,
                        Arguments { volume,
                                    parent: 16,
                                    payload: "CAFE\u{301}".as_bytes(),
                                    ..Default::default() })
               .unwrap();
    let m = Metadata::decode(&out).unwrap();
    assert_eq!(m.name, "Café");
    assert_eq!(m.id, 19);
    assert_eq!(&m.finder_info[..8], b"TEXTTEST");
    assert!(m.modified > 3_000_000_000_000_000);
    for (parent, expected) in [(2, &[16][..]), (16, &[17, 19, 20][..]), (17, &[18][..])]
    {
        for (offset, id) in expected.iter().enumerate()
        {
            let out = h.execute(Operation::Scan,
                                Arguments { volume,
                                            parent,
                                            offset: offset as u64,
                                            ..Default::default() })
                       .unwrap();
            assert_eq!(&out[..4], &fs::SCAN_ENTRY.to_le_bytes());
            assert_eq!(Metadata::decode(&out[4..]).unwrap().id, *id);
        }
        assert_eq!(h.execute(Operation::Scan,
                             Arguments { volume,
                                         parent,
                                         offset: expected.len() as u64,
                                         ..Default::default() })
                    .unwrap(),
                   fs::SCAN_DONE.to_le_bytes());
    }
    assert_eq!(h.lower.0.lock().unwrap().trace.len(),
               before,
               "catalog reads touched fork payloads");
}

#[test]
fn positional_reads_cross_chunks_seek_back_and_verify_crc_on_exact_end()
{
    let mut h = Harness::new(NESTED);
    let volume = h.mount();
    let token = h.open(volume, 18);
    assert_eq!(h.read(volume, token, 0, 0, 4096).unwrap(),
               (0..4096).map(|i| i as u8).collect::<Vec<_>>());
    assert_eq!(h.read(volume, token, 0, 4096, 4096).unwrap(),
               (0..4096).map(|i| i as u8).collect::<Vec<_>>());
    assert_eq!(h.read(volume, token, 0, 7, 23).unwrap(),
               (7..30).map(|i| i as u8).collect::<Vec<_>>());
    assert_eq!(h.read(volume, token, 0, 8187, 64).unwrap(), vec![251, 252, 253, 254, 255]);
    assert!(h.read(volume, token, 0, 8192, 1).unwrap().is_empty());
    assert!(h.read(volume, token, 1, 0, 1).unwrap().is_empty());
    assert_eq!(h.read(volume, token, 0, 8193, 1), Err(Error(mac_abi::BAD_ARGUMENT)));
    assert_eq!(h.execute(Operation::Unmount,
                         Arguments { volume,
                                     ..Default::default() }),
               Err(Error(mac_abi::SERVICE_BUSY)));
    h.execute(Operation::Close,
              Arguments { volume,
                          token,
                          ..Default::default() })
     .unwrap();
    h.execute(Operation::Unmount,
              Arguments { volume,
                          ..Default::default() })
     .unwrap();
    assert_eq!(h.service.quiesce(), Ok(()));
}

#[test]
fn nonempty_resource_and_data_forks_survive_independent_readers()
{
    let mut h = Harness::new(FORKS);
    let volume = h.mount();
    let a = h.open(volume, 16);
    let b = h.open(volume, 16);
    assert_eq!(h.read(volume, a, 1, 0, 2048).unwrap(),
               (0..2048).map(|i| i as u8).collect::<Vec<_>>());
    assert_eq!(h.read(volume, b, 0, 0, 10).unwrap(), b"Data fork\r");
    assert_eq!(h.read(volume, a, 0, 0, 10).unwrap(), b"Data fork\r");
    assert_eq!(h.read(volume, b, 1, 1020, 9).unwrap(), vec![252, 253, 254, 255, 0, 1, 2, 3, 4]);
}

#[test]
fn corrupted_payload_fails_on_the_last_full_read_and_readonly_mutations_are_rejected()
{
    let mut bytes = FORKS.to_vec();
    bytes[134] ^= 1;
    let mut h = Harness::new(&bytes);
    let volume = h.mount();
    let token = h.open(volume, 16);
    assert_eq!(h.read(volume, token, 1, 0, 2048),
               Err(Error(mac_abi::archive::ArchiveError::ForkChecksum.status())));
    assert_eq!(h.read(volume, token, 1, 0, 2048),
               Err(Error(mac_abi::archive::ArchiveError::ForkChecksum.status())));
    for op in [Operation::BeginCreate,
               Operation::BeginReplace,
               Operation::WriteChunk,
               Operation::FinishWrite,
               Operation::AbortWrite,
               Operation::Rename,
               Operation::Move,
               Operation::MakeDirectory,
               Operation::Remove,
               Operation::SetMetadata]
    {
        assert_eq!(h.execute(op,
                             Arguments { volume,
                                         ..Default::default() }),
                   Err(Error(mac_abi::READ_ONLY)));
    }
}

#[test]
fn cancellation_drains_the_live_lower_ticket_before_releasing_state()
{
    let mut h = Harness::new(NESTED);
    let volume = h.volumes.insert(()).unwrap();
    let ticket = h.requests.insert(()).unwrap();
    let input = Harness::bytes(Arguments { volume,
                                           auxiliary: h.source.into(),
                                           payload: &metadata(),
                                           ..Default::default() });
    for i in 0..30
    {
        assert_eq!(h.step(ticket,
                          Operation::Mount,
                          if i == 0 { Phase::Start } else { Phase::Poll },
                          if i == 0 { &input } else { &[] },
                          &mut vec![0; fs::MAX_REPLY]),
                   Ok(Completion::Pending));
        if h.lower.0.lock().unwrap().flight.is_some()
        {
            break;
        }
    }
    assert!(h.lower.0.lock().unwrap().flight.is_some());
    assert_eq!(h.step(ticket, Operation::Mount, Phase::Cancel, &[], &mut vec![0; fs::MAX_REPLY]),
               Ok(Completion::Pending));
    assert!(h.lower.0.lock().unwrap().flight.is_some());
    assert_eq!(h.finish(ticket, Operation::Mount, Phase::Cancel, &[]),
               Err(Error(mac_abi::OPERATION_CANCELLED)));
    assert!(h.lower.0.lock().unwrap().flight.is_none());
    assert_eq!(h.execute(Operation::Catalog,
                         Arguments { volume,
                                     node: 2,
                                     ..Default::default() }),
               Err(Error(mac_abi::NOT_FOUND)));
    h.requests.remove(ticket).unwrap();
    let volume = h.mount();
    let token = h.open(volume, 18);
    let ticket = h.requests.insert(()).unwrap();
    let input = Harness::bytes(Arguments { volume,
                                           token,
                                           length: 100,
                                           ..Default::default() });
    for i in 0..30
    {
        assert_eq!(h.step(ticket,
                          Operation::Read,
                          if i == 0 { Phase::Start } else { Phase::Poll },
                          if i == 0 { &input } else { &[] },
                          &mut vec![0; fs::MAX_REPLY]),
                   Ok(Completion::Pending));
        if h.lower.0.lock().unwrap().flight.is_some()
        {
            break;
        }
    }
    assert_eq!(h.finish(ticket, Operation::Read, Phase::Cancel, &[]),
               Err(Error(mac_abi::OPERATION_CANCELLED)));
    h.requests.remove(ticket).unwrap();
    assert_eq!(h.read(volume, token, 0, 0, 100).unwrap(),
               (0..100).map(|i| i as u8).collect::<Vec<_>>());
}

#[test]
fn malformed_polls_and_small_buffers_preserve_the_exact_live_operation()
{
    let mut h = Harness::new(NESTED);
    let volume = h.mount();
    let token = h.open(volume, 19);
    let ticket = h.requests.insert(()).unwrap();
    let input = Harness::bytes(Arguments { volume,
                                           token,
                                           length: 21,
                                           ..Default::default() });
    assert_eq!(h.step(ticket, Operation::Read, Phase::Start, &input, &mut [0; 20]),
               Err(Error(mac_abi::BAD_ARGUMENT)));
    assert_eq!(h.step(ticket, Operation::Read, Phase::Start, &input, &mut [0; 21]),
               Ok(Completion::Pending));
    let foreign = h.requests.insert(()).unwrap();
    assert_eq!(h.step(foreign, Operation::Read, Phase::Poll, &[], &mut [0; 21]),
               Err(Error(mac_abi::NOT_FOUND)));
    assert_eq!(h.step(ticket, Operation::Read, Phase::Poll, &[1], &mut [0; 21]),
               Err(Error(mac_abi::BAD_ARGUMENT)));
    // Reach terminal data with no room to return it. A subsequent CANCEL must
    // return the actual acknowledged read, not erase it or recreate the job.
    for _ in 0..1000
    {
        assert_eq!(h.step(ticket, Operation::Read, Phase::Poll, &[], &mut []),
                   Ok(Completion::Pending));
    }
    let before = h.lower.0.lock().unwrap().trace.len();
    assert_eq!(h.finish(ticket, Operation::Read, Phase::Cancel, &[]).unwrap(),
               b"Native MacRoman name\r");
    assert_eq!(h.lower.0.lock().unwrap().trace.len(), before);
}
