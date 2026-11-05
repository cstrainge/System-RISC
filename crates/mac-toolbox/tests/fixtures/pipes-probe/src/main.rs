//! Guest execution probe for pipes. It replaces Finder on a cloned disk so the
//! System runs it at boot; every case logs a serial marker and the harness
//! accepts only `PIPES_PROBE_PASS`.
#![no_std]
#![no_main]
use core::{fmt::{self, Write},
           sync::atomic::{AtomicBool, Ordering}};

use mac_toolbox::{Error, System, abi::pipes as wire};

static ARMED: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);
const NAME: &str = "ProbePipe";
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
