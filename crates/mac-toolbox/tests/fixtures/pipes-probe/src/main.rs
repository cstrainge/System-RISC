//! Guest execution probe for pipes. It replaces Finder on a cloned disk so the
//! System runs it at boot; every case logs a serial marker and the harness
//! accepts only `PIPES_PROBE_PASS`.
#![no_std]
#![no_main]
use core::{fmt::{self, Write},
           sync::atomic::{AtomicBool, Ordering}};

use mac_toolbox::{Error, System, abi::pipes as wire, files};

static ARMED: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);
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

fn run(system: &System<'_>) -> Result<(), Line>
{
    echo(system)?;
    block(system)?;
    eof(system)?;
    broken(system)?;
    named(system)?;
    folder(system)?;
    Ok(())
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
                                         .create_named(NAME, wire::MIN_CAPACITY)
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
                                     .create_named(NAME, wire::MIN_CAPACITY)
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

struct Listing
{
    items: u32,
    found: Option<files::Cnid>
}

/// Enumerate the folder through the same resident scan the Finder uses. One
/// step is one item, so the count is exactly what a folder window would show.
fn listing(files: files::Files<'_>, volume: files::VolumeId, folder: files::Cnid,
           name: &str) -> Result<Listing, Line>
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
            Ok(files::ScanStep::Entry { catalog, .. }) =>
            {
                items += 1;
                if catalog.name() == name
                {
                    found = Some(catalog.id);
                }
            }
            Ok(files::ScanStep::Pending) => (),
            Ok(files::ScanStep::Done) => return Ok(Listing { items, found }),
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
    // This probe replaces Finder, so it may be the run that creates the folder.
    let folder = match files.lookup(volume.id, blessed, "Pipes")
    {
        Ok(entry) => entry,
        Err(Error::NotFound) =>
        {
            files.new_folder(volume.id, blessed, "Pipes")
                 .map_err(|error| failed("create Pipes", error))?
        }
        Err(error) => return Err(failed("lookup Pipes", error))
    };
    check(folder.is_directory(), "Pipes is not a folder")?;

    let (reader, writer) = system.pipes()
                                  .create_named(PIPE, wire::MIN_CAPACITY)
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
                                   .create_named(PIPE2, wire::MIN_CAPACITY)
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
    check(with_anon.found == Some(fresh_cnid), "the named item moved when an anonymous pipe existed")?;
    drop(anon_writer);
    drop(anon_reader);
    note(system, "PIPES_FOLDER_ANON_ABSENT_OK\n");

    // No File Manager mutation may reach the registry or a volume.
    refusal("rename", files.rename(volume.id, fresh_cnid, "Other"))?;
    refusal("set_locked", files.set_locked(volume.id, fresh_cnid, true))?;
    refusal("set_stationery", files.set_stationery(volume.id, fresh_cnid, true))?;
    refusal("set_comment", files.set_comment(volume.id, fresh_cnid, "x"))?;
    refusal("set_icon_location",
            files.set_icon_location(volume.id, fresh_cnid, Some(mac_toolbox::abi::geometry::Point {
                v: 10,
                h: 10
            })))?;
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


fn main(system: &System<'_>) -> mac_toolbox::ToolboxResult<()>
{
    match run(system)
    {
        Ok(()) =>
        {
            system.log("PIPES_PROBE_PASS cases=5\n")?;
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
