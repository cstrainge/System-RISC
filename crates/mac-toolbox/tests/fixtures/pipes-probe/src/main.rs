//! Guest execution probe for pipes. It replaces Finder on a cloned disk so the
//! System runs it at boot; every case logs a serial marker and the harness
//! accepts only `PIPES_PROBE_PASS`.
#![no_std]
#![no_main]
use core::{fmt::{self, Write},
           sync::atomic::{AtomicBool, AtomicU64, Ordering}};

use mac_toolbox::{Error, Rect, System, abi::pipes as wire, files};

static ARMED: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);
/// Reader handle created by the main thread and used by the parked reader
/// thread. Endpoint ownership is per process, so the same application's other
/// thread may read through it.
static READER_HANDLE: AtomicU64 = AtomicU64::new(0);
/// Count transferred to the parked reader, and its first eight bytes packed
/// little-endian, so the main thread can check what the parked read received.
static RECEIVED: AtomicU64 = AtomicU64::new(0);
static FIRST_BYTES: AtomicU64 = AtomicU64::new(0);
const NAME: &str = "ProbePipe";
const PIPE: &str = "FolderPipe";
const PIPE2: &str = "FolderPipe2";
const PAYLOAD: [u8; 512] = [0xab; 512];

struct Line
{
    bytes:  [u8; 192],
    length: usize
}
impl Line
{
    fn new() -> Self
    {
        Self { bytes:  [0; 192],
               length: 0 }
    }

    fn text(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.length]).unwrap_or("PIPES_PROBE_TEXT_ERROR\n")
    }
}
impl Write for Line
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        let remaining = self.bytes.len() - self.length;
        let count = text.len().min(remaining);
        self.bytes[self.length..self.length + count].copy_from_slice(&text.as_bytes()[..count]);
        self.length += count;
        if count != text.len() { Err(fmt::Error) } else { Ok(()) }
    }
}

fn note(system: &System<'_>, text: &str)
{
    let _ = system.log(text);
}

fn check(condition: bool, cause: &str) -> Result<(), Line>
{
    if condition
    {
        return Ok(());
    }
    let mut line = Line::new();
    let _ = writeln!(line, "PIPES_PROBE_FAIL {cause}");
    Err(line)
}

/// Run every case in order and report how many actually ran, so the marker
/// cannot claim a count the run did not reach.
fn run(system: &System<'_>) -> Result<u32, Line>
{
    let cases: [fn(&System<'_>) -> Result<(), Line>; 11] = [echo, block, reader_park, eof, broken,
                                                            named, writer_limit, folder, scan,
                                                            icons, pipe_open];
    let mut completed = 0;
    for case in cases
    {
        case(system)?;
        completed += 1;
    }
    Ok(completed)
}

fn echo(system: &System<'_>) -> Result<(), Line>
{
    let (mut reader, mut writer) =
        system.pipes().create(wire::MIN_CAPACITY).map_err(|error| failed("create", error))?;
    let sent = [0x11u8, 0x22, 0x33, 0x44, 0x55];
    check(writer.write(&sent).map_err(|error| failed("echo write", error))? == sent.len(),
          "echo write was short")?;
    let mut back = [0u8; 5];
    check(reader.read(&mut back).map_err(|error| failed("echo read", error))? == sent.len(),
          "echo read was short")?;
    check(back == sent, "echo changed the bytes")?;
    note(system, "PIPES_PROBE_ECHO_OK bytes=5\n");
    Ok(())
}

unsafe extern "C" fn blocked_writer(_: *mut u8)
{
    // The platform runtime installed this process's client before entry, so a
    // native application thread obtains the same routing view without a table.
    let Ok(system) = System::new()
    else
    {
        return;
    };
    let Ok(mut writer) = system.pipes().connect(NAME)
    else
    {
        note(&system, "PIPES_PROBE_CONNECT_REFUSED\n");
        return;
    };
    ARMED.store(true, Ordering::Release);
    note(&system, "PIPES_PROBE_WRITER_ARMED\n");
    match writer.write(&PAYLOAD)
    {
        Ok(count) =>
        {
            let mut line = Line::new();
            let _ = writeln!(line, "PIPES_PROBE_WRITER_RESUMED transferred={count}");
            note(&system, line.text());
            note(&system, "\n");
        }
        Err(error) =>
        {
            let mut line = Line::new();
            let _ = writeln!(line, "PIPES_PROBE_WRITER_REFUSED {error:?}");
            note(&system, line.text());
            note(&system, "\n");
        }
    }
    DONE.store(true, Ordering::Release);
}

fn failed(stage: &'static str, error: Error) -> Line
{
    let mut line = Line::new();
    let _ = writeln!(line, "PIPES_PROBE_FAIL {stage} {error:?}");
    line
}

fn fault(text: &'static str) -> Line
{
    let mut line = Line::new();
    let _ = write!(line, "PIPES_PROBE_FAIL {text}");
    line
}

/// A File Manager mutation inside the Pipes folder, or on a virtual item, must
/// be refused with a named status rather than reaching a volume or the
/// registry. Anything else is a failure of the merge, including success.
fn refusal<T>(stage: &'static str, result: core::result::Result<T, Error>) -> Result<(), Line>
{
    match result
    {
        Err(Error::NotSupported) => Ok(()),
        Err(error) => Err(failed(stage, error)),
        Ok(_) => Err(fault(stage))
    }
}

fn block(system: &System<'_>) -> Result<(), Line>
{
    let (mut reader, mut writer) = system.pipes()
                                         .create_named(NAME, wire::MIN_CAPACITY, 0)
                                         .map_err(|error| failed("create_named", error))?;
    let fill = [0x55u8; 512];
    check(writer.write(&fill).map_err(|error| failed("fill write", error))? == fill.len(),
          "fill write was short")?;
    ARMED.store(false, Ordering::Release);
    DONE.store(false, Ordering::Release);
    // SAFETY: the entry uses the loader table, takes no caller pointer, and the
    // join right stays on this creating application thread.
    let right = match unsafe {
              system.client().create_native_thread(blocked_writer, core::ptr::null_mut(), 64 * 1024)
          }
    {
        Ok(right) => right,
        Err(failure) =>
        {
            let mut line = Line::new();
            let _ = writeln!(line, "PIPES_PROBE_FAIL thread {failure}");
            return Err(line);
        }
    };
    let mut spins = 0u32;
    while !ARMED.load(Ordering::Acquire)
    {
        spins = spins.saturating_add(1);
        if spins > 100_000_000
        {
            let mut line = Line::new();
            let _ = writeln!(line, "PIPES_PROBE_FAIL writer never armed");
            return Err(line);
        }
        core::hint::spin_loop();
    }
    check(!DONE.load(Ordering::Acquire), "atomic write returned without space")?;
    note(system, "PIPES_PROBE_WRITER_PARKED\n");
    let mut sink = [0u8; 512];
    check(reader.read(&mut sink).map_err(|error| failed("drain", error))? == fill.len(),
          "drain read was short")?;
    check(sink == fill, "drain changed the bytes")?;
    note(system, "PIPES_PROBE_MAKE_ROOM\n");
    match right.join()
    {
        Ok(_) => (),
        Err(_) =>
        {
            let mut line = Line::new();
            let _ = writeln!(line, "PIPES_PROBE_FAIL worker join refused");
            return Err(line);
        }
    }
    check(DONE.load(Ordering::Acquire), "worker did not resume")?;
    let mut resumed = [0u8; 512];
    check(reader.read(&mut resumed).map_err(|error| failed("resumed", error))? == PAYLOAD.len(),
          "resumed read was short")?;
    check(resumed == PAYLOAD, "resumed bytes were wrong")?;
    drop(writer);
    let mut tail = [0u8; 1];
    check(reader.read(&mut tail).map_err(|error| failed("eof", error))? == 0,
          "no end of stream after the last writer")?;
    note(system, "PIPES_PROBE_BLOCK_OK parked=1 resumed=1 eof=1\n");
    Ok(())
}

/// A reader thread parks on an empty pipe that still has a writer, and is
/// resumed by the writer. This is the direction the writer-park case does not
/// cover: the suspension is on the read, and the wake is a write.
///
/// What these markers prove, and what they do not. They prove the read did not
/// return before a byte was written, that it returned all five bytes after the
/// write, and that the creating thread ran in between. They do **not** prove the
/// reader was descheduled while it waited: a read that spun would produce the
/// same sequence. Distinguishing the two needs the scheduler's own wait state
/// for a thread, which the SDK does not expose and which this repository will
/// not grow production introspection for the sake of one probe. Read the marker
/// as "the read waited for a writer", not as "the thread was parked".
unsafe extern "C" fn parked_reader(_: *mut u8)
{
    let Ok(system) = System::new()
    else
    {
        return;
    };
    let handle = READER_HANDLE.load(Ordering::Acquire);
    let Ok(mut reader) = system.pipes().adopt_reader(handle)
    else
    {
        note(&system, "PIPES_PROBE_READER_REFUSED\n");
        DONE.store(true, Ordering::Release);
        return;
    };
    ARMED.store(true, Ordering::Release);
    note(&system, "PIPES_PROBE_READER_ARMED\n");
    let mut buffer = [0u8; 8];
    match reader.read(&mut buffer)
    {
        Ok(count) =>
        {
            RECEIVED.store(count as u64, Ordering::Release);
            let mut packed = 0u64;
            for (index, byte) in buffer.iter().take(count.min(8)).enumerate()
            {
                packed |= u64::from(*byte) << (index * 8);
            }
            FIRST_BYTES.store(packed, Ordering::Release);
            let mut line = Line::new();
            let _ = writeln!(line, "PIPES_PROBE_READER_RESUMED transferred={count}");
            note(&system, line.text());
            note(&system, "\n");
        }
        Err(error) =>
        {
            let mut line = Line::new();
            let _ = writeln!(line, "PIPES_PROBE_READER_READ_REFUSED {error:?}");
            note(&system, line.text());
            note(&system, "\n");
        }
    }
    DONE.store(true, Ordering::Release);
}

fn reader_park(system: &System<'_>) -> Result<(), Line>
{
    let (reader, mut writer) = system.pipes()
                                     .create_named("ProbeRead", wire::MIN_CAPACITY, 0)
                                     .map_err(|error| failed("create reader pipe", error))?;
    READER_HANDLE.store(reader.raw(), Ordering::Release);
    ARMED.store(false, Ordering::Release);
    DONE.store(false, Ordering::Release);
    RECEIVED.store(0, Ordering::Release);
    FIRST_BYTES.store(0, Ordering::Release);
    // SAFETY: the entry takes no caller pointer, and the join right stays on
    // this creating application thread.
    let right = match unsafe {
              system.client().create_native_thread(parked_reader, core::ptr::null_mut(), 64 * 1024)
          }
    {
        Ok(right) => right,
        Err(failure) =>
        {
            let mut line = Line::new();
            let _ = writeln!(line, "PIPES_PROBE_FAIL reader thread {failure}");
            return Err(line);
        }
    };
    // The reader thread now owns the reader end through the same handle; this
    // copy must not close it while that thread is parked on it.
    core::mem::forget(reader);
    let mut spins = 0u32;
    while !ARMED.load(Ordering::Acquire)
    {
        spins = spins.saturating_add(1);
        if spins > 100_000_000
        {
            let mut line = Line::new();
            let _ = writeln!(line, "PIPES_PROBE_FAIL reader never armed");
            return Err(line);
        }
        core::hint::spin_loop();
    }
    // Let the scheduler run the reader so it reaches its wait, rather than
    // writing while it is still on its way there. An idle wait returns after
    // one tick with no event required.
    let _ = system.events().wait_event(mac_toolbox::EventMask::ALL, 2);
    check(!DONE.load(Ordering::Acquire), "the empty pipe read returned early")?;
    note(system, "PIPES_PROBE_READER_PARKED\n");
    let sent = [0x5au8, 0x5b, 0x5c, 0x5d, 0x5e];
    check(writer.write(&sent).map_err(|error| failed("wake write", error))? == sent.len(),
          "the waking write was short")?;
    note(system, "PIPES_PROBE_READER_WROTE\n");
    match right.join()
    {
        Ok(_) => (),
        Err(_) =>
        {
            let mut line = Line::new();
            let _ = writeln!(line, "PIPES_PROBE_FAIL reader join refused");
            return Err(line);
        }
    }
    check(DONE.load(Ordering::Acquire), "the parked reader never resumed")?;
    check(RECEIVED.load(Ordering::Acquire) == sent.len() as u64,
          "the resumed read transferred the wrong count")?;
    let mut expected = 0u64;
    for (index, byte) in sent.iter().enumerate()
    {
        expected |= u64::from(*byte) << (index * 8);
    }
    check(FIRST_BYTES.load(Ordering::Acquire) == expected,
          "the resumed read received different bytes")?;
    note(system, "PIPES_PROBE_READER_OK parked=1 resumed=1 bytes=5\n");
    drop(writer);
    Ok(())
}

fn eof(system: &System<'_>) -> Result<(), Line>
{
    let (mut reader, writer) =
        system.pipes().create(wire::MIN_CAPACITY).map_err(|error| failed("create eof", error))?;
    reader.set_blocking(false).map_err(|error| failed("nonblocking", error))?;
    let mut sink = [0u8; 1];
    check(matches!(reader.read(&mut sink), Err(Error::WouldBlock)),
          "empty pipe with a live writer did not report WouldBlock")?;
    note(system, "PIPES_PROBE_WOULD_BLOCK_OK\n");
    drop(writer);
    check(reader.read(&mut sink).map_err(|error| failed("eof read", error))? == 0,
          "no end of stream after the last writer")?;
    note(system, "PIPES_PROBE_EOF_OK\n");
    Ok(())
}

fn broken(system: &System<'_>) -> Result<(), Line>
{
    let (reader, mut writer) = system.pipes()
                                     .create(wire::MIN_CAPACITY)
                                     .map_err(|error| failed("create broken", error))?;
    drop(reader);
    check(matches!(writer.write(b"x"), Err(Error::BrokenPipe)),
          "write without readers did not report BrokenPipe")?;
    note(system, "PIPES_PROBE_BROKEN_OK\n");
    Ok(())
}

fn named(system: &System<'_>) -> Result<(), Line>
{
    {
        let (reader, writer) = system.pipes()
                                     .create_named(NAME, wire::MIN_CAPACITY, 0)
                                     .map_err(|error| failed("create named", error))?;
        let connected = system.pipes().connect(NAME).map_err(|error| failed("connect", error))?;
        drop(connected);
        drop(writer);
        drop(reader);
    }
    check(matches!(system.pipes().connect(NAME), Err(Error::NotFound)),
          "a name survived its last handle")?;
    note(system, "PIPES_PROBE_NAME_OK\n");
    Ok(())
}

/// A name that declares one writer has exactly one connecting owner: the second
/// application is refused at the connection rather than joined to the first
/// one's stream, and the slot is free again when that owner closes.
fn writer_limit(system: &System<'_>) -> Result<(), Line>
{
    const OWNED: &str = "OwnedPipe";
    let (reader, writer) = system.pipes()
                                 .create_named(OWNED, wire::MIN_CAPACITY, 1)
                                 .map_err(|error| failed("create exclusive", error))?;
    let owner = system.pipes().connect(OWNED).map_err(|error| failed("first connect", error))?;
    note(system, "PIPES_WRITER_FIRST_OK\n");
    check(matches!(system.pipes().connect(OWNED), Err(Error::AccessDenied)),
          "a second connect was not refused")?;
    note(system, "PIPES_WRITER_SECOND_REFUSED\n");
    drop(owner);
    let again =
        system.pipes().connect(OWNED).map_err(|error| failed("connect after release", error))?;
    note(system, "PIPES_WRITER_RELEASE_OK\n");
    drop(again);
    drop(writer);
    drop(reader);
    check(matches!(system.pipes().connect(OWNED), Err(Error::NotFound)),
          "the name survived its last handle")?;
    note(system, "PIPES_WRITER_CLOSED_OK\n");
    Ok(())
}

struct Listing
{
    items: u32,
    found: Option<files::Cnid>
}

/// Enumerate the folder through the same resident scan the Finder uses. One
/// step is one item, so the count is exactly what a folder window would show.
fn listing(files: files::Files<'_>, volume: files::VolumeId, folder: files::Cnid, name: &str)
           -> Result<Listing, Line>
{
    let mut cursor = files::DirectoryCursor::default();
    let mut items = 0;
    let mut found = None;
    // Bounded so a scan that never reaches Done fails the probe instead of
    // hanging the guest before its readiness line.
    for _ in 0..1024
    {
        match files.scan_directory(volume, folder, &mut cursor)
        {
            Ok(files::ScanStep::Entry { catalog,
                                        .. }) =>
            {
                items += 1;
                if catalog.name() == name
                {
                    found = Some(catalog.id);
                }
            }
            Ok(files::ScanStep::Pending) => (),
            Ok(files::ScanStep::Done) =>
            {
                return Ok(Listing { items,
                                    found });
            }
            Err(error) => return Err(failed("folder scan", error))
        }
    }
    Err(fault("the folder scan never finished"))
}

fn folder(system: &System<'_>) -> Result<(), Line>
{
    let files = system.files();
    let mut startup = None;
    for index in 0..files.volume_count().map_err(|error| failed("volume count", error))?
    {
        let volume = files.volume_at(index).map_err(|error| failed("volume", error))?;
        if volume.startup
        {
            startup = Some(volume);
            break;
        }
    }
    let Some(volume) = startup
    else
    {
        return Err(fault("no startup volume"));
    };
    let blessed = volume.blessed_folder;
    check(blessed.0 >= files::ROOT_ID.0, "the startup volume has no blessed folder")?;
    // The folder is the System's to create, not the probe's: this replaces
    // Finder, so if it had to create the folder the boot step would be
    // untested. Its absence is a failure, not a fallback.
    let folder = match files.lookup(volume.id, blessed, "Pipes")
    {
        Ok(entry) => entry,
        Err(Error::NotFound) =>
        {
            return Err(fault("System Folder/Pipes was absent at application start"));
        }
        Err(error) => return Err(failed("lookup Pipes", error))
    };
    check(folder.is_directory(), "Pipes is not a folder")?;
    let mut present = Line::new();
    let _ = writeln!(present,
                     "PIPES_FOLDER_PRESENT volume={} blessed={} cnid={}",
                     volume.id.0, blessed.0, folder.id.0);
    note(system, present.text());
    note(system, "\n");

    let (reader, writer) = system.pipes()
                                 .create_named(PIPE, wire::MIN_CAPACITY, 0)
                                 .map_err(|error| failed("create folder pipe", error))?;
    let first = listing(files, volume.id, folder.id, PIPE)?;
    check(first.items == 1, "the folder does not hold exactly one named pipe")?;
    let Some(cnid) = first.found
    else
    {
        return Err(fault("the named pipe is absent from the folder"));
    };
    note(system, "PIPES_FOLDER_LIST_OK items=1\n");

    // The same item by name and by identity, parented by the folder.
    let by_name =
        files.lookup(volume.id, folder.id, PIPE).map_err(|error| failed("lookup item", error))?;
    let by_cnid = files.catalog(volume.id, cnid).map_err(|error| failed("catalog item", error))?;
    check(by_name.id == cnid, "lookup by name returned another identity")?;
    check(by_cnid.id == cnid, "catalog by CNID returned another identity")?;
    check(by_cnid.parent == folder.id, "the item is not parented by the folder")?;
    check(matches!(files.lookup(volume.id, folder.id, "NoSuchPipe"), Err(Error::NotFound)),
          "an unknown name resolved")?;
    note(system, "PIPES_FOLDER_LOOKUP_OK\n");

    // Stable identity, and a destroyed identity is never handed out again.
    let again = listing(files, volume.id, folder.id, PIPE)?;
    check(again.found == Some(cnid), "the identity changed across two enumerations")?;
    drop(writer);
    drop(reader);
    let gone = listing(files, volume.id, folder.id, PIPE)?;
    check(gone.items == 0 && gone.found.is_none(), "a destroyed pipe is still listed")?;
    let (reader2, writer2) = system.pipes()
                                   .create_named(PIPE2, wire::MIN_CAPACITY, 0)
                                   .map_err(|error| failed("create second pipe", error))?;
    let fresh = listing(files, volume.id, folder.id, PIPE2)?;
    let Some(fresh_cnid) = fresh.found
    else
    {
        return Err(fault("the second named pipe is absent from the folder"));
    };
    check(fresh_cnid != cnid, "a destroyed identity was handed to another pipe")?;
    note(system, "PIPES_FOLDER_STABLE_OK\n");

    // Anonymous pipes have no registry row, so a folder listing cannot see one.
    let (anon_reader, anon_writer) = system.pipes()
                                           .create(wire::MIN_CAPACITY)
                                           .map_err(|error| failed("create anonymous", error))?;
    let with_anon = listing(files, volume.id, folder.id, PIPE2)?;
    check(with_anon.items == fresh.items, "an anonymous pipe entered the folder")?;
    check(with_anon.found == Some(fresh_cnid),
          "the named item moved when an anonymous pipe existed")?;
    drop(anon_writer);
    drop(anon_reader);
    note(system, "PIPES_FOLDER_ANON_ABSENT_OK\n");

    // No File Manager mutation may reach the registry or a volume.
    refusal("rename", files.rename(volume.id, fresh_cnid, "Other"))?;
    refusal("set_locked", files.set_locked(volume.id, fresh_cnid, true))?;
    refusal("set_stationery", files.set_stationery(volume.id, fresh_cnid, true))?;
    refusal("set_comment", files.set_comment(volume.id, fresh_cnid, "x"))?;
    refusal("set_icon_location",
            files.set_icon_location(volume.id,
                                    fresh_cnid,
                                    Some(mac_toolbox::abi::geometry::Point { v: 10,
                                                                             h: 10 })))?;
    refusal("set_properties",
            files.set_properties(volume.id,
                                 fresh_cnid,
                                 files::ForkMetadata::from_catalog(&by_cnid, "")))?;
    refusal("move_to_trash", files.move_to_trash(volume.id, fresh_cnid))?;
    refusal("put_away", files.put_away(volume.id, fresh_cnid))?;
    refusal("remove", files.remove(volume.id, fresh_cnid))?;
    refusal("move_into", files.move_into(volume.id, fresh_cnid, blessed))?;
    refusal("new_folder", files.new_folder(volume.id, folder.id, "Nested"))?;
    let survived = listing(files, volume.id, folder.id, PIPE2)?;
    check(survived.found == Some(fresh_cnid), "a refused mutation changed the folder")?;
    note(system, "PIPES_FOLDER_REFUSED_OK refusals=11\n");

    drop(writer2);
    drop(reader2);
    Ok(())
}

fn startup_volume(files: files::Files<'_>) -> Result<files::VolumeInfo, Line>
{
    for index in 0..files.volume_count().map_err(|error| failed("volume count", error))?
    {
        let volume = files.volume_at(index).map_err(|error| failed("volume", error))?;
        if volume.startup
        {
            return Ok(volume);
        }
    }
    Err(fault("no startup volume"))
}

/// Enumerate the `Pipes` folder the way the Finder's browser window does: one
/// bounded continuation step at a time, restarting from a fresh cursor only when
/// the continuation is refused as stale.
///
/// A stale scan is provoked deliberately here rather than waited for: the folder
/// has two sources, so registering a pipe between two steps changes the registry
/// revision and the next step must be `ScanStale` instead of rows that no longer
/// mean the same items. Without the provocation nothing in this probe would see
/// one, so this is coverage of the contract, not of an accident.
fn scan(system: &System<'_>) -> Result<(), Line>
{
    const FIRST: &str = "ScanPipeA";
    const SECOND: &str = "ScanPipeB";
    let files = system.files();
    let volume = startup_volume(files)?;
    let folder = files.lookup(volume.id, volume.blessed_folder, "Pipes")
                      .map_err(|error| failed("lookup Pipes", error))?;
    let (mut reader, writer) = system.pipes()
                                     .create_named(FIRST, wire::MIN_CAPACITY, 0)
                                     .map_err(|error| failed("create scanned pipe", error))?;

    // The whole folder through one continuation, counting the steps it takes.
    // More than one step is the point: the listing is read incrementally, not
    // handed back as an array.
    let (entries, steps) = drain(files, volume.id, folder.id)?;
    check(entries == 1, "the folder did not hold exactly one named pipe")?;
    let mut line = Line::new();
    let _ =
        writeln!(line, "PIPES_SCAN_OK directory={} entries={entries} steps={steps}", folder.id.0);
    note(system, line.text());
    note(system, "\n");

    // One step, then a registration between steps, then the same continuation.
    let mut cursor = files::DirectoryCursor::default();
    match files.scan_directory(volume.id, folder.id, &mut cursor)
    {
        Ok(files::ScanStep::Entry { .. }) => (),
        Ok(_) => return Err(fault("the first scan step was not an entry")),
        Err(error) => return Err(failed("first scan step", error))
    }
    let (second_reader, second_writer) =
        system.pipes().create_named(SECOND, wire::MIN_CAPACITY, 0).map_err(|error| {
                                                                       failed("create second \
                                                                               scanned pipe",
                                                                              error)
                                                                   })?;
    match files.scan_directory(volume.id, folder.id, &mut cursor)
    {
        Ok(_) => return Err(fault("a registration did not invalidate the continuation")),
        Err(Error::ScanStale) => (),
        Err(error) => return Err(failed("expected a stale continuation", error))
    }
    note(system, "PIPES_SCAN_STALE_OK\n");

    // A stale continuation is discarded, not completed: the restarted scan sees
    // both pipes, which is what a caller must do with the refusal.
    let (restarted, restarted_steps) = drain(files, volume.id, folder.id)?;
    check(restarted == 2, "the restarted scan did not see both named pipes")?;
    let mut line = Line::new();
    let _ = writeln!(line, "PIPES_SCAN_RESTART_OK entries={restarted} steps={restarted_steps}");
    note(system, line.text());
    note(system, "\n");

    drop(second_writer);
    drop(second_reader);
    drop(writer);
    drop(reader);
    Ok(())
}

/// Count the entries of one folder through a single continuation, and how many
/// steps it took. Bounded so a scan that never reports `Done` fails the probe
/// instead of hanging the guest before its readiness line.
fn drain(files: files::Files<'_>, volume: files::VolumeId, folder: files::Cnid)
         -> Result<(u32, u32), Line>
{
    let mut cursor = files::DirectoryCursor::default();
    let mut entries = 0;
    let mut steps = 0;
    for _ in 0..2048
    {
        steps += 1;
        match files.scan_directory(volume, folder, &mut cursor)
        {
            Ok(files::ScanStep::Entry { .. }) => entries += 1,
            Ok(files::ScanStep::Pending) => (),
            Ok(files::ScanStep::Done) => return Ok((entries, steps)),
            Err(error) => return Err(failed("folder scan", error))
        }
    }
    Err(fault("the folder scan never finished"))
}

/// The two glyph families, resolved and drawn. This is the evidence the icon
/// work needs: an identifier in a table proves nothing, so each glyph is
/// plotted into an offscreen world and compared, pixel for pixel, with the
/// generic folder or document it must not be.
///
/// The folder is resolved the way the Finder resolves it - by name under the
/// blessed folder, through the SDK table the Finder itself uses - and the item
/// through the resident type bundle, which is the path that turns the
/// `PIPE`/`PIPE` pair into an icon id.
fn icons(system: &System<'_>) -> Result<(), Line>
{
    use mac_toolbox::quickdraw::icons::standard;

    const ICON: &str = "IconPipe";
    let files = system.files();
    let volume = startup_volume(files)?;
    let folder = files.lookup(volume.id, volume.blessed_folder, "Pipes")
                      .map_err(|error| failed("lookup Pipes", error))?;
    check(folder.is_directory(), "Pipes is not a folder")?;
    check(folder.parent == volume.blessed_folder,
          "Pipes is not directly inside the blessed folder")?;
    let folder_id = standard::for_blessed_folder(folder.name()).ok_or_else(|| {
                                                                   fault("the Pipes folder has no \
                                                                          icon of its own")
                                                               })?;
    check(folder_id == standard::PIPES_FOLDER, "the Pipes folder resolved to another icon")?;

    let (reader, writer) = system.pipes()
                                 .create_named(ICON, wire::MIN_CAPACITY, 0)
                                 .map_err(|error| failed("create icon pipe", error))?;
    let item = files.lookup(volume.id, folder.id, ICON)
                    .map_err(|error| failed("lookup named pipe item", error))?;
    let item_id = system.quickdraw()
                        .icons()
                        .for_file(item.creator, item.file_type)
                        .map_err(|error| failed("resolve the pipe type", error))?;
    check(item_id == standard::NAMED_PIPE, "the named pipe type resolved to another icon")?;

    // A fresh world's own pixels are the background, so no fill and no
    // assumption about what colour the surface starts as.
    let mut blank = [0u32; 32 * 32];
    let background = {
        let world = system.quickdraw()
                          .new_gworld(32, 32)
                          .map_err(|error| failed("create comparison world", error))?;
        let count = world.pixels(&mut blank).map_err(|error| failed("read blank world", error))?;
        check(count == blank.len(), "the comparison world is not 32 by 32")?;
        blank[0]
    };
    let mut folder_pixels = [0u32; 32 * 32];
    let mut item_pixels = [0u32; 32 * 32];
    let mut other = [0u32; 32 * 32];
    let generic_folder = draw(system, standard::FOLDER, background, &mut other)?;
    let pipes_folder = draw(system, folder_id, background, &mut folder_pixels)?;
    check(pipes_folder > 0, "the Pipes folder glyph drew nothing")?;
    check(pipes_folder != generic_folder, "the Pipes folder drew the generic folder")?;
    note(system, line_of("PIPES_ICON_FOLDER_OK", folder_id, pipes_folder).text());
    note(system, "\n");

    let generic_document = draw(system, standard::DOCUMENT, background, &mut other)?;
    let named_pipe = draw(system, item_id, background, &mut item_pixels)?;
    check(named_pipe > 0, "the named pipe glyph drew nothing")?;
    check(named_pipe != generic_document, "the named pipe drew the generic document")?;
    check(folder_pixels != item_pixels, "the folder and the named pipe drew the same glyph")?;
    note(system, line_of("PIPES_ICON_ITEM_OK", item_id, named_pipe).text());
    note(system, "\n");
    note(system, "PIPES_ICON_DISTINCT_OK folder=1 item=1\n");

    drop(writer);
    drop(reader);
    Ok(())
}

fn line_of(marker: &str, id: i16, drawn: u32) -> Line
{
    let mut line = Line::new();
    let _ = writeln!(line, "{marker} id={id} drawn={drawn}");
    line
}

/// Plot `id` into a fresh 32 by 32 world, leave the pixels in `output`, and
/// return how many of them differ from `background`.
fn draw(system: &System<'_>, id: i16, background: u32, output: &mut [u32]) -> Result<u32, Line>
{
    use mac_toolbox::quickdraw::icons::IconFlags;

    let world = system.quickdraw()
                      .new_gworld(32, 32)
                      .map_err(|error| failed("create icon world", error))?;
    let target = world.graphics().target();
    system.quickdraw()
          .icons()
          .plot(target, id, Rect::new(0, 0, 32, 32), IconFlags::default())
          .map_err(|error| failed("plot icon", error))?;
    let count = world.pixels(output).map_err(|error| failed("read icon world", error))?;
    check(count == output.len(), "the icon world is not 32 by 32")?;
    Ok(output.iter().filter(|pixel| **pixel != background).count() as u32)
}

/// keep it away from application launch. The witness for "no application was
/// launched" is the process count across the open: the item carries no fork, so
/// the File Manager resolves it and the pipe facility attaches a writer.
///
/// The writer comes back bound to the pipe the creator holds, which the data
/// path proves: bytes written through the adopted handle are read by the
/// creator's reader. The declared writer limit is exercised in the same open,
/// because a handle that did not pass the connection accounting would not
/// consume the limit a second open then collides with.
fn pipe_open(system: &System<'_>) -> Result<(), Line>
{
    const OPENED: &str = "OpenPipe";
    let files = system.files();
    let volume = startup_volume(files)?;
    let folder = files.lookup(volume.id, volume.blessed_folder, "Pipes")
                      .map_err(|error| failed("lookup Pipes", error))?;
    let (mut creator, keeper) = system.pipes()
                                      .create_named(OPENED, wire::MIN_CAPACITY, 1)
                                      .map_err(|error| failed("create open pipe", error))?;
    let Some(cnid) = listing(files, volume.id, folder.id, OPENED)?.found
    else
    {
        return Err(fault("the named pipe is absent from the folder"));
    };

    let before = system.processes().count().map_err(|error| failed("process count", error))?;
    let handle =
        files.open_pipe(volume.id, cnid).map_err(|error| failed("open pipe item", error))?;
    check(handle != 0, "the open produced no writer")?;
    let after = system.processes().count().map_err(|error| failed("process count", error))?;
    check(after == before, "opening a pipe item launched an application")?;
    let adopted = system.pipes().adopt_writer(handle).map_err(|error| failed("adopt", error))?;
    let mut launched = Line::new();
    let _ = writeln!(launched,
                     "PIPES_OPEN_NO_LAUNCH_OK processes_before={before} processes_after={after}");
    note(system, launched.text());
    note(system, "\n");

    // The handle is a real connection: it consumes the one declared writer.
    check(matches!(files.open_pipe(volume.id, cnid), Err(Error::AccessDenied)),
          "the open did not pass the writer limit")?;
    note(system, "PIPES_OPEN_LIMIT_OK\n");

    let mut adopted = adopted;
    let sent = [0x5au8, 0xa5, 0x01, 0x02];
    check(adopted.write(&sent).map_err(|error| failed("open write", error))? == sent.len(),
          "the opened writer wrote short")?;
    let mut back = [0u8; 4];
    check(creator.read(&mut back).map_err(|error| failed("open read", error))? == sent.len(),
          "the creator did not receive the opened writer's bytes")?;
    check(back == sent, "the opened writer's bytes changed")?;
    note(system, "PIPES_OPEN_DATA_OK bytes=4\n");

    // A destroyed identity is not-found rather than a fresh pipe. The name is
    // removed with the last handle, so the identity must go with it.
    drop(adopted);
    drop(keeper);
    drop(creator);
    check(listing(files, volume.id, folder.id, OPENED)?.found.is_none(),
          "the destroyed pipe is still listed")?;
    check(matches!(files.open_pipe(volume.id, cnid), Err(Error::NotFound)),
          "opening a destroyed identity did not refuse")?;
    note(system, "PIPES_OPEN_GONE_OK\n");
    Ok(())
}


fn main(system: &System<'_>) -> mac_toolbox::ToolboxResult<()>
{
    match run(system)
    {
        Ok(cases) =>
        {
            let mut line = Line::new();
            let _ = writeln!(line, "PIPES_PROBE_PASS cases={cases}");
            system.log(line.text())?;
            system.log("\n")?;
            // The boot check stops reading at the platform's readiness line, so
            // it must follow every outcome marker, never precede them.
            system.log("DESKTOP_READY: pipes probe pass\n")?;
            Ok(())
        }
        Err(line) =>
        {
            system.log(line.text())?;
            system.log("\n")?;
            system.log("DESKTOP_READY: pipes probe failed\n")?;
            Err(Error::BadArgument)
        }
    }
}

mac_toolbox::application!(main);
