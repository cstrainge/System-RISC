mod common;
#[path = "../../../crates/mac-object-table/tests/support/native_requests.rs"]
mod native_requests;
use std::{future::Future,
          path::PathBuf,
          pin::pin,
          sync::{Arc,
                 atomic::{AtomicBool, AtomicU64, Ordering}},
          task::{Context, Poll, Waker}};

use mac_fork_file::{Fork, Kind};
use mac_host_protocol::{Arguments, Operation};
use mac_object_table::{Kind as ObjectKind, Table};
use native_requests::Requests as NativeRequests;
use risc_forks::{Volume, io::Slice};
use risc_host::{Export, Session};
use vhost_filesystem::{Cause, Filesystem, Reply, Result, Rpc};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp
{
    fn new() -> Self
    {
        let path = std::env::temp_dir().join(format!("host-fs-{}-{}",
                                                     std::process::id(),
                                                     NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp
{
    fn drop(&mut self)
    {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn run<T>(future: impl Future<Output = T>) -> T
{
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    for _ in 0..100_000
    {
        if let Poll::Ready(result) = future.as_mut().poll(&mut context)
        {
            return result;
        }
    }
    panic!("filesystem future failed to finish its bounded test operation")
}
struct Host
{
    session:                   Session,
    tickets:                   NativeRequests,
    calls:                     Vec<Operation>,
    trace:                     Vec<(Operation, u32, u64, usize)>,
    commits:                   Vec<Vec<u8>>,
    fail_next:                 Option<Operation>,
    fail_resource_read_after:  Option<usize>,
    reached_child:             Arc<AtomicBool>,
    modify_before_description: Option<PathBuf>
}
impl Host
{
    fn new(temp: &Temp, writable: bool) -> Self
    {
        Self {
            session: Session::new(Arc::new(vec![Export::open("Share", &temp.0, writable).unwrap()])).unwrap(),
            tickets: NativeRequests::new(),
            calls: Vec::new(), trace: Vec::new(), commits: Vec::new(), fail_next: None, fail_resource_read_after: None,
            reached_child: Arc::new(AtomicBool::new(false)),
            modify_before_description: None
        }
    }
}
impl Rpc for Host
{
    async fn invoke(&mut self, operation: Operation, arguments: Arguments<'_>) -> Result<Reply>
    {
        // Simulate a real split callback on every operation. All wire correlation
        // IDs come from the same shared facility as the resident broker's.
        let mut yielded = false;
        std::future::poll_fn(|cx| {
            if yielded
            {
                Poll::Ready(())
            }
            else
            {
                yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }).await;
        if operation == Operation::Describe
           && let Some(path) = self.modify_before_description.take()
        {
            let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
            std::fs::write(&path, b"new").unwrap();
            std::fs::OpenOptions::new().write(true)
                                       .open(&path)
                                       .unwrap()
                                       .set_times(std::fs::FileTimes::new().set_modified(modified))
                                       .unwrap();
        }
        self.calls.push(operation);
        self.trace.push((operation, arguments.object, arguments.offset, arguments.payload.len()));
        if operation == Operation::Read
           && arguments.offset >= mac_fork_file::METADATA_BYTES as u64
           && let Some(remaining) = &mut self.fail_resource_read_after
        {
            if *remaining == 0
            {
                self.fail_resource_read_after = None;
                return Err(Cause::Host(mac_host_protocol::status::IO).into());
            }
            *remaining -= 1;
        }
        if self.fail_next == Some(operation)
        {
            self.fail_next = None;
            return Err(Cause::Host(mac_host_protocol::status::IO).into());
        }
        if operation == Operation::Commit
        {
            self.commits.push(arguments.payload.to_vec());
        }
        let ticket = self.tickets.insert(()).unwrap();
        let header = arguments.header(operation, ticket).unwrap();
        let (header, body) = self.session.respond(header, arguments.payload);
        if operation == Operation::ListNext
           && (!body.starts_with(b".system-risc-") && !body.is_empty())
        {
            self.reached_child.store(true, Ordering::Relaxed);
        }
        self.tickets.remove(ticket);
        Ok(Reply { header,
                   body })
    }
}
fn seeded() -> (Temp, u32, Vec<u8>, Vec<u8>)
{
    let temp = Temp::new();
    let volume = Volume::initialize(&temp.0, "External Software").unwrap();
    volume.create("/", "Tools", Kind::Directory, &mut Slice(&[]), &mut Slice(&[]), None).unwrap();
    let data: Vec<_> = (0..10_017).map(|i| (i * 31) as u8).collect();
    let resource: Vec<_> = (0..6_009).map(|i| (i * 13) as u8).collect();
    let file = volume.create("/Tools",
                             "Cafe\u{301}",
                             Kind::File,
                             &mut Slice(&data),
                             &mut Slice(&resource),
                             None)
                     .unwrap();
    (temp, file.metadata.catalog_id, data, resource)
}
#[test]
fn actual_host_containers_support_nested_unicode_lookup_both_forks_and_stale_open_tokens()
{
    let (temp, cnid, data, resource) = seeded();
    let mut filesystem = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    let info = run(filesystem.mount(volume, 0)).unwrap();
    assert_eq!(info.metadata.name, "External Software");
    assert!(!info.read_only);
    let directory = run(filesystem.lookup(volume, 2, "TOOLS")).unwrap().metadata.catalog_id;
    assert_eq!(run(filesystem.lookup(volume, directory, "CAFÉ")).unwrap().metadata.catalog_id,
               cnid);
    assert_eq!(run(filesystem.lookup(volume, directory, "Cafe\u{301}")).unwrap().metadata.name,
               "Café");
    let listings =
        filesystem.transport().calls.iter().filter(|&&op| op == Operation::ListNext).count();
    run(filesystem.lookup(volume, directory, "café")).unwrap();
    assert_eq!(filesystem.transport()
                         .calls
                         .iter()
                         .filter(|&&op| op == Operation::ListNext)
                         .count(),
               listings,
               "cached lookup re-enumerated a folder");
    let mut files = Table::new(ObjectKind::Files);
    let first = files.insert(()).unwrap();
    let layout = run(filesystem.open(volume, cnid, u64::from(first))).unwrap();
    assert_eq!(layout.data.length, data.len() as u64);
    assert_eq!(layout.resources.length, resource.len() as u64);
    assert_eq!(run(filesystem.read(volume, u64::from(first), Fork::Data, 4095, 5000)).unwrap(),
               data[4095..9095]);
    assert_eq!(run(filesystem.read(volume, u64::from(first), Fork::Resource, 0, 65536)).unwrap(),
               resource);
    assert_eq!(run(filesystem.unmount(volume)).unwrap_err().cause, Cause::Busy);
    run(filesystem.close_file(volume, u64::from(first))).unwrap();
    files.remove(first);
    let next = files.insert(()).unwrap();
    assert_eq!(first & 0xffff, next & 0xffff);
    assert_ne!(first, next);
    run(filesystem.open(volume, cnid, u64::from(next))).unwrap();
    assert_eq!(run(filesystem.read(volume, u64::from(first), Fork::Data, 0, 1)).unwrap_err().cause,
               Cause::NotFound);
    assert_eq!(run(filesystem.close_file(volume, u64::from(first))).unwrap_err().cause,
               Cause::NotFound);
    assert_eq!(run(filesystem.read(volume.wrapping_add(1), u64::from(next), Fork::Data, 0, 1)).unwrap_err()
                                                                                   .cause,
               Cause::NotFound);
    run(filesystem.close_file(volume, u64::from(next))).unwrap();
    run(filesystem.unmount(volume)).unwrap();
    assert_eq!(filesystem.info(volume).unwrap_err().cause, Cause::NotFound);
}
#[test]
fn corrupt_container_fork_is_rejected_while_plain_host_data_is_not_a_container()
{
    let (temp, cnid, _, _) = seeded();
    let volume = Volume::open(&temp.0).unwrap();
    let entry = volume.resolve("/Tools/Café").unwrap();
    let path = temp.0.join(&entry.path);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[mac_fork_file::METADATA_BYTES] ^= 1;
    std::fs::write(path, bytes).unwrap();
    let mut filesystem = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let id = volumes.insert(()).unwrap();
    run(filesystem.mount(id, 0)).unwrap();
    let dir = run(filesystem.lookup(id, 2, "Tools")).unwrap().metadata.catalog_id;
    run(filesystem.lookup(id, dir, "café")).unwrap();
    let mut files = Table::new(ObjectKind::Files);
    let token = files.insert(()).unwrap();
    run(filesystem.open(id, cnid, u64::from(token))).unwrap();
    assert_eq!(run(filesystem.read(id, u64::from(token), Fork::Data, 0, 64)).unwrap_err().cause,
               Cause::Format(mac_fork_file::Error::Corrupt));
    run(filesystem.close_file(id, u64::from(token))).unwrap();
    run(filesystem.unmount(id)).unwrap();
    std::fs::write(temp.0.join("unwrapped.txt"), b"ordinary host data").unwrap();
    run(filesystem.mount(id, 0)).unwrap();
    run(filesystem.scan(id, 2)).unwrap();
    let plain = run(filesystem.lookup(id, 2, "unwrapped.txt")).unwrap().metadata.catalog_id;
    run(filesystem.open(id, plain, u64::from(token))).unwrap();
    assert_eq!(run(filesystem.read(id, u64::from(token), Fork::Data, 0, 64)).unwrap(),
               b"ordinary host data");
    assert!(run(filesystem.read(id, u64::from(token), Fork::Resource, 0, 64)).unwrap().is_empty());
    run(filesystem.close_file(id, u64::from(token))).unwrap();
    assert_eq!(std::fs::read(temp.0.join("unwrapped.txt")).unwrap(), b"ordinary host data");
    run(filesystem.unmount(id)).unwrap();
}

#[test]
fn raw_host_folders_and_files_are_readable_without_records_on_writable_and_readonly_exports()
{
    for writable in [true, false]
    {
        let (temp, _, _, _) = seeded();
        std::fs::create_dir(temp.0.join("Test")).unwrap();
        std::fs::write(temp.0.join("Test/Plain.txt"), b"plain file, no metadata").unwrap();
        std::fs::write(temp.0.join("Empty.txt"), b"").unwrap();
        let mut fs = Filesystem::new(Host::new(&temp, writable));
        let mut volumes = Table::new(ObjectKind::Volumes);
        let volume = volumes.insert(()).unwrap();
        run(fs.mount(volume, 0)).unwrap();
        let folder = run(fs.lookup(volume, 2, "TEST")).unwrap().clone();
        assert_eq!(folder.metadata.kind, Kind::Directory);
        let file = run(fs.lookup(volume, folder.metadata.catalog_id, "plain.TXT")).unwrap().clone();
        assert_eq!(file.layout.data.length, 23);
        assert_eq!(file.layout.resources.length, 0);
        assert_eq!(file.metadata.finder_info, [0; 32]);
        assert!(file.metadata.modified > 3_800_000_000_000_000);
        let mut files = Table::new(ObjectKind::Files);
        let token = files.insert(()).unwrap();
        run(fs.open(volume, file.metadata.catalog_id, u64::from(token))).unwrap();
        assert_eq!(run(fs.read(volume, u64::from(token), Fork::Data, 6, 17)).unwrap(),
                   b"file, no metadata");
        assert!(run(fs.read(volume, u64::from(token), Fork::Resource, 0, 100)).unwrap().is_empty());
        run(fs.close_file(volume, u64::from(token))).unwrap();
        // A refresh and rename preserve identity; replacement cannot alias it.
        std::fs::rename(temp.0.join("Test/Plain.txt"), temp.0.join("Test/Renamed.txt")).unwrap();
        run(fs.refresh(volume)).unwrap();
        let again = run(fs.lookup(volume, 2, "Test")).unwrap().metadata.catalog_id;
        assert_eq!(again, folder.metadata.catalog_id);
        assert_eq!(run(fs.lookup(volume, again, "Renamed.txt")).unwrap().metadata.catalog_id,
                   file.metadata.catalog_id);
        std::fs::remove_file(temp.0.join("Test/Renamed.txt")).unwrap();
        std::fs::write(temp.0.join("Test/Renamed.txt"), b"replacement").unwrap();
        run(fs.refresh(volume)).unwrap();
        run(fs.lookup(volume, 2, "Test")).unwrap();
        let replacement = run(fs.lookup(volume, again, "Renamed.txt")).unwrap().metadata.catalog_id;
        assert_ne!(replacement, file.metadata.catalog_id);
        assert_eq!(run(fs.open(volume, file.metadata.catalog_id, u64::from(token))).unwrap_err()
                                                                                   .cause,
                   Cause::NotFound);
        run(fs.unmount(volume)).unwrap();
        assert!(!temp.0.join("Test/.system-risc-directory").exists());
        assert_eq!(std::fs::read(temp.0.join("Test/Renamed.txt")).unwrap(), b"replacement");
    }
}
#[test]
fn read_only_mounts_preserve_the_same_metadata_and_failed_export_is_not_published()
{
    let (temp, _, _, _) = seeded();
    let mut filesystem = Filesystem::new(Host::new(&temp, false));
    let mut ids = Table::new(ObjectKind::Volumes);
    let id = ids.insert(()).unwrap();
    assert_eq!(run(filesystem.mount(id, 9)).unwrap_err().cause, Cause::NotFound);
    assert!(filesystem.info(id).is_err());
    assert!(run(filesystem.mount(id, 0)).unwrap().read_only);
    assert_eq!(run(filesystem.lookup(id, 2, "Tools")).unwrap().metadata.kind, Kind::Directory);
    run(filesystem.unmount(id)).unwrap();
}

#[test]
fn native_writer_creates_both_forks_replaces_atomically_and_keeps_unrelated_directory_cache()
{
    use vhost_filesystem::{Create, Replace};
    let (temp, _, _, _) = seeded();
    let mut filesystem = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(filesystem.mount(volume, 0)).unwrap();
    let parent = run(filesystem.lookup(volume, 2, "Tools")).unwrap().metadata.catalog_id;
    run(filesystem.scan(volume, parent)).unwrap();
    let mut writes = common::Tokens::new();
    let token = writes.insert(()).unwrap();
    let data = vec![0x53; 9001];
    let resource = vec![0xa7; 4097];
    let when = 3_875_171_696_123_456;
    let id = run(filesystem.begin_create(Create { volume,
                                                  parent,
                                                  token,
                                                  properties: mac_filesystem_interface::FileProperties::fresh("New Script", when),
                                                  data_length: data.len() as u64,
                                                  resource_length: resource.len() as u64,
                                                  when })).unwrap();
    assert_eq!(run(filesystem.finish_write(volume, token)).unwrap_err().cause,
               Cause::Format(mac_fork_file::Error::Incomplete));
    for bytes in data.chunks(mac_filesystem_interface::MAX_TRANSFER)
    {
        run(filesystem.write_chunk(volume, token, Fork::Data, bytes)).unwrap();
    }
    for bytes in resource.chunks(mac_fork_file::CHUNK_BYTES)
    {
        run(filesystem.write_chunk(volume, token, Fork::Resource, bytes)).unwrap();
    }
    assert_eq!(run(filesystem.finish_write(volume, token)).unwrap(), id);
    assert_eq!(run(filesystem.finish_write(volume, token)).unwrap_err().cause, Cause::NotFound);
    let listings =
        filesystem.transport().calls.iter().filter(|&&op| op == Operation::ListNext).count();
    assert_eq!(run(filesystem.lookup(volume, parent, "NEW SCRIPT")).unwrap().metadata.catalog_id,
               id);
    assert_eq!(run(filesystem.lookup(volume, 2, "Tools")).unwrap().metadata.catalog_id, parent);
    assert_eq!(filesystem.transport()
                         .calls
                         .iter()
                         .filter(|&&op| op == Operation::ListNext)
                         .count(),
               listings,
               "own mutation re-enumerated cached folders");
    let physical = Volume::open(&temp.0).unwrap();
    let entry = physical.resolve("/Tools/New Script").unwrap();
    assert_eq!(entry.metadata.created, when);
    assert_eq!(entry.layout.data.length, data.len() as u64);
    assert_eq!(entry.layout.resources.length, resource.len() as u64);
    writes.remove(token);
    let replacement = writes.insert(()).unwrap();
    assert_ne!(replacement, token);
    run(filesystem.begin_replace(Replace { volume,
                                           node: id,
                                           token: replacement,
                                           data_length: 3,
                                           resource_length: 2,
                                           preserve_resources: false,
                                           properties: None,
                                           when: when + 1 })).unwrap();
    run(filesystem.write_chunk(volume, replacement, Fork::Data, b"new")).unwrap();
    run(filesystem.write_chunk(volume, replacement, Fork::Resource, b"rs")).unwrap();
    assert_eq!(run(filesystem.finish_write(volume, replacement)).unwrap(), id);
    assert_eq!(physical.resolve("/Tools/New Script").unwrap().metadata.catalog_id, id);
    let mut opens = Table::new(ObjectKind::Files);
    let open = opens.insert(()).unwrap();
    run(filesystem.open(volume, id, u64::from(open))).unwrap();
    assert_eq!(run(filesystem.read(volume, u64::from(open), Fork::Data, 0, 30)).unwrap(), b"new");
    assert_eq!(run(filesystem.read(volume, u64::from(open), Fork::Resource, 0, 30)).unwrap(),
               b"rs");
    run(filesystem.close_file(volume, u64::from(open))).unwrap();
    run(filesystem.unmount(volume)).unwrap();
    assert_eq!(physical.check().unwrap(), 4);
}

#[test]
fn aborted_create_removes_stage_preserves_catalog_and_never_reuses_reserved_identity()
{
    use vhost_filesystem::Create;
    let (temp, _, _, _) = seeded();
    let mut filesystem = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(filesystem.mount(volume, 0)).unwrap();
    let parent = run(filesystem.lookup(volume, 2, "Tools")).unwrap().metadata.catalog_id;
    let root_modified = filesystem.info(volume).unwrap().metadata.modified;
    let parent_modified = filesystem.cached_node(volume, parent).unwrap().metadata.modified;
    let mut writes = common::Tokens::new();
    let first = writes.insert(()).unwrap();
    let reserved = run(filesystem.begin_create(Create { volume,
                                                        parent,
                                                        token: first,
                                                        properties: mac_filesystem_interface::FileProperties::fresh("Cancelled", 10),
                                                        data_length: 20,
                                                        resource_length: 0,
                                                        when: 10 })).unwrap();
    assert_eq!(run(filesystem.unmount(volume)).unwrap_err().cause, Cause::Busy);
    assert!(!run(filesystem.abort_write(volume, first)).unwrap());
    assert_eq!(filesystem.info(volume).unwrap().metadata.modified, root_modified);
    assert_eq!(filesystem.cached_node(volume, parent).unwrap().metadata.modified, parent_modified);
    assert_eq!(Volume::open(&temp.0).unwrap().resolve("/").unwrap().metadata.modified,
               root_modified);
    writes.remove(first);
    assert_eq!(run(filesystem.lookup(volume, parent, "Cancelled")).unwrap_err().cause,
               Cause::NotFound);
    let next = writes.insert(()).unwrap();
    assert_eq!(run(filesystem.write_chunk(volume, first, Fork::Data, b"stale")).unwrap_err().cause,
               Cause::NotFound);
    let fresh = run(filesystem.begin_create(Create { volume,
                                                     parent,
                                                     token: next,
                                                     properties: mac_filesystem_interface::FileProperties::fresh("Cancelled", 11),
                                                     data_length: 0,
                                                     resource_length: 0,
                                                     when: 11 })).unwrap();
    assert!(fresh > reserved);
    assert_eq!(run(filesystem.finish_write(volume, next)).unwrap(), fresh);
    run(filesystem.unmount(volume)).unwrap();
    let physical = Volume::open(&temp.0).unwrap();
    assert_eq!(physical.resolve("/Tools/Cancelled").unwrap().metadata.catalog_id, fresh);
    physical.check().unwrap();
    for entry in std::fs::read_dir(temp.0
                                       .join(physical.resolve("/Tools").unwrap().path)
                                       .parent()
                                       .unwrap()).unwrap()
    {
        assert!(!entry.unwrap().file_name().to_string_lossy().starts_with(".system-risc-stage-"));
    }
}


#[test]
fn failed_flush_retries_durability_without_rewriting_the_acknowledged_header()
{
    use vhost_filesystem::Create;
    let (temp, _, _, _) = seeded();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let parent = run(fs.lookup(volume, 2, "Tools")).unwrap().metadata.catalog_id;
    let mut writes = common::Tokens::new();
    let token = writes.insert(()).unwrap();
    let id = run(fs.begin_create(Create { volume,
                                          parent,
                                          token,
                                          properties: mac_filesystem_interface::FileProperties::fresh("Flush Retry", 20),
                                          data_length: 3,
                                          resource_length: 0,
                                          when: 20 })).unwrap();
    run(fs.write_chunk(volume, token, Fork::Data, b"abc")).unwrap();
    fs.transport().fail_next = Some(Operation::Flush);
    assert_eq!(run(fs.finish_write(volume, token)).unwrap_err().cause,
               Cause::Host(mac_host_protocol::status::IO));
    let stage = fs.transport()
                  .trace
                  .iter()
                  .rev()
                  .find(|&&(op, _, offset, len)| {
                      op == Operation::Write && offset == 0 && len == mac_fork_file::HEADER_BYTES
                  })
                  .unwrap()
                  .1;
    let count = fs.transport()
                  .trace
                  .iter()
                  .filter(|&&(op, object, _, _)| op == Operation::Write && object == stage)
                  .count();
    assert_eq!(run(fs.finish_write(volume, token)).unwrap(), id);
    assert_eq!(fs.transport()
                 .trace
                 .iter()
                 .filter(|&&(op, object, _, _)| op == Operation::Write && object == stage)
                 .count(),
               count);
    run(fs.unmount(volume)).unwrap();
    Volume::open(&temp.0).unwrap().check().unwrap();
}

#[test]
fn cleanup_failure_after_publication_keeps_actual_result_and_does_not_repeat_commits()
{
    use vhost_filesystem::Create;
    let (temp, _, _, _) = seeded();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let parent = run(fs.lookup(volume, 2, "Tools")).unwrap().metadata.catalog_id;
    let mut writes = common::Tokens::new();
    let token = writes.insert(()).unwrap();
    let id = run(fs.begin_create(Create { volume,
                                          parent,
                                          token,
                                          properties: mac_filesystem_interface::FileProperties::fresh("Published", 30),
                                          data_length: 0,
                                          resource_length: 0,
                                          when: 30 })).unwrap();
    fs.transport().fail_next = Some(Operation::Close);
    assert_eq!(run(fs.finish_write(volume, token)).unwrap_err().cause,
               Cause::Host(mac_host_protocol::status::IO));
    let commits = fs.transport().commits.clone();
    run(fs.cleanup_orphans()).unwrap();
    assert_eq!(run(fs.finish_write(volume, token)).unwrap(), id);
    let actual = &fs.transport().commits;
    assert_eq!(&actual[..commits.len()], commits);
    // Identity annotations and parent/volume dates are publication obligations
    // too. Retry may finish those, but must never republish the user file.
    assert_eq!(actual.iter().filter(|name| name.as_slice() == b"Published").count(), 1);
    let suffix = &actual[commits.len()..];
    for (at, name) in suffix.iter().enumerate()
    {
        assert!(!suffix[..at].contains(name), "cleanup repeated a metadata publication");
    }
    assert_eq!(fs.cached_node(volume, parent).unwrap().metadata.modified, 30);
    run(fs.unmount(volume)).unwrap();
    let physical = Volume::open(&temp.0).unwrap();
    assert_eq!(physical.resolve("/Tools/Published").unwrap().metadata.catalog_id, id);
    assert_eq!(physical.resolve("/Tools").unwrap().metadata.modified, 30);
    physical.check().unwrap();
}

#[test]
fn abort_after_publication_reports_actual_metadata_without_a_second_commit()
{
    use mac_filesystem_interface::{self as wire, Arguments as A, Operation as O};
    let (temp, _, _, _) = seeded();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    let a = A { volume,
                ..Default::default() };
    serve(&mut fs, O::Mount, a).unwrap();
    let mut writes = common::Tokens::new();
    let token = writes.insert(()).unwrap();
    let a = A { token,
                ..a };
    let out = serve(&mut fs,
                    O::BeginCreate,
                    A { parent: 2,
                        when: 55,
                        payload: &create_bytes("Published abort", 55),
                        ..a }).unwrap();
    let id = wire::Metadata::decode(&out).unwrap().id;
    fs.transport().fail_next = Some(Operation::Close);
    assert_eq!(serve(&mut fs, O::FinishWrite, a).unwrap_err().cause,
               Cause::Host(mac_host_protocol::status::IO));
    let commits =
        fs.transport().commits.iter().filter(|name| name.as_slice() == b"Published abort").count();
    let out = serve(&mut fs, O::AbortWrite, a).unwrap();
    let receipt = wire::AbortReceipt::decode(&out).unwrap().0.unwrap();
    assert_eq!(receipt.file.id, id);
    assert_eq!(receipt.file.name, "Published abort");
    assert_eq!(receipt.file.modified, 55);
    assert_eq!(receipt.volume.root.modified, 55);
    assert_eq!(fs.transport()
                 .commits
                 .iter()
                 .filter(|name| name.as_slice() == b"Published abort")
                 .count(),
               commits);
    serve(&mut fs,
          O::Unmount,
          A { token: 0,
              ..a }).unwrap();
    let physical = Volume::open(&temp.0).unwrap();
    assert_eq!(physical.resolve("/Published abort").unwrap().metadata.catalog_id, id);
    physical.check().unwrap();
}

#[test]
fn casefold_collisions_invalid_names_and_read_only_writes_do_not_reserve_catalog_ids()
{
    use vhost_filesystem::Create;
    let (temp, _, _, _) = seeded();
    let physical = Volume::open(&temp.0).unwrap();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let initial = physical.root().unwrap().metadata.next_catalog_id;
    let parent = run(fs.lookup(volume, 2, "Tools")).unwrap().metadata.catalog_id;
    let mut writes = common::Tokens::new();
    let token = writes.insert(()).unwrap();
    for name in ["CAFÉ", "cafe\u{301}"]
    {
        assert_eq!(run(fs.begin_create(Create { volume,
                                                parent,
                                                token,
                                                properties: mac_filesystem_interface::FileProperties::fresh(name, 40),
                                                data_length: 0,
                                                resource_length: 0,
                                                when: 40 })).unwrap_err()
                                                            .cause,
                   Cause::Conflict);
    }
    let long = "a".repeat(256);
    assert_eq!(run(fs.begin_create(Create { volume,
                                            parent,
                                            token,
                                            properties: mac_filesystem_interface::FileProperties::fresh(&long, 40),
                                            data_length: 0,
                                            resource_length: 0,
                                            when: 40 })).unwrap_err()
                                                        .cause,
               Cause::Invalid);
    assert_eq!(physical.root().unwrap().metadata.next_catalog_id, initial);
    run(fs.unmount(volume)).unwrap();
    let mut fs = Filesystem::new(Host::new(&temp, false));
    let readonly = volumes.insert(()).unwrap();
    run(fs.mount(readonly, 0)).unwrap();
    assert_eq!(run(fs.begin_create(Create { volume: readonly,
                                            parent: 2,
                                            token,
                                            properties: mac_filesystem_interface::FileProperties::fresh("Read Only", 40),
                                            data_length: 0,
                                            resource_length: 0,
                                            when: 40 })).unwrap_err()
                                                        .cause,
               Cause::ReadOnly);
    assert_eq!(physical.root().unwrap().metadata.next_catalog_id, initial);
    run(fs.unmount(readonly)).unwrap();
}

#[test]
fn external_changes_report_stale_then_refresh_preserves_existing_fork_leases()
{
    let (temp, cnid, data, _) = seeded();
    let physical = Volume::open(&temp.0).unwrap();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let parent = run(fs.lookup(volume, 2, "Tools")).unwrap().metadata.catalog_id;
    run(fs.scan(volume, parent)).unwrap();
    let mut opens = Table::new(ObjectKind::Files);
    let token = opens.insert(()).unwrap();
    run(fs.open(volume, cnid, u64::from(token))).unwrap();
    physical.create("/Tools",
                    "From Host",
                    Kind::File,
                    &mut Slice(b"outside"),
                    &mut Slice(&[]),
                    None)
            .unwrap();
    assert_eq!(run(fs.lookup(volume, parent, "From Host")).unwrap_err().cause, Cause::Stale);
    run(fs.refresh(volume)).unwrap();
    let parent = run(fs.lookup(volume, 2, "Tools")).unwrap().metadata.catalog_id;
    assert_eq!(run(fs.lookup(volume, parent, "FROM HOST")).unwrap().metadata.name, "From Host");
    assert_eq!(run(fs.read(volume, u64::from(token), Fork::Data, 0, 31)).unwrap(), data[..31]);
    run(fs.close_file(volume, u64::from(token))).unwrap();
    run(fs.unmount(volume)).unwrap();
}

#[test]
fn abandoned_mount_acquisition_is_cleaned_without_a_transport_error()
{
    let (temp, _, _, _) = seeded();
    let mut filesystem = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    {
        let mut mount = pin!(filesystem.mount(volume, 0));
        let mut cx = Context::from_waker(Waker::noop());
        // Hello, then Export, then the first metadata read is pending. The
        // exported root capability has been acquired but not published.
        for _ in 0..3
        {
            assert!(mount.as_mut().poll(&mut cx).is_pending());
        }
    }
    assert!(filesystem.info(volume).is_err());
    let before = filesystem.transport().calls.len();
    run(filesystem.cleanup_orphans()).unwrap();
    assert_eq!(&filesystem.transport().calls[before..], &[Operation::Close]);
    let cleaned = filesystem.transport().calls.len();
    run(filesystem.cleanup_orphans()).unwrap();
    assert_eq!(filesystem.transport().calls.len(), cleaned, "clean maintenance did host I/O");
    volumes.remove(volume);
    let replacement = volumes.insert(()).unwrap();
    assert_ne!(volume, replacement);
    run(filesystem.mount(replacement, 0)).unwrap();
    run(filesystem.unmount(replacement)).unwrap();
}

fn serve(fs: &mut Filesystem<Host>, op: mac_filesystem_interface::Operation,
         args: mac_filesystem_interface::Arguments<'_>)
         -> Result<Vec<u8>>
{
    let mut bytes = vec![0; mac_filesystem_interface::HEADER_BYTES + args.payload.len()];
    args.encode(&mut bytes).unwrap();
    run(fs.execute(op as u32, &bytes))
}
fn create_bytes(name: &str, when: u64) -> Vec<u8>
{
    use mac_filesystem_interface as fs;
    let mut bytes = vec![0; fs::PROPERTIES_BYTES + name.len()];
    fs::FileProperties::fresh(name, when).encode(&mut bytes).unwrap();
    bytes
}
#[test]
fn data_only_replacement_preserves_resources_and_aborts_partial_copy_failures()
{
    use mac_filesystem_interface::{self as fs, Arguments as A, Operation as O};
    let (temp, node, original, resources) = seeded();
    let physical = Volume::open(&temp.0).unwrap();
    let before = physical.resolve("/Tools/Café").unwrap().metadata;
    let mut filesystem = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    let args = A { volume,
                   ..Default::default() };
    serve(&mut filesystem, O::Mount, args).unwrap();
    let mut writes = common::Tokens::new();
    let token = writes.insert(()).unwrap();
    let when = 3_875_171_696_123_456;
    let data = vec![0x73; 9003];
    let replace = A { node,
                      token,
                      flags: fs::PRESERVE_RESOURCE_FORK,
                      length: data.len() as u64,
                      when,
                      ..args };
    let prospective = serve(&mut filesystem, O::BeginReplace, replace).unwrap();
    let prospective = fs::Metadata::decode(&prospective).unwrap();
    assert_eq!(prospective.id, node);
    assert_eq!(prospective.data_length, data.len() as u64);
    assert_eq!(prospective.resource_length, resources.len() as u64);
    let before_calls = filesystem.transport().calls.len();
    for bytes in [&b"bad"[..], &b""[..]]
    {
        assert_eq!(serve(&mut filesystem,
                         O::WriteChunk,
                         A { token,
                             payload: bytes,
                             ..args }).unwrap_err()
                                      .cause,
                   Cause::Invalid);
    }
    assert_eq!(filesystem.transport().calls.len(), before_calls);
    for bytes in data.chunks(fs::MAX_TRANSFER)
    {
        serve(&mut filesystem,
              O::WriteChunk,
              A { token,
                  payload: bytes,
                  ..args }).unwrap();
    }
    let mut files = Table::new(ObjectKind::Files);
    let reader = files.insert(()).unwrap();
    serve(&mut filesystem,
          O::Open,
          A { node,
              token: u64::from(reader),
              ..args }).unwrap();
    assert_eq!(serve(&mut filesystem,
                     O::FinishWrite,
                     A { token,
                         ..args }).unwrap_err()
                                  .cause,
               Cause::Busy);
    assert_eq!(serve(&mut filesystem,
                     O::Read,
                     A { token: u64::from(reader),
                         length: fs::MAX_TRANSFER as u64,
                         ..args }).unwrap(),
               original);
    serve(&mut filesystem,
          O::Close,
          A { token: u64::from(reader),
              ..args }).unwrap();
    serve(&mut filesystem,
          O::FinishWrite,
          A { token,
              ..args }).unwrap();
    let mut expected = before;
    expected.modified = when;
    expected.revision += 1;
    assert_eq!(physical.resolve("/Tools/Café").unwrap().metadata, expected);
    files.remove(reader);
    let reader = files.insert(()).unwrap();
    serve(&mut filesystem,
          O::Open,
          A { node,
              token: u64::from(reader),
              ..args }).unwrap();
    assert_eq!(serve(&mut filesystem,
                     O::Read,
                     A { token: u64::from(reader),
                         length: fs::MAX_TRANSFER as u64,
                         ..args }).unwrap(),
               data);
    assert_eq!(serve(&mut filesystem,
                     O::Read,
                     A { token: u64::from(reader),
                         flags: fs::RESOURCE_FORK,
                         length: fs::MAX_TRANSFER as u64,
                         ..args }).unwrap(),
               resources);
    serve(&mut filesystem,
          O::Close,
          A { token: u64::from(reader),
              ..args }).unwrap();
    writes.remove(token);
    let token = writes.insert(()).unwrap();
    // Fail after one old resource chunk has been verified and copied to the
    // hidden stage. Abort must preserve the entire previously published file.
    filesystem.transport().fail_resource_read_after = Some(1);
    assert_eq!(serve(&mut filesystem,
                     O::BeginReplace,
                     A { token,
                         when: when + 1,
                         ..replace }).unwrap_err()
                                     .cause,
               Cause::Host(mac_host_protocol::status::IO));
    assert_eq!(serve(&mut filesystem,
                     O::AbortWrite,
                     A { token,
                         ..args }).unwrap(),
               0u32.to_le_bytes());
    assert_eq!(physical.resolve("/Tools/Café").unwrap().metadata, expected);
    assert_eq!(physical.resolve("/").unwrap().metadata.modified, when);
    serve(&mut filesystem, O::Unmount, args).unwrap();
    physical.check().unwrap();
}
#[test]
fn filesystem_creation_preserves_properties_and_normalizes_before_the_byte_limit()
{
    use mac_filesystem_interface::{self as fs, Arguments as A, Operation as O};
    let (temp, _, _, _) = seeded();
    let mut filesystem = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    let args = A { volume,
                   ..Default::default() };
    serve(&mut filesystem, O::Mount, args).unwrap();
    let tools = serve(&mut filesystem,
                      O::Lookup,
                      A { parent: 2,
                          payload: b"Tools",
                          ..args }).unwrap();
    let parent = fs::Metadata::decode(&tools).unwrap().id;
    let name = "e\u{301}".repeat(127) + "x";
    let normalized = mac_hfs_b::names::normalize(&name).unwrap();
    assert!(name.len() > 255);
    assert_eq!(normalized.len(), 255);
    let mut props = fs::FileProperties::fresh(&name, 3_875_171_696_123_456);
    props.modified += 42;
    props.backup = Some(0);
    props.attributes = 1;
    props.finder_info = [0x37; 32];
    props.finder_info[..8].copy_from_slice(b"TEXTTEST");
    props.comment = "Source metadata\r🦀";
    let mut payload = vec![0; 2048];
    let count = props.encode(&mut payload).unwrap();
    payload.truncate(count);
    let mut writes = common::Tokens::new();
    let token = writes.insert(()).unwrap();
    let when = props.modified + 999;
    let id = serve(&mut filesystem,
                   O::BeginCreate,
                   A { parent,
                       token,
                       length: 3,
                       auxiliary: 2,
                       when,
                       payload: &payload,
                       ..args }).unwrap();
    let prospective = fs::Metadata::decode(&id).unwrap();
    assert_eq!(prospective.name, normalized);
    assert_eq!(prospective.modified, props.modified);
    let id = prospective.id;
    serve(&mut filesystem,
          O::WriteChunk,
          A { token,
              payload: b"abc",
              ..args }).unwrap();
    serve(&mut filesystem,
          O::WriteChunk,
          A { token,
              flags: fs::RESOURCE_FORK,
              payload: b"rs",
              ..args }).unwrap();
    serve(&mut filesystem,
          O::FinishWrite,
          A { token,
              ..args }).unwrap();
    let found = serve(&mut filesystem,
                      O::Lookup,
                      A { parent,
                          payload: name.as_bytes(),
                          ..args }).unwrap();
    let metadata = fs::Metadata::decode(&found).unwrap();
    assert_eq!(metadata.id, id);
    assert_eq!(metadata.name, normalized);
    assert_eq!((metadata.created, metadata.modified, metadata.backup),
               (props.created, props.modified, props.backup));
    assert_eq!((metadata.attributes, metadata.finder_info, metadata.comment),
               (props.attributes, props.finder_info, props.comment));
    serve(&mut filesystem, O::Unmount, args).unwrap();
    let physical = Volume::open(&temp.0).unwrap();
    let file = physical.resolve(&format!("/Tools/{normalized}")).unwrap();
    assert_eq!(file.metadata.catalog_id, id);
    assert_eq!(physical.resolve("/Tools").unwrap().metadata.modified, when);
    assert_eq!(physical.root().unwrap().metadata.modified, when);
    physical.check().unwrap();
}
#[test]
fn metadata_only_ancestry_does_not_enumerate_folders_and_count_requests_are_explicit()
{
    use mac_filesystem_interface::{self as fs, Arguments as A, Operation as O};
    let (temp, _, _, _) = seeded();
    let mut filesystem = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    let args = A { volume,
                   ..Default::default() };
    serve(&mut filesystem, O::Mount, args).unwrap();
    let bytes = serve(&mut filesystem,
                      O::Catalog,
                      A { node: 2,
                          ..args }).unwrap();
    assert_eq!(fs::Metadata::decode(&bytes).unwrap().child_count, fs::CHILD_COUNT_UNKNOWN);
    assert!(!filesystem.transport().calls.contains(&Operation::ListBegin));
    let bytes = serve(&mut filesystem,
                      O::Catalog,
                      A { node: 2,
                          flags: fs::INCLUDE_CHILD_COUNT,
                          ..args }).unwrap();
    assert_eq!(fs::Metadata::decode(&bytes).unwrap().child_count, 1);
    let before =
        filesystem.transport().calls.iter().filter(|&&op| op == Operation::ListBegin).count();
    let bytes = serve(&mut filesystem,
                      O::Lookup,
                      A { parent: 2,
                          payload: b"Tools",
                          ..args }).unwrap();
    assert_eq!(fs::Metadata::decode(&bytes).unwrap().child_count, fs::CHILD_COUNT_UNKNOWN);
    assert_eq!(filesystem.transport()
                         .calls
                         .iter()
                         .filter(|&&op| op == Operation::ListBegin)
                         .count(),
               before);
    let bytes = serve(&mut filesystem,
                      O::Lookup,
                      A { parent: 2,
                          payload: b"Tools",
                          flags: fs::INCLUDE_CHILD_COUNT,
                          ..args }).unwrap();
    assert_eq!(fs::Metadata::decode(&bytes).unwrap().child_count, 1);
    assert_eq!(filesystem.transport()
                         .calls
                         .iter()
                         .filter(|&&op| op == Operation::ListBegin)
                         .count(),
               before + 1);
    let before = filesystem.transport().calls.len();
    for (op, args) in [(O::Catalog,
                        A { node: 2,
                            flags: 2,
                            ..args }),
                       (O::Lookup,
                        A { parent: 2,
                            flags: 2,
                            payload: b"Tools",
                            ..args }),
                       (O::Scan,
                        A { parent: 2,
                            flags: 1,
                            ..args })]
    {
        assert_eq!(serve(&mut filesystem, op, args).unwrap_err().cause, Cause::Invalid);
    }
    assert_eq!(filesystem.transport().calls.len(), before);
}
#[test]
fn filesystem_class_exchanges_metadata_and_both_forks_through_owned_byte_requests()
{
    use mac_filesystem_interface::{self as fs, Arguments as A, Operation as O};
    let (temp, cnid, data, resources) = seeded();
    let mut filesystem = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    let args = A { volume,
                   ..Default::default() };
    let bytes = serve(&mut filesystem, O::Mount, args).unwrap();
    let mounted = fs::Mount::decode(&bytes).unwrap();
    assert_eq!(mounted.filesystem, *b"HOST");
    assert_eq!(mounted.filesystem_version, 1);
    assert_eq!(mounted.capabilities, fs::READ | fs::WRITE | fs::CHANGES);
    assert_eq!(mounted.capacity, None);
    assert_eq!(mounted.root.child_count, fs::CHILD_COUNT_UNKNOWN);
    assert!(!filesystem.transport().calls.iter().any(|op| {
                                                    matches!(op,
                                                             Operation::ListBegin
                                                             | Operation::ListNext)
                                                }));
    let bytes = serve(&mut filesystem,
                      O::Catalog,
                      A { node: cnid,
                          ..args }).unwrap();
    let file = fs::Metadata::decode(&bytes).unwrap();
    assert_eq!(file.name, "Café");
    assert_eq!(file.data_length, data.len() as u64);
    assert_eq!(file.resource_length, resources.len() as u64);
    let mut files = Table::new(ObjectKind::Files);
    let token = files.insert(()).unwrap();
    let open = A { token: u64::from(token),
                   ..args };
    serve(&mut filesystem,
          O::Open,
          A { node: cnid,
              ..open }).unwrap();
    assert_eq!(serve(&mut filesystem,
                     O::Read,
                     A { length: fs::MAX_TRANSFER as u64,
                         flags: fs::RESOURCE_FORK,
                         ..open }).unwrap(),
               resources);
    serve(&mut filesystem, O::Close, open).unwrap();
    files.remove(token);
    let replacement = files.insert(()).unwrap();
    assert_ne!(token, replacement);
    assert_eq!(serve(&mut filesystem,
                     O::Read,
                     A { length: 1,
                         ..open }).unwrap_err()
                                  .cause,
               Cause::NotFound);
    let mut writes = common::Tokens::new();
    let token = writes.insert(()).unwrap();
    let write = A { token,
                    ..args };
    let id = serve(&mut filesystem,
                   O::BeginCreate,
                   A { parent: 2,
                       length: 3,
                       auxiliary: 2,
                       when: 3_875_171_696_123_456,
                       payload: &create_bytes("Copied", 3_875_171_696_123_456),
                       ..write }).unwrap();
    let cnid = fs::Metadata::decode(&id).unwrap().id;
    serve(&mut filesystem,
          O::WriteChunk,
          A { payload: b"abc",
              ..write }).unwrap();
    serve(&mut filesystem,
          O::WriteChunk,
          A { flags: fs::RESOURCE_FORK,
              payload: b"rs",
              ..write }).unwrap();
    let receipt = serve(&mut filesystem, O::FinishWrite, write).unwrap();
    let receipt = fs::WriteReceipt::decode(&receipt).unwrap();
    assert_eq!(receipt.file.id, cnid);
    assert_eq!(receipt.file.name, "Copied");
    assert_eq!((receipt.file.data_length, receipt.file.resource_length), (3, 2));
    assert_eq!(receipt.volume.filesystem, *b"HOST");
    let copied = Volume::open(&temp.0).unwrap().resolve("/Copied").unwrap();
    assert_eq!(copied.metadata.catalog_id, cnid);
    let bytes = serve(&mut filesystem,
                      O::Catalog,
                      A { node: cnid,
                          ..args }).unwrap();
    assert_eq!(fs::Metadata::decode(&bytes).unwrap().resource_length, 2);
    serve(&mut filesystem, O::Unmount, args).unwrap();
    Volume::open(&temp.0).unwrap().check().unwrap();
}
#[test]
fn filesystem_class_rejects_invalid_requests_and_read_only_mutations_without_host_side_effects()
{
    use mac_filesystem_interface::{Arguments as A, Operation as O};
    let (temp, _, _, _) = seeded();
    let mut filesystem = Filesystem::new(Host::new(&temp, false));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    let args = A { volume,
                   ..Default::default() };
    serve(&mut filesystem, O::Mount, args).unwrap();
    let before = filesystem.transport().calls.len();
    assert_eq!(serve(&mut filesystem,
                     O::Read,
                     A { token: 1,
                         length: 65537,
                         ..args }).unwrap_err()
                                  .cause,
               Cause::Invalid);
    assert_eq!(serve(&mut filesystem,
                     O::Catalog,
                     A { node: 2,
                         flags: 2,
                         ..args }).unwrap_err()
                                  .cause,
               Cause::Invalid);
    assert_eq!(filesystem.transport().calls.len(), before, "invalid wire fields reached host I/O");
    let mut writes = common::Tokens::new();
    let token = writes.insert(()).unwrap();
    let before = Volume::open(&temp.0).unwrap().root().unwrap().metadata;
    assert_eq!(serve(&mut filesystem,
                     O::BeginCreate,
                     A { parent: 2,
                         token,
                         payload: &create_bytes("No write", 0),
                         ..args }).unwrap_err()
                                  .cause,
               Cause::ReadOnly);
    assert_eq!(Volume::open(&temp.0).unwrap().root().unwrap().metadata, before);
    assert!(!filesystem.transport().calls.contains(&Operation::BeginWrite));
    serve(&mut filesystem, O::Unmount, args).unwrap();
}

#[test]
fn metadata_failures_retry_the_consumed_child_instead_of_skipping_it()
{
    for failure in [Operation::OpenRead, Operation::Read, Operation::Close]
    {
        let (temp, _, _, _) = seeded();
        let mut filesystem = Filesystem::new(Host::new(&temp, true));
        let mut volumes = Table::new(ObjectKind::Volumes);
        let volume = volumes.insert(()).unwrap();
        run(filesystem.mount(volume, 0)).unwrap();
        let directory_baseline = filesystem.transport()
                                           .calls
                                           .iter()
                                           .filter(|&&op| op == Operation::OpenDirectory)
                                           .count();
        filesystem.transport().fail_next = Some(failure);
        assert_eq!(run(filesystem.scan(volume, 2)).unwrap_err().cause,
                   Cause::Host(mac_host_protocol::status::IO));
        run(filesystem.cleanup_orphans()).unwrap();
        run(filesystem.scan(volume, 2)).unwrap();
        assert_eq!(filesystem.child(volume, 2, 0).unwrap().metadata.name,
                   "Tools",
                   "lost child after {failure:?}");
        assert_eq!(filesystem.transport()
                             .calls
                             .iter()
                             .filter(|&&op| op == Operation::OpenDirectory)
                             .count(),
                   directory_baseline + 1,
                   "retry reopened an owned directory");
        assert_eq!(filesystem.transport()
                             .calls
                             .iter()
                             .filter(|&&op| op == Operation::ListBegin)
                             .count(),
                   1,
                   "retry restarted the directory");
        run(filesystem.unmount(volume)).unwrap();
    }
}
#[test]
fn abandoned_scan_retains_the_consumed_entry_until_its_metadata_is_published()
{
    let (temp, _, _, _) = seeded();
    let mut filesystem = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(filesystem.mount(volume, 0)).unwrap();
    let observed = filesystem.transport().reached_child.clone();
    {
        let mut scan = pin!(filesystem.scan(volume, 2));
        let mut cx = Context::from_waker(Waker::noop());
        let mut stopped = false;
        for _ in 0..1000
        {
            assert!(scan.as_mut().poll(&mut cx).is_pending());
            if observed.load(Ordering::Relaxed)
            {
                stopped = true;
                break;
            }
        }
        assert!(stopped, "test did not reach the split child/metadata boundary");
    }
    run(filesystem.cleanup_orphans()).unwrap();
    run(filesystem.scan(volume, 2)).unwrap();
    assert_eq!(filesystem.child(volume, 2, 0).unwrap().metadata.name, "Tools");
    assert_eq!(filesystem.transport()
                         .calls
                         .iter()
                         .filter(|&&op| op == Operation::ListBegin)
                         .count(),
               1);
    run(filesystem.unmount(volume)).unwrap();
}

#[test]
fn directory_creation_publishes_metadata_atomically_and_supports_nested_writes()
{
    use vhost_filesystem::Create;
    let (temp, _, _, _) = seeded();
    std::fs::create_dir(temp.0.join("Host Folder")).unwrap();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut ids = Table::new(ObjectKind::Volumes);
    let volume = ids.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let parent = run(fs.lookup(volume, 2, "Host Folder")).unwrap().metadata.catalog_id;
    let mut marks = common::Tokens::new();
    let mark = marks.insert(()).unwrap();
    let folder=run(fs.begin_directory(Create {volume,parent,token:mark,
        properties:mac_filesystem_interface::FileProperties::fresh("Café",4_000_000_000_000_000),
        data_length:0,resource_length:0,when:4_000_000_000_000_000})).unwrap();
    let request = mac_filesystem_interface::Arguments { volume,
                                                        token: mark,
                                                        ..Default::default() };
    let mut request_bytes = [0; mac_filesystem_interface::HEADER_BYTES];
    request.encode(&mut request_bytes).unwrap();
    let receipt = run(fs.execute(mac_filesystem_interface::Operation::FinishWrite as u32,
                                 &request_bytes)).unwrap();
    assert_eq!(mac_filesystem_interface::WriteReceipt::decode(&receipt).unwrap().file.id, folder);
    assert_eq!(run(fs.lookup(volume, parent, "CAFE\u{301}")).unwrap().metadata.catalog_id, folder);
    assert_eq!(fs.cached_node(volume, folder).unwrap().metadata.kind, Kind::Directory);
    assert!(fs.child(volume, folder, 0).is_err());
    marks.remove(mark);
    let next = marks.insert(()).unwrap();
    run(fs.begin_create(Create {volume,parent:folder,token:next,
        properties:mac_filesystem_interface::FileProperties::fresh("Child",4_000_000_000_000_001),
        data_length:0,resource_length:0,when:4_000_000_000_000_001})).unwrap();
    run(fs.finish_write(volume, next)).unwrap();
    let physical = "Café";
    assert!(temp.0
                .join("Host Folder")
                .join(physical)
                .join(mac_fork_file::DIRECTORY_RECORD)
                .exists());
    run(fs.refresh(volume)).unwrap();
    let parent = run(fs.lookup(volume, 2, "Host Folder")).unwrap().metadata.catalog_id;
    let folder = run(fs.lookup(volume, parent, "Café")).unwrap().metadata.catalog_id;
    assert_eq!(run(fs.lookup(volume, folder, "Child")).unwrap().metadata.kind, Kind::File);
    run(fs.unmount(volume)).unwrap();
}

#[test]
fn directory_change_probe_is_metadata_only_scoped_and_preserves_open_snapshots()
{
    let (temp, cnid, data, _) = seeded();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut ids = Table::new(ObjectKind::Volumes);
    let volume = ids.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let folder = run(fs.lookup(volume, 2, "Tools")).unwrap().metadata.catalog_id;
    run(fs.scan(volume, folder)).unwrap();
    let mut files = Table::new(ObjectKind::Files);
    let mark = files.insert(()).unwrap();
    run(fs.open(volume, cnid, u64::from(mark))).unwrap();
    fs.transport().calls.clear();
    assert!(!run(fs.check_directory(volume, folder)).unwrap().0);
    assert_eq!(fs.transport().calls, [Operation::Stat]);
    let physical = "Tools";
    std::fs::write(temp.0.join(physical).join("Arrived.txt"), b"arrival").unwrap();
    fs.transport().calls.clear();
    assert!(run(fs.check_directory(volume, folder)).unwrap().0);
    assert_eq!(fs.transport().calls, [Operation::Stat], "probe must never enumerate");
    assert!(fs.cached_node(volume, 2).is_ok(), "unrelated parent cache must survive");
    assert_eq!(run(fs.read(volume, u64::from(mark), Fork::Data, 0, 32)).unwrap(), &data[..32]);
    assert_eq!(run(fs.lookup(volume, folder, "Arrived.txt")).unwrap().layout.data.length, 7);
    assert!(!run(fs.check_directory(volume, folder)).unwrap().0);
    // Editing a child's data leaves the directory mtime unchanged. The host's
    // notification generation must still invalidate the cached metadata.
    std::fs::write(temp.0.join(physical).join("Arrived.txt"), b"updated").unwrap();
    assert!(run(fs.check_directory(volume, folder)).unwrap().0);
    assert_eq!(run(fs.lookup(volume, folder, "Arrived.txt")).unwrap().layout.data.length, 7);
    run(fs.close_file(volume, u64::from(mark))).unwrap();
    run(fs.unmount(volume)).unwrap();
}

#[test]
fn native_host_renames_preserve_identity_forks_and_nested_writes_across_remount()
{
    use vhost_filesystem::Create;
    let (temp, file, data, resources) = seeded();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let folder = run(fs.lookup(volume, 2, "Tools")).unwrap().metadata.catalog_id;
    run(fs.scan(volume, folder)).unwrap();
    std::fs::rename(temp.0.join("Tools"), temp.0.join("Renamed Tools")).unwrap();
    std::fs::rename(temp.0.join("Renamed Tools/Café"), temp.0.join("Renamed Tools/New name")).unwrap();
    run(fs.refresh(volume)).unwrap();
    assert_eq!(run(fs.lookup(volume, 2, "Renamed Tools")).unwrap().metadata.catalog_id, folder);
    assert_eq!(run(fs.lookup(volume, folder, "New name")).unwrap().metadata.catalog_id, file);
    let mut writes = common::Tokens::new();
    let mark = writes.insert(()).unwrap();
    run(fs.begin_create(Create { volume, parent: folder, token: mark,
        properties: mac_filesystem_interface::FileProperties::fresh("Readable Child", 4_000_000_000_000_000),
        data_length: 0, resource_length: 0, when: 4_000_000_000_000_000 })).unwrap();
    run(fs.finish_write(volume, mark)).unwrap();
    assert!(temp.0.join("Renamed Tools/Readable Child").is_file());
    run(fs.unmount(volume)).unwrap();
    // New transport and driver discard every in-memory path/cache/lease.
    drop(fs);
    let mut fs = Filesystem::new(Host::new(&temp, true));
    run(fs.mount(volume, 0)).unwrap();
    assert_eq!(run(fs.lookup(volume, 2, "renamed tools")).unwrap().metadata.catalog_id, folder);
    assert_eq!(run(fs.lookup(volume, folder, "NEW NAME")).unwrap().metadata.catalog_id, file);
    let mut opens = Table::new(ObjectKind::Files);
    let open = opens.insert(()).unwrap();
    run(fs.open(volume, file, u64::from(open))).unwrap();
    assert_eq!(run(fs.read(volume, u64::from(open), Fork::Data, 0, data.len())).unwrap(), data);
    assert_eq!(run(fs.read(volume, u64::from(open), Fork::Resource, 0, resources.len())).unwrap(),
               resources);
    run(fs.close_file(volume, u64::from(open))).unwrap();
    run(fs.unmount(volume)).unwrap();
    assert_eq!(Volume::open(&temp.0).unwrap()
                                    .resolve("/Renamed Tools/New name")
                                    .unwrap()
                                    .metadata
                                    .catalog_id,
               file);
}

#[test]
fn plain_open_binds_catalog_metadata_to_the_exact_snapshot_revision()
{
    let temp = Temp::new();
    Volume::initialize(&temp.0, "Snapshot").unwrap();
    let path = temp.0.join("plain");
    std::fs::write(&path, b"old").unwrap();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let id = run(fs.lookup(volume, 2, "plain")).unwrap().metadata.catalog_id;
    let mut files = Table::new(ObjectKind::Files);
    let token = files.insert(()).unwrap();
    fs.transport().modify_before_description = Some(path);
    let failure = run(fs.open(volume, id, u64::from(token))).unwrap_err();
    assert_eq!(failure.cause, Cause::Stale);
    assert!(!fs.is_open(volume, id), "failed open retained a live fork");
    assert_eq!(run(fs.read(volume, u64::from(token), Fork::Data, 0, 3)).unwrap_err().cause,
               Cause::NotFound);
    run(fs.cleanup_orphans()).unwrap();
    run(fs.refresh(volume)).unwrap();
    assert_eq!(run(fs.lookup(volume, 2, "plain")).unwrap().metadata.catalog_id, id);
    run(fs.open(volume, id, u64::from(token))).unwrap();
    assert_eq!(run(fs.read(volume, u64::from(token), Fork::Data, 0, 3)).unwrap(), b"new");
    run(fs.close_file(volume, u64::from(token))).unwrap();
    run(fs.unmount(volume)).unwrap();
}

#[test]
fn small_metadata_edits_preserve_plain_host_bytes_and_identity_across_rename_and_reboot()
{
    use mac_filesystem_interface::MetadataPatch;
    let temp = Temp::new();
    Volume::initialize(&temp.0, "Metadata").unwrap();
    std::fs::write(temp.0.join("plain.txt"), b"original host bytes").unwrap();
    std::fs::create_dir(temp.0.join("raw folder")).unwrap();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let original = run(fs.lookup(volume, 2, "plain.txt")).unwrap().metadata.clone();
    let metadata_when = fs.info(volume).unwrap().metadata.modified;
    let mut writes = common::Tokens::new();
    let token = writes.insert(()).unwrap();
    let mut patch = MetadataPatch { comment: Some("native café 🦀"),
                                    ..Default::default() };
    patch.finder_mask[8] = 1;
    patch.finder_info[8] = 1;
    patch.finder_mask[10..14].fill(255);
    patch.finder_info[10..14].copy_from_slice(&[0, 20, 0, 30]);
    fs.transport().trace.clear();
    assert!(run(fs.begin_metadata(volume, original.catalog_id, token, patch, metadata_when)).unwrap());
    assert_eq!(fs.cached_node(volume, original.catalog_id).unwrap().metadata.comment, "");
    assert_eq!(run(fs.write_chunk(volume, token, Fork::Data, b"no")).unwrap_err().cause,
               Cause::Invalid);
    run(fs.finish_write(volume, token)).unwrap();
    assert_eq!(std::fs::read(temp.0.join("plain.txt")).unwrap(), b"original host bytes");
    let saved = fs.cached_node(volume, original.catalog_id).unwrap().metadata.clone();
    assert_eq!(saved.comment, "native café 🦀");
    assert_eq!(saved.modified, original.modified);
    assert_eq!(saved.created, original.created);
    assert_eq!(&saved.finder_info[..8], &[0; 8]);
    assert_eq!(&saved.finder_info[10..14], &[0, 20, 0, 30]);
    assert_eq!(fs.transport()
                 .trace
                 .iter()
                 .filter(|t| t.0 == Operation::Write)
                 .map(|t| t.3)
                 .sum::<usize>(),
               mac_fork_file::annotation::BYTES);
    let calls = fs.transport().calls.len();
    assert!(!run(fs.begin_metadata(volume, original.catalog_id, token, patch, metadata_when)).unwrap());
    assert_eq!(fs.transport().calls.len(), calls, "no-change edit performed host I/O");
    let raw_folder = run(fs.lookup(volume, 2, "raw folder")).unwrap().metadata.catalog_id;
    assert!(run(fs.begin_metadata(volume,
                                  raw_folder,
                                  token,
                                  MetadataPatch { comment: Some("folder state"),
                                                  ..Default::default() },
                                  metadata_when)).unwrap());
    run(fs.finish_write(volume, token)).unwrap();
    run(fs.unmount(volume)).unwrap();
    drop(fs);
    std::fs::rename(temp.0.join("plain.txt"), temp.0.join("host renamed.txt")).unwrap();
    let mut fs = Filesystem::new(Host::new(&temp, false));
    let next = volumes.insert(()).unwrap();
    run(fs.mount(next, 0)).unwrap();
    let restored = run(fs.lookup(next, 2, "host renamed.txt")).unwrap().metadata.clone();
    assert_eq!(restored.catalog_id, original.catalog_id);
    assert_eq!(restored.comment, saved.comment);
    assert_eq!(restored.finder_info, saved.finder_info);
    assert_eq!(run(fs.lookup(next, 2, "raw folder")).unwrap().metadata.catalog_id, raw_folder);
    assert_eq!(fs.cached_node(next, raw_folder).unwrap().metadata.comment, "folder state");
    assert_eq!(run(fs.begin_metadata(next, restored.catalog_id, token, patch, metadata_when)).unwrap_err().cause,
               Cause::ReadOnly);
    run(fs.unmount(next)).unwrap();
}

#[test]
fn publishing_a_recursive_tree_keeps_destination_and_descendant_revision_caches_valid()
{
    use mac_filesystem_interface::{FileProperties, MetadataPatch};
    use vhost_filesystem::Create;
    let (temp, _, _, _) = seeded();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let mut marks = common::Tokens::new();
    let token = marks.insert(()).unwrap();
    let mkdir = |fs: &mut Filesystem<Host>, parent, name| {
        let id = run(fs.begin_directory(Create { volume,
                                                 parent,
                                                 token,
                                                 properties: FileProperties::fresh(name, 42),
                                                 data_length: 0,
                                                 resource_length: 0,
                                                 when: 42 })).unwrap();
        run(fs.finish_write(volume, token)).unwrap();
        id
    };
    let target = mkdir(&mut fs, 2, "Target");
    let stage = mkdir(&mut fs, 2, ".System RISC transfer test");
    let root = mkdir(&mut fs, stage, "Source");
    let child = mkdir(&mut fs, root, "Child");
    for (parent, name) in [(child, "Data"), (root, "Empty")]
    {
        run(fs.begin_create(Create { volume,
                                     parent,
                                     token,
                                     properties: FileProperties::fresh(name, 42),
                                     data_length: 0,
                                     resource_length: 0,
                                     when: 42 })).unwrap();
        run(fs.finish_write(volume, token)).unwrap();
    }
    for id in [child, root]
    {
        assert!(run(fs.begin_metadata(volume,
                                      id,
                                      token,
                                      MetadataPatch { modified: Some(12),
                                                      comment: Some("Copied"),
                                                      ..Default::default() },
                                      43)).unwrap());
        run(fs.finish_write(volume, token)).unwrap();
    }
    assert!(run(fs.begin_rename(volume, root, target, "Source", token, 44)).unwrap());
    run(fs.finish_write(volume, token)).unwrap();
    for id in [root]
    {
        assert!(run(fs.begin_metadata(volume,
                                      id,
                                      token,
                                      MetadataPatch { modified: Some(11),
                                                      ..Default::default() },
                                      44)).unwrap());
        run(fs.finish_write(volume, token)).unwrap();
    }
    run(fs.begin_remove(volume, stage, token, 44)).unwrap();
    run(fs.finish_write(volume, token)).unwrap();
    assert_eq!(run(fs.lookup(volume, target, "Source")).unwrap().metadata.catalog_id, root);
    assert_eq!(run(fs.lookup(volume, root, "Child")).unwrap().metadata.catalog_id, child);
    assert_eq!(run(fs.lookup(volume, child, "Data")).unwrap().metadata.name, "Data");
    std::fs::write(temp.0.join("Target/Source/Child/Data"), b"changed externally").unwrap();
    assert_eq!(run(fs.scan(volume, child)).unwrap_err().cause,
               Cause::Stale,
               "moving a directory may not suppress real changes to its children");
    run(fs.unmount(volume)).unwrap();
}

#[test]
fn root_view_state_and_explicit_date_restoration_keep_exact_metadata_receipts()
{
    use mac_filesystem_interface::MetadataPatch;
    let (temp, _, _, _) = seeded();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let original = fs.info(volume).unwrap().metadata.clone();
    let mut writes = common::Tokens::new();
    let token = writes.insert(()).unwrap();
    let when = original.modified + 1_000_000;
    let mut view = MetadataPatch::default();
    view.finder_mask[..8].fill(255);
    view.finder_info[..8].copy_from_slice(&[0, 40, 0, 60, 1, 40, 1, 80]);
    assert!(run(fs.begin_metadata(volume, 2, token, view, when)).unwrap());
    let confirmed = fs.cached_node(volume, 2).unwrap().metadata.clone();
    run(fs.finish_write(volume, token)).unwrap();
    let saved = fs.cached_node(volume, 2).unwrap().metadata.clone();
    assert_eq!(saved.modified, original.modified);
    assert_eq!(&saved.finder_info[..8], &view.finder_info[..8]);
    assert_eq!(saved.revision, confirmed.revision + 1);
    assert_eq!(fs.info(volume).unwrap().metadata, saved);
    // Transfer rollback supplies the old date explicitly. The operation's own
    // timestamp must not overwrite the restoration during volume publication.
    let restored_date = original.modified - 1;
    assert!(run(fs.begin_metadata(volume,
                                  2,
                                  token,
                                  MetadataPatch { modified: Some(restored_date),
                                                  ..Default::default() },
                                  when)).unwrap());
    run(fs.finish_write(volume, token)).unwrap();
    assert_eq!(fs.info(volume).unwrap().metadata.modified, restored_date);
    run(fs.unmount(volume)).unwrap();
    let remount = volumes.insert(()).unwrap();
    run(fs.mount(remount, 0)).unwrap();
    assert_eq!(fs.info(remount).unwrap().metadata.modified, restored_date);
    assert_eq!(&fs.info(remount).unwrap().metadata.finder_info[..8], &view.finder_info[..8]);
    run(fs.unmount(remount)).unwrap();
}

#[test]
fn wrapped_file_metadata_never_reads_or_rewrites_forks_and_direct_folder_records_persist()
{
    use mac_filesystem_interface::MetadataPatch;
    let (temp, file, _, _) = seeded();
    let before = std::fs::read(temp.0.join("Tools/Café")).unwrap();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let folder = run(fs.lookup(volume, 2, "Tools")).unwrap().metadata.catalog_id;
    run(fs.lookup(volume, folder, "Café")).unwrap();
    let metadata_when = fs.info(volume).unwrap().metadata.modified;
    let mut writes = common::Tokens::new();
    let token = writes.insert(()).unwrap();
    fs.transport().trace.clear();
    assert!(run(fs.begin_metadata(volume,
                                  file,
                                  token,
                                  MetadataPatch { comment: Some("resource app"),
                                                  ..Default::default() },
                                  metadata_when)).unwrap());
    run(fs.finish_write(volume, token)).unwrap();
    assert!(fs.transport()
              .trace
              .iter()
              .filter(|t| t.0 == Operation::Read)
              .all(|t| t.2 < 1024 && t.3 <= 1024));
    assert_eq!(std::fs::read(temp.0.join("Tools/Café")).unwrap(), before);
    assert!(run(fs.begin_metadata(volume,
                                  folder,
                                  token,
                                  MetadataPatch { comment: Some("native directory"),
                                                  ..Default::default() },
                                  metadata_when)).unwrap());
    run(fs.finish_write(volume, token)).unwrap();
    assert_eq!(run(fs.lookup(volume, 2, "Tools")).unwrap().metadata.catalog_id, folder);
    assert_eq!(run(fs.lookup(volume, folder, "Café")).unwrap().metadata.catalog_id, file);
    assert!(run(fs.begin_metadata(volume,
                                  2,
                                  token,
                                  MetadataPatch { comment: Some("volume root"),
                                                  ..Default::default() },
                                  metadata_when)).unwrap());
    run(fs.finish_write(volume, token)).unwrap();
    run(fs.unmount(volume)).unwrap();
    let next = volumes.insert(()).unwrap();
    run(fs.mount(next, 0)).unwrap();
    assert_eq!(fs.info(next).unwrap().metadata.comment, "volume root");
    assert_eq!(run(fs.lookup(next, 2, "Tools")).unwrap().metadata.comment, "native directory");
    assert_eq!(run(fs.lookup(next, folder, "Café")).unwrap().metadata.comment, "resource app");
    assert_eq!(run(fs.begin_metadata(next,
                                     u32::MAX,
                                     token,
                                     MetadataPatch { comment: Some("missing"),
                                                     ..Default::default() },
                                     metadata_when)).unwrap_err()
                                                    .cause,
               Cause::NotFound);
    run(fs.unmount(next)).unwrap();
}

#[test]
fn metadata_failure_and_cleanup_retry_preserve_actual_publication_and_source_snapshots()
{
    use mac_filesystem_interface::MetadataPatch;
    let temp = Temp::new();
    Volume::initialize(&temp.0, "Recovery").unwrap();
    std::fs::write(temp.0.join("raw"), b"unchanged").unwrap();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let node = run(fs.lookup(volume, 2, "raw")).unwrap().metadata.catalog_id;
    let when = fs.info(volume).unwrap().metadata.modified;
    let mut writes = common::Tokens::new();
    let token = writes.insert(()).unwrap();
    let patch = MetadataPatch { comment: Some("committed"),
                                ..Default::default() };
    assert!(run(fs.begin_metadata(volume, node, token, patch, when)).unwrap());
    fs.transport().fail_next = Some(Operation::Flush);
    assert_eq!(run(fs.finish_write(volume, token)).unwrap_err().cause,
               Cause::Host(mac_host_protocol::status::IO));
    assert_eq!(fs.cached_node(volume, node).unwrap().metadata.comment, "");
    assert!(!run(fs.abort_write(volume, token)).unwrap());
    assert!(run(fs.begin_metadata(volume, node, token, patch, when)).unwrap());
    let mut marks = Table::new(ObjectKind::Files);
    let mark = marks.insert(()).unwrap();
    run(fs.open(volume, node, u64::from(mark))).unwrap();
    fs.transport().fail_next = Some(Operation::Close);
    assert_eq!(run(fs.finish_write(volume, token)).unwrap_err().cause,
               Cause::Host(mac_host_protocol::status::IO));
    assert_eq!(fs.cached_node(volume, node).unwrap().metadata.comment, "committed");
    let commits = fs.transport().commits.len();
    run(fs.finish_write(volume, token)).unwrap();
    assert_eq!(fs.transport().commits.len(), commits, "cleanup replay republished metadata");
    assert_eq!(run(fs.read(volume, u64::from(mark), Fork::Data, 0, 9)).unwrap(), b"unchanged");
    run(fs.close_file(volume, u64::from(mark))).unwrap();
    assert_eq!(std::fs::read(temp.0.join("raw")).unwrap(), b"unchanged");
    run(fs.unmount(volume)).unwrap();
}

#[test]
fn plain_catalog_identity_is_durable_without_saving_finder_metadata()
{
    let temp = Temp::new();
    Volume::initialize(&temp.0, "Durable plain identities").unwrap();
    std::fs::write(temp.0.join("Original"), b"host bytes").unwrap();
    std::fs::create_dir(temp.0.join("Host Folder")).unwrap();
    let mut volumes = Table::new(ObjectKind::Volumes);
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let volume = volumes.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let modified = fs.info(volume).unwrap().metadata.modified;
    let file = run(fs.lookup(volume, 2, "Original")).unwrap().metadata.catalog_id;
    let folder = run(fs.lookup(volume, 2, "Host Folder")).unwrap().metadata.catalog_id;
    assert_ne!(file, folder);
    assert_eq!(fs.info(volume).unwrap().metadata.modified,
               modified,
               "identity bookkeeping must not change the volume's logical date");
    run(fs.unmount(volume)).unwrap();
    drop(fs);
    std::fs::rename(temp.0.join("Original"), temp.0.join("Renamed by host")).unwrap();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let next = volumes.insert(()).unwrap();
    run(fs.mount(next, 0)).unwrap();
    assert_eq!(run(fs.lookup(next, 2, "Renamed by host")).unwrap().metadata.catalog_id, file);
    assert_eq!(run(fs.lookup(next, 2, "Host Folder")).unwrap().metadata.catalog_id, folder);
    assert_eq!(std::fs::read(temp.0.join("Renamed by host")).unwrap(), b"host bytes");
    run(fs.unmount(next)).unwrap();
    drop(fs);
    // Keep the original incarnation pinned: recreating the path must get a new
    // identity even on filesystems that aggressively recycle inode numbers.
    let _old_pin = std::fs::File::open(temp.0.join("Renamed by host")).unwrap();
    std::fs::remove_file(temp.0.join("Renamed by host")).unwrap();
    std::fs::write(temp.0.join("Renamed by host"), b"a different file").unwrap();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let next = volumes.insert(()).unwrap();
    run(fs.mount(next, 0)).unwrap();
    let replacement = run(fs.lookup(next, 2, "Renamed by host")).unwrap().metadata.catalog_id;
    assert_ne!(replacement, file, "reusing a path must never reuse its old catalog identity");
    assert_eq!(fs.cached_node(next, file).unwrap_err().cause, Cause::NotFound);
    run(fs.unmount(next)).unwrap();
}

#[test]
fn failed_plain_identity_persistence_is_reported_and_can_retry()
{
    let temp = Temp::new();
    Volume::initialize(&temp.0, "Identity failure").unwrap();
    std::fs::write(temp.0.join("Raw"), b"untouched").unwrap();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    fs.transport().fail_next = Some(Operation::Flush);
    let error = run(fs.lookup(volume, 2, "Raw")).unwrap_err();
    assert_eq!(error.cause, Cause::Host(mac_host_protocol::status::IO));
    assert!(error.to_string().contains("Host"), "persistence cause must survive: {error}");
    let id = run(fs.lookup(volume, 2, "Raw")).unwrap().metadata.catalog_id;
    assert_eq!(std::fs::read(temp.0.join("Raw")).unwrap(), b"untouched");
    run(fs.unmount(volume)).unwrap();
    drop(fs);
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let next = volumes.insert(()).unwrap();
    run(fs.mount(next, 0)).unwrap();
    assert_eq!(run(fs.lookup(next, 2, "Raw")).unwrap().metadata.catalog_id, id);
    run(fs.unmount(next)).unwrap();
}

#[test]
fn imported_containers_and_copied_directory_markers_get_distinct_durable_catalog_ids()
{
    let (temp, original, _, _) = seeded();
    std::fs::copy(temp.0.join("Tools/Café"), temp.0.join("Copied File")).unwrap();
    std::fs::create_dir(temp.0.join("Copied Tools")).unwrap();
    std::fs::copy(temp.0.join("Tools").join(mac_fork_file::DIRECTORY_RECORD),
                  temp.0.join("Copied Tools").join(mac_fork_file::DIRECTORY_RECORD)).unwrap();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let folder = run(fs.lookup(volume, 2, "Tools")).unwrap().metadata.catalog_id;
    let copy_folder = run(fs.lookup(volume, 2, "Copied Tools")).unwrap().metadata.catalog_id;
    let copy = run(fs.lookup(volume, 2, "Copied File")).unwrap().metadata.catalog_id;
    assert_ne!(folder, copy_folder);
    assert_ne!(original, copy);
    assert_eq!(run(fs.lookup(volume, folder, "Café")).unwrap().metadata.catalog_id, original);
    let name = fs.cached_node(volume, copy).unwrap().metadata.name.clone();
    assert_eq!(name, "Copied File");
    run(fs.unmount(volume)).unwrap();
    drop(fs);
    std::fs::rename(temp.0.join("Copied Tools"), temp.0.join("Renamed Copy")).unwrap();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let next = volumes.insert(()).unwrap();
    run(fs.mount(next, 0)).unwrap();
    assert_eq!(run(fs.lookup(next, 2, "Renamed Copy")).unwrap().metadata.catalog_id, copy_folder);
    assert_eq!(run(fs.lookup(next, 2, "Copied File")).unwrap().metadata.catalog_id, copy);
    run(fs.unmount(next)).unwrap();
}

#[test]
fn removal_releases_owned_receipts_rejects_nonempty_folders_and_preserves_cancelled_sources()
{
    let (temp, file, _, _) = seeded();
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let folder = run(fs.lookup(volume, 2, "Tools")).unwrap().metadata.catalog_id;
    let mut writes = common::Tokens::new();
    let token = writes.insert(()).unwrap();
    let when = fs.info(volume).unwrap().metadata.modified + 100;
    let error = run(fs.begin_remove(volume, folder, token, when)).unwrap_err();
    assert_eq!(error.cause, Cause::NotEmpty);
    assert!(error.to_string().contains("NotEmpty"), "{error}");
    assert_eq!(run(fs.begin_remove(volume, 2, token, when)).unwrap_err().cause, Cause::Protected);
    run(fs.begin_remove(volume, file, token, when)).unwrap();
    assert!(!run(fs.abort_write(volume, token)).unwrap());
    assert!(temp.0.join("Tools/Café").is_file());
    run(fs.begin_remove(volume, file, token, when)).unwrap();
    fs.transport().fail_next = Some(Operation::CommitRemove);
    assert_eq!(run(fs.finish_write(volume, token)).unwrap_err().cause,
               Cause::Host(mac_host_protocol::status::IO));
    assert!(!run(fs.abort_write(volume, token)).unwrap());
    assert!(temp.0.join("Tools/Café").is_file());
    // Cancellation acknowledges the restored incarnation/revision. A second
    // operation must work immediately, without a caller-forced refresh.
    run(fs.lookup(volume, folder, "Café")).unwrap();
    run(fs.begin_remove(volume, file, token, when)).unwrap();
    run(fs.finish_write(volume, token)).unwrap();
    assert_eq!(fs.cached_node(volume, file).unwrap_err().cause, Cause::NotFound);
    assert!(!temp.0.join("Tools/Café").exists());
    run(fs.begin_remove(volume, folder, token, when)).unwrap();
    run(fs.finish_write(volume, token)).unwrap();
    assert!(!temp.0.join("Tools").exists());
    run(fs.unmount(volume)).unwrap();
}
