use std::{future::Future,
          path::PathBuf,
          pin::pin,
          sync::{Arc,
                 atomic::{AtomicBool, AtomicU64, Ordering}},
          task::{Context, Poll, Waker}};

use mac_fork_file::{Fork, Kind};
use mac_host_protocol::{Arguments, Operation};
use mac_object_table::{Kind as ObjectKind, Table};
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
    session:       Session,
    tickets:       Table<()>,
    calls:         Vec<Operation>,
    trace:         Vec<(Operation, u32, u64, usize)>,
    fail_next:     Option<Operation>,
    reached_child: Arc<AtomicBool>
}
impl Host
{
    fn new(temp: &Temp, writable: bool) -> Self
    {
        Self {
            session: Session::new(Arc::new(vec![Export::open("Share", &temp.0, writable).unwrap()])).unwrap(),
            tickets: Table::new(ObjectKind::DriverRequests),
            calls: Vec::new(), trace: Vec::new(), fail_next: None,
            reached_child: Arc::new(AtomicBool::new(false))
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
        self.calls.push(operation);
        self.trace.push((operation, arguments.object, arguments.offset, arguments.payload.len()));
        if self.fail_next == Some(operation)
        {
            self.fail_next = None;
            return Err(Cause::Host(mac_host_protocol::status::IO).into());
        }
        let ticket = self.tickets.insert(()).unwrap();
        let header = arguments.header(operation, ticket).unwrap();
        let (header, body) = self.session.respond(header, arguments.payload);
        if operation == Operation::ListNext && (body.starts_with(b"d-") || body.starts_with(b"f-"))
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
    let layout = run(filesystem.open(volume, cnid, first)).unwrap();
    assert_eq!(layout.data.length, data.len() as u64);
    assert_eq!(layout.resources.length, resource.len() as u64);
    assert_eq!(run(filesystem.read(volume, first, Fork::Data, 4095, 5000)).unwrap(),
               data[4095..9095]);
    assert_eq!(run(filesystem.read(volume, first, Fork::Resource, 0, 65536)).unwrap(), resource);
    assert_eq!(run(filesystem.unmount(volume)).unwrap_err().cause, Cause::Busy);
    run(filesystem.close_file(volume, first)).unwrap();
    files.remove(first);
    let next = files.insert(()).unwrap();
    assert_eq!(first & 0xffff, next & 0xffff);
    assert_ne!(first, next);
    run(filesystem.open(volume, cnid, next)).unwrap();
    assert_eq!(run(filesystem.read(volume, first, Fork::Data, 0, 1)).unwrap_err().cause,
               Cause::NotFound);
    assert_eq!(run(filesystem.close_file(volume, first)).unwrap_err().cause, Cause::NotFound);
    assert_eq!(run(filesystem.read(volume.wrapping_add(1), next, Fork::Data, 0, 1)).unwrap_err()
                                                                                   .cause,
               Cause::NotFound);
    run(filesystem.close_file(volume, next)).unwrap();
    run(filesystem.unmount(volume)).unwrap();
    assert_eq!(filesystem.info(volume).unwrap_err().cause, Cause::NotFound);
}
#[test]
fn corrupt_fork_is_rejected_before_returning_bytes_and_plain_host_files_are_rejected()
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
    run(filesystem.open(id, cnid, token)).unwrap();
    assert_eq!(run(filesystem.read(id, token, Fork::Data, 0, 64)).unwrap_err().cause,
               Cause::Format(mac_fork_file::Error::Corrupt));
    run(filesystem.close_file(id, token)).unwrap();
    run(filesystem.unmount(id)).unwrap();
    std::fs::write(temp.0.join("unwrapped.txt"), b"must be imported explicitly").unwrap();
    run(filesystem.mount(id, 0)).unwrap();
    assert!(run(filesystem.scan(id, 2)).is_err());
    run(filesystem.unmount(id)).unwrap();
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
    let mut writes = Table::new(ObjectKind::FileWrites);
    let token = writes.insert(()).unwrap();
    let data = vec![0x53; 9001];
    let resource = vec![0xa7; 4097];
    let when = 3_875_171_696_123_456;
    let id = run(filesystem.begin_create(Create { volume,
                                                  parent,
                                                  token,
                                                  name: "New Script",
                                                  data_length: data.len() as u64,
                                                  resource_length: resource.len() as u64,
                                                  when })).unwrap();
    assert_eq!(run(filesystem.finish_write(volume, token)).unwrap_err().cause,
               Cause::Format(mac_fork_file::Error::Incomplete));
    for bytes in data.chunks(mac_fork_file::CHUNK_BYTES)
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
                                           when: when + 1 })).unwrap();
    run(filesystem.write_chunk(volume, replacement, Fork::Data, b"new")).unwrap();
    run(filesystem.write_chunk(volume, replacement, Fork::Resource, b"rs")).unwrap();
    assert_eq!(run(filesystem.finish_write(volume, replacement)).unwrap(), id);
    assert_eq!(physical.resolve("/Tools/New Script").unwrap().metadata.catalog_id, id);
    let mut opens = Table::new(ObjectKind::Files);
    let open = opens.insert(()).unwrap();
    run(filesystem.open(volume, id, open)).unwrap();
    assert_eq!(run(filesystem.read(volume, open, Fork::Data, 0, 30)).unwrap(), b"new");
    assert_eq!(run(filesystem.read(volume, open, Fork::Resource, 0, 30)).unwrap(), b"rs");
    run(filesystem.close_file(volume, open)).unwrap();
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
    let mut writes = Table::new(ObjectKind::FileWrites);
    let first = writes.insert(()).unwrap();
    let reserved = run(filesystem.begin_create(Create { volume,
                                                        parent,
                                                        token: first,
                                                        name: "Cancelled",
                                                        data_length: 20,
                                                        resource_length: 0,
                                                        when: 10 })).unwrap();
    assert_eq!(run(filesystem.unmount(volume)).unwrap_err().cause, Cause::Busy);
    assert!(!run(filesystem.abort_write(volume, first)).unwrap());
    writes.remove(first);
    assert_eq!(run(filesystem.lookup(volume, parent, "Cancelled")).unwrap_err().cause,
               Cause::NotFound);
    let next = writes.insert(()).unwrap();
    assert_eq!(run(filesystem.write_chunk(volume, first, Fork::Data, b"stale")).unwrap_err().cause,
               Cause::NotFound);
    let fresh = run(filesystem.begin_create(Create { volume,
                                                     parent,
                                                     token: next,
                                                     name: "Cancelled",
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
    let mut writes = Table::new(ObjectKind::FileWrites);
    let token = writes.insert(()).unwrap();
    let id = run(fs.begin_create(Create { volume,
                                          parent,
                                          token,
                                          name: "Flush Retry",
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
    let mut writes = Table::new(ObjectKind::FileWrites);
    let token = writes.insert(()).unwrap();
    let id = run(fs.begin_create(Create { volume,
                                          parent,
                                          token,
                                          name: "Published",
                                          data_length: 0,
                                          resource_length: 0,
                                          when: 30 })).unwrap();
    fs.transport().fail_next = Some(Operation::Close);
    assert_eq!(run(fs.finish_write(volume, token)).unwrap_err().cause,
               Cause::Host(mac_host_protocol::status::IO));
    let commits = fs.transport().calls.iter().filter(|&&op| op == Operation::Commit).count();
    run(fs.cleanup_orphans()).unwrap();
    assert_eq!(run(fs.finish_write(volume, token)).unwrap(), id);
    assert_eq!(fs.transport().calls.iter().filter(|&&op| op == Operation::Commit).count(), commits);
    assert_eq!(fs.cached_node(volume, parent).unwrap().metadata.modified, 30);
    run(fs.unmount(volume)).unwrap();
    let physical = Volume::open(&temp.0).unwrap();
    assert_eq!(physical.resolve("/Tools/Published").unwrap().metadata.catalog_id, id);
    assert_eq!(physical.resolve("/Tools").unwrap().metadata.modified, 30);
    physical.check().unwrap();
}

#[test]
fn casefold_collisions_invalid_names_and_read_only_writes_do_not_reserve_catalog_ids()
{
    use vhost_filesystem::Create;
    let (temp, _, _, _) = seeded();
    let physical = Volume::open(&temp.0).unwrap();
    let initial = physical.root().unwrap().metadata.next_catalog_id;
    let mut fs = Filesystem::new(Host::new(&temp, true));
    let mut volumes = Table::new(ObjectKind::Volumes);
    let volume = volumes.insert(()).unwrap();
    run(fs.mount(volume, 0)).unwrap();
    let parent = run(fs.lookup(volume, 2, "Tools")).unwrap().metadata.catalog_id;
    let mut writes = Table::new(ObjectKind::FileWrites);
    let token = writes.insert(()).unwrap();
    for name in ["CAFÉ", "cafe\u{301}"]
    {
        assert_eq!(run(fs.begin_create(Create { volume,
                                                parent,
                                                token,
                                                name,
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
                                            name: &long,
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
                                            name: "Read Only",
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
    run(fs.open(volume, cnid, token)).unwrap();
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
    assert_eq!(run(fs.read(volume, token, Fork::Data, 0, 31)).unwrap(), data[..31]);
    run(fs.close_file(volume, token)).unwrap();
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
    assert_eq!(mounted.capabilities, fs::READ | fs::WRITE);
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
    let open = A { token,
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
    let mut writes = Table::new(ObjectKind::FileWrites);
    let token = writes.insert(()).unwrap();
    let write = A { token,
                    ..args };
    let id = serve(&mut filesystem,
                   O::BeginCreate,
                   A { parent: 2,
                       length: 3,
                       auxiliary: 2,
                       when: 3_875_171_696_123_456,
                       payload: b"Copied",
                       ..write }).unwrap();
    let cnid = u32::from_le_bytes(id.try_into().unwrap());
    serve(&mut filesystem,
          O::WriteChunk,
          A { payload: b"abc",
              ..write }).unwrap();
    serve(&mut filesystem,
          O::WriteChunk,
          A { flags: fs::RESOURCE_FORK,
              payload: b"rs",
              ..write }).unwrap();
    assert_eq!(serve(&mut filesystem, O::FinishWrite, write).unwrap(), cnid.to_le_bytes());
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
                         flags: 1,
                         ..args }).unwrap_err()
                                  .cause,
               Cause::Invalid);
    assert_eq!(filesystem.transport().calls.len(), before, "invalid wire fields reached host I/O");
    let mut writes = Table::new(ObjectKind::FileWrites);
    let token = writes.insert(()).unwrap();
    let before = Volume::open(&temp.0).unwrap().root().unwrap().metadata;
    assert_eq!(serve(&mut filesystem,
                     O::BeginCreate,
                     A { parent: 2,
                         token,
                         payload: b"No write",
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
                   1,
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
