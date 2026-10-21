use std::{fs,
          io::{Read, Write},
          net::{TcpListener, TcpStream},
          path::PathBuf,
          sync::Arc};

use mac_host_protocol::{self as wire, Header, Operation as Op, status};
use risc_host::{Export, Session};
#[test]
fn one_session_companion_exits_when_its_vm_disconnects()
{
    use std::{io::{BufRead, BufReader},
              process::{Command, Stdio},
              time::{Duration, Instant}};
    let temp = Temp::new();
    let mut child =
        Command::new(env!("CARGO_BIN_EXE_risc-host")).args(["--one-session", "--read-only-export"])
                                                     .arg(format!("Test={}", temp.0.display()))
                                                     .stdout(Stdio::piped())
                                                     .spawn()
                                                     .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let address = line.trim().strip_prefix("HOST_BRIDGE_READY ").unwrap();
    let stream = TcpStream::connect(address).unwrap();
    drop(stream);
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop
    {
        if let Some(status) = child.try_wait().unwrap()
        {
            break status;
        }
        if Instant::now() >= deadline
        {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("companion retained its listener after VM disconnect");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "{status}");
}

struct Temp(PathBuf);
impl Temp
{
    fn new() -> Self
    {
        let mut id = [0; 8];
        getrandom::fill(&mut id).unwrap();
        let path = std::env::temp_dir().join(format!("risc-host-{}-{:x}",
                                                     std::process::id(),
                                                     u64::from_le_bytes(id)));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp
{
    fn drop(&mut self)
    {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Client
{
    session: Session,
    id:      u32,
    root:    u32
}
impl Client
{
    fn new(exports: Arc<Vec<Export>>) -> Self
    {
        let mut session = Session::new(exports).unwrap();
        let (hello, _) = session.respond(Header::request(Op::Hello, 1, 0, 0), &[]);
        assert_eq!(hello.status, status::OK);
        assert_ne!(hello.session, 0);
        let id = hello.session;
        let (export, _) = session.respond(Header::request(Op::Export, 1, id, 0), &[]);
        assert_eq!(export.status, status::OK);
        Self { session,
               id,
               root: export.object }
    }

    fn call(&mut self, operation: Op, object: u32, offset: u64, value: u32, bytes: &[u8])
            -> (Header, Vec<u8>)
    {
        let mut request = Header::request(operation, 1, self.id, object);
        request.offset = offset;
        request.value = value;
        request.payload = bytes.len() as u32;
        self.session.respond(request, bytes)
    }

    fn ok(&mut self, operation: Op, object: u32, offset: u64, value: u32, bytes: &[u8])
          -> (Header, Vec<u8>)
    {
        let reply = self.call(operation, object, offset, value, bytes);
        assert_eq!(reply.0.status, status::OK, "{operation:?}");
        reply
    }

    fn stage(&mut self, bytes: &[u8]) -> u32
    {
        let (stage, _) = self.ok(Op::BeginWrite, self.root, 0, 0, &[]);
        self.ok(Op::Write, stage.object, 0, 0, bytes);
        stage.object
    }
}
fn exports(temp: &Temp, writable: bool) -> Arc<Vec<Export>>
{
    Arc::new(vec![Export::open("Share", &temp.0, writable).unwrap()])
}
#[test]
fn staged_write_flush_replace_and_remove_reach_the_real_host_filesystem()
{
    let temp = Temp::new();
    let mut client = Client::new(exports(&temp, true));
    let stage = client.stage(b"both forks are opaque bytes at this layer");
    assert!(!temp.0.join("container").exists());
    client.ok(Op::Flush, stage, 0, 0, &[]);
    client.ok(Op::Flush, stage, 0, 0, &[]);
    client.ok(Op::Commit, stage, 0, 0, b"container");
    assert_eq!(fs::read(temp.0.join("container")).unwrap(),
               b"both forks are opaque bytes at this layer");
    assert_eq!(client.call(Op::Write, stage, 0, 0, b"stale").0.status, status::STALE);
    let (source, revision) = client.ok(Op::OpenRead, client.root, 0, 0, b"container");
    assert_eq!(revision.len(), 32);
    assert_eq!(client.ok(Op::Read, source.object, 5, 5, &[]).1, b"forks");
    assert!(client.ok(Op::Read, source.object, u64::MAX, 20, &[]).1.is_empty());
    let replacement = client.stage(b"new container revision");
    assert_eq!(client.call(Op::Commit, replacement, 0, 0, b"container").0.status, status::CONFLICT);
    assert_eq!(client.ok(Op::Read, source.object, 0, 4, &[]).1, b"both");
    client.ok(Op::Commit, replacement, 0, source.object, b"container");
    assert_eq!(fs::read(temp.0.join("container")).unwrap(), b"new container revision");
    // The old inode may be retained, but its changed ctime must not be silently
    // confused with the new catalog revision.
    assert_eq!(client.call(Op::Read, source.object, 0, 4, &[]).0.status, status::STALE);
    let (source, _) = client.ok(Op::OpenRead, client.root, 0, 0, b"container");
    client.ok(Op::Remove, client.root, 0, source.object, b"container");
    assert!(!temp.0.join("container").exists());
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 0);
}
#[test]
fn sparse_positional_io_preserves_offsets_beyond_four_gibibytes()
{
    let temp = Temp::new();
    let mut client = Client::new(exports(&temp, true));
    let stage = client.stage(b"start");
    let offset = 5 * 1024 * 1024 * 1024u64;
    client.ok(Op::SetLength, stage, offset + 4, 0, &[]);
    client.ok(Op::Write, stage, offset, 0, b"tail");
    client.ok(Op::Commit, stage, 0, 0, b"large");
    let (source, _) = client.ok(Op::OpenRead, client.root, 0, 0, b"large");
    assert_eq!(source.offset, offset + 4);
    assert_eq!(client.ok(Op::Read, source.object, offset - 4, 8, &[]).1, b"\0\0\0\0tail");
}
#[test]
fn stale_and_foreign_ids_cannot_alias_reused_files_directories_or_sessions()
{
    let temp = Temp::new();
    fs::write(temp.0.join("file"), b"content").unwrap();
    let exports = exports(&temp, true);
    let mut a = Client::new(exports.clone());
    let mut b = Client::new(exports.clone());
    let (first, _) = a.ok(Op::OpenRead, a.root, 0, 0, b"file");
    assert_eq!(b.call(Op::Read, first.object, 0, 7, &[]).0.status, status::STALE);
    a.ok(Op::Close, first.object, 0, 0, &[]);
    let (second, _) = a.ok(Op::OpenRead, a.root, 0, 0, b"file");
    assert_eq!(first.object as u16, second.object as u16);
    assert_ne!(first.object, second.object);
    assert_eq!(a.call(Op::Read, first.object, 0, 7, &[]).0.status, status::STALE);
    assert_eq!(a.call(Op::Read, a.root, 0, 7, &[]).0.status, status::STALE);
    let root = a.root;
    a.ok(Op::Close, root, 0, 0, &[]);
    let (new_root, _) = a.ok(Op::Export, 0, 0, 0, &[]);
    assert_eq!(new_root.object as u16, root as u16);
    assert_ne!(root, new_root.object);
    assert_eq!(a.call(Op::ListBegin, root, 0, 0, &[]).0.status, status::STALE);
    let old = b.id;
    drop(b);
    let mut replacement = Client::new(exports);
    assert_ne!(replacement.id, old);
    let bad = Header::request(Op::Export, 1, old, 0);
    assert_eq!(replacement.session.respond(bad, &[]).0.status, status::STALE);
}
#[test]
fn cancellation_and_disconnect_remove_unpublished_stages()
{
    let temp = Temp::new();
    let export = exports(&temp, true);
    {
        let mut client = Client::new(export);
        let stage = client.stage(b"cancel me");
        client.ok(Op::Close, stage, 0, 0, &[]);
        assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 0);
        client.stage(b"disconnect me");
        assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 1);
    }
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 0);
}
#[test]
fn read_only_exports_reject_every_mutation_and_exclusive_lock()
{
    let temp = Temp::new();
    fs::write(temp.0.join("lock"), []).unwrap();
    let mut client = Client::new(exports(&temp, false));
    for operation in [Op::BeginWrite, Op::MakeDirectory, Op::Remove, Op::SyncDirectory]
    {
        let bytes = if matches!(operation, Op::MakeDirectory | Op::Remove)
        {
            b"file".as_slice()
        }
        else
        {
            &[]
        };
        assert_eq!(client.call(operation, client.root, 0, 0, bytes).0.status, status::READ_ONLY);
    }
    let (lock, _) = client.ok(Op::OpenLock, client.root, 0, 0, b"lock");
    assert_eq!(client.call(Op::Lock, lock.object, 0, 1, &[]).0.status, status::READ_ONLY);
    client.ok(Op::Lock, lock.object, 0, 0, &[]);
    client.ok(Op::Unlock, lock.object, 0, 0, &[]);
}
#[test]
fn renames_never_replace_an_existing_object_and_open_directory_caps_survive_moves()
{
    let temp = Temp::new();
    let mut client = Client::new(exports(&temp, true));
    client.ok(Op::MakeDirectory, client.root, 0, 0, b"folder");
    let (directory, _) = client.ok(Op::OpenDirectory, client.root, 0, 0, b"folder");
    let names = b"\x06\x00foldermoved";
    client.ok(Op::Rename, client.root, client.root as u64, directory.object, names);
    assert!(!temp.0.join("folder").exists());
    assert!(temp.0.join("moved").is_dir());
    client.ok(Op::MakeDirectory, directory.object, 0, 0, b"inside");
    assert!(temp.0.join("moved/inside").is_dir());
    fs::write(temp.0.join("file"), b"data").unwrap();
    fs::write(temp.0.join("occupied"), b"keep").unwrap();
    let (source, _) = client.ok(Op::OpenRead, client.root, 0, 0, b"file");
    assert_eq!(client.call(Op::Rename,
                           client.root,
                           client.root as u64,
                           source.object,
                           b"\x04\x00fileoccupied")
                     .0
                     .status,
               status::CONFLICT);
    client.ok(Op::Rename, client.root, client.root as u64, source.object, b"\x04\x00filerenamed");
    assert_eq!(fs::read(temp.0.join("renamed")).unwrap(), b"data");
    assert_eq!(fs::read(temp.0.join("occupied")).unwrap(), b"keep");
}
#[test]
fn locks_are_nonblocking_and_external_changes_invalidate_snapshots_and_enumerations()
{
    let temp = Temp::new();
    fs::write(temp.0.join("lock"), []).unwrap();
    fs::write(temp.0.join("file"), b"data").unwrap();
    let export = exports(&temp, true);
    let mut a = Client::new(export.clone());
    let mut b = Client::new(export);
    let (lock_a, _) = a.ok(Op::OpenLock, a.root, 0, 0, b"lock");
    let (lock_b, _) = b.ok(Op::OpenLock, b.root, 0, 0, b"lock");
    a.ok(Op::Lock, lock_a.object, 0, 1, &[]);
    assert_eq!(b.call(Op::Lock, lock_b.object, 0, 1, &[]).0.status, status::BUSY);
    a.ok(Op::Unlock, lock_a.object, 0, 0, &[]);
    b.ok(Op::Lock, lock_b.object, 0, 1, &[]);
    let (source, _) = a.ok(Op::OpenRead, a.root, 0, 0, b"file");
    fs::write(temp.0.join("file"), b"edited externally").unwrap();
    assert_eq!(a.call(Op::Read, source.object, 0, 4, &[]).0.status, status::STALE);
    let (enumeration, _) = a.ok(Op::ListBegin, a.root, 0, 0, &[]);
    fs::write(temp.0.join("added"), b"new").unwrap();
    assert_eq!(a.call(Op::ListNext, enumeration.object, 0, 0, &[]).0.status, status::STALE);
}
#[test]
#[cfg(unix)]
fn symlink_paths_and_traversals_cannot_escape_an_export()
{
    let temp = Temp::new();
    let outside = Temp::new();
    fs::write(outside.0.join("private"), b"secret").unwrap();
    std::os::unix::fs::symlink(&outside.0, temp.0.join("link")).unwrap();
    let mut client = Client::new(exports(&temp, true));
    for name in [b"../private".as_slice(), b"/private", b"link/private", b"C:drive"]
    {
        assert_ne!(client.call(Op::OpenRead, client.root, 0, 0, name).0.status, status::OK);
    }
    assert_eq!(client.call(Op::OpenDirectory, client.root, 0, 0, b"link").0.status,
               status::INVALID);
    assert_eq!(fs::read(outside.0.join("private")).unwrap(), b"secret");
}
#[test]
fn real_tcp_connection_accepts_fragmented_frames_and_cleans_up_at_eof()
{
    let temp = Temp::new();
    fs::write(temp.0.join("file"), b"tcp data").unwrap();
    let export = exports(&temp, true);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        risc_host::serve_connection(&mut stream, export).unwrap();
    });
    let mut stream = TcpStream::connect(address).unwrap();
    stream.set_nodelay(true).unwrap();
    let exchange = |stream: &mut TcpStream, header: Header, payload: &[u8]| {
        for byte in header.encode().unwrap()
        {
            stream.write_all(&[byte]).unwrap();
        }
        stream.write_all(payload).unwrap();
        let mut bytes = [0; wire::HEADER_BYTES];
        stream.read_exact(&mut bytes).unwrap();
        let reply = Header::decode(&bytes).unwrap();
        let mut payload = vec![0; reply.payload as usize];
        stream.read_exact(&mut payload).unwrap();
        assert_eq!(reply.status, status::OK);
        (reply, payload)
    };
    let (hello, _) = exchange(&mut stream, Header::request(Op::Hello, 1, 0, 0), &[]);
    let (root, _) = exchange(&mut stream, Header::request(Op::Export, 2, hello.session, 0), &[]);
    let (stage, _) =
        exchange(&mut stream, Header::request(Op::BeginWrite, 3, hello.session, root.object), &[]);
    assert_ne!(stage.object, 0);
    drop(stream);
    server.join().unwrap();
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 1);
}

#[test]
fn host_files_with_pre_unix_dates_remain_readable()
{
    let temp = Temp::new();
    let path = temp.0.join("old");
    fs::write(&path, b"old file").unwrap();
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH
                                                     - std::time::Duration::from_secs(60)))
        .unwrap();
    let mut client = Client::new(exports(&temp, false));
    let (source, _) = client.ok(Op::OpenRead, client.root, 0, 0, b"old");
    assert_eq!(client.ok(Op::Read, source.object, 0, 8, &[]).1, b"old file");
}
