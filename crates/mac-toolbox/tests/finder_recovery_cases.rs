//! Canonical restart assertions run inside independently loaded RV64 apps.
#![cfg(target_arch = "riscv64")]
use core::{fmt::Write,
           sync::atomic::{AtomicU32, Ordering}};

use mac_toolbox::{events::{Event, EventMask},
                  files::{DataFileSpec, Fork, ROOT_ID},
                  windows::WindowSpec,
                  *};
static ENTRIES: AtomicU32 = AtomicU32::new(0);
const MARKER: &str = "Finder Recovery State";

struct Log
{
    bytes: [u8; 256],
    len:   usize
}
impl Write for Log
{
    fn write_str(&mut self, text: &str) -> core::fmt::Result
    {
        if text.len() > self.bytes.len() - self.len
        {
            return Err(core::fmt::Error);
        }
        self.bytes[self.len..self.len + text.len()].copy_from_slice(text.as_bytes());
        self.len += text.len();
        Ok(())
    }
}
fn log(system: &System<'_>, args: core::fmt::Arguments<'_>) -> ToolboxResult<()>
{
    let mut text = Log { bytes: [0; 256],
                         len:   0 };
    text.write_fmt(args).map_err(|_| Error::BadArgument)?;
    system.log(core::str::from_utf8(&text.bytes[..text.len]).map_err(|_| Error::BadArgument)?)
}
fn save(system: &System<'_>, bytes: &[u8; 48]) -> ToolboxResult<()>
{
    let files = system.files();
    let volume = files.volume_at(0)?.id;
    let replacing = match files.lookup(volume, ROOT_ID, MARKER)
    {
        Ok(entry) => Some(entry.id),
        Err(Error::NotFound) => None,
        Err(error) => return Err(error)
    };
    let mut writer = files.create_data(DataFileSpec { volume,
                                                      parent: ROOT_ID,
                                                      name: MARKER,
                                                      replacing,
                                                      file_type: TypeCode::new(*b"DATA"),
                                                      creator: TypeCode::new(*b"FREC") },
                                       bytes.len())?;
    writer.write_all(bytes)?;
    writer.finish()?;
    Ok(())
}
pub fn finder(system: &System<'_>) -> ToolboxResult<()>
{
    assert_eq!(ENTRIES.fetch_add(1, Ordering::SeqCst),
               0,
               "replacement reused writable image state");
    let current = system.processes().current()?;
    let files = system.files();
    let volume = files.volume_at(0)?.id;
    let mut bytes = [0u8; 48];
    let phase = match files.lookup(volume, ROOT_ID, MARKER)
    {
        Ok(entry) =>
        {
            let mut source = files.open(volume, entry.id, Fork::Data)?;
            assert_eq!(source.read(&mut bytes)?, bytes.len(), "persisted recovery proof truncated");
            bytes[28] + 1
        }
        Err(Error::NotFound) => 0,
        Err(error) => return Err(error)
    };
    if phase != 0
    {
        let old = u64::from_le_bytes(bytes[..8].try_into().unwrap());
        let thread = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let window = WindowId(u64::from_le_bytes(bytes[16..24].try_into().unwrap()));
        let handle = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        assert_ne!(old, current.id.0, "restart reused process identity");
        assert_ne!(thread, current.thread.0, "restart reused thread identity");
        assert_eq!(system.windows().info(window),
                   Err(Error::NotFound),
                   "old window survived owner teardown");
        let mut stale = abi::memory::MemoryCall { operation: abi::memory::SIZE,
                                                  handle,
                                                  ..Default::default() };
        assert!(system.client().memory(&mut stale, &[], &mut []).is_err(),
                "old memory handle aliased replacement");
        let worker = u64::from_le_bytes(bytes[32..40].try_into().unwrap());
        let mut survivors = 0;
        for index in 0..system.processes().count()?
        {
            let process = system.processes().at(index)?;
            assert_ne!(process.id.0, old, "dying Finder exposed as running");
            if process.id.0 == worker
            {
                survivors += 1;
            }
        }
        // The pressure case explicitly quits the worker to release its arena.
        if bytes[40] == 0
        {
            assert_eq!(survivors, 1, "independent app disappeared during shell recovery");
        }
        log(system,
            format_args!("FINDER_RECOVERY_PASS phase={phase} old={old} new={} \
                          old_thread={thread} new_thread={} stale_window={} \
                          stale_handle={handle}\n",
                         current.id.0, current.thread.0, window.0))?;
    }
    else
    {
        let apps = files.lookup(volume, ROOT_ID, "Applications")?;
        let worker = files.lookup(volume, apps.id, "RecoveryWorker")?;
        let worker = system.processes().launch(volume, worker.id)?;
        bytes[32..40].copy_from_slice(&worker.id.0.to_le_bytes());
    }
    let window =
        system.windows()
              .create(WindowSpec::new(Rect::new(80, 100, 360, 220), "Finder recovery fixture"))?;
    let mut handle = abi::memory::MemoryCall { operation: abi::memory::NEW_CLEAR,
                                               size: 64,
                                               ..Default::default() };
    system.client().memory(&mut handle, &[], &mut [])?;
    if phase != 0
    {
        let old_window = WindowId(u64::from_le_bytes(bytes[16..24].try_into().unwrap()));
        assert_ne!(window, old_window, "replacement window aliased old public ID");
        assert_eq!(system.windows().info(old_window),
                   Err(Error::NotFound),
                   "old ID became valid after new window publication");
        let mut old_handle =
            abi::memory::MemoryCall { operation: abi::memory::SIZE,
                                      handle: u32::from_le_bytes(bytes[24..28].try_into()
                                                                              .unwrap()),
                                      ..Default::default() };
        assert!(system.client().memory(&mut old_handle, &[], &mut []).is_err(),
                "old handle became valid after replacement allocation");
    }
    bytes[..8].copy_from_slice(&current.id.0.to_le_bytes());
    bytes[8..16].copy_from_slice(&current.thread.0.to_le_bytes());
    bytes[16..24].copy_from_slice(&window.0.to_le_bytes());
    bytes[24..28].copy_from_slice(&handle.handle.to_le_bytes());
    bytes[28] = phase;
    save(system, &bytes)?;
    if phase == 0
    {
        system.log("DESKTOP_READY: public Finder recovery fixture\n")?;
        // A background launch must not own the failed shell's report input.
        system.processes().activate(current.id)?;
        panic!("Finder recovery fixture: retained report café λ phase={phase}");
    }
    loop
    {
        match system.events().wait_event(EventMask::ALL, 60)?
        {
            Event::KeyDown { key,
                             .. } if key.character() == 'p' =>
            {
                panic!("Finder recovery fixture: second failure phase={phase}")
            }
            Event::KeyDown { key,
                             .. } if key.character() == 'x' =>
            {
                system.log("FINDER_RECOVERY_INPUT_ACK\n")?
            }
            Event::Update { window,
                            .. } =>
            {
                let update = system.windows().update(window)?;
                update.finish()?;
            }
            _ => ()
        }
    }
}

pub fn worker(system: &System<'_>) -> ToolboxResult<()>
{
    let current = system.processes().current()?;
    let _window =
        system.windows().create_owned(WindowSpec::new(Rect::new(400, 100, 700, 240),
                                                       "Surviving application"))?;
    log(system,
        format_args!("FINDER_RECOVERY_WORKER_READY app={} thread={}\n",
                     current.id.0, current.thread.0))?;
    loop
    {
        match system.events().wait_event(EventMask::ALL, 60)?
        {
            Event::KeyDown { key,
                             .. } if key.character() == 'p' =>
            {
                let files = system.files();
                let volume = files.volume_at(0)?.id;
                let entry = files.lookup(volume, ROOT_ID, MARKER)?;
                let mut bytes = [0u8; 48];
                files.open(volume, entry.id, Fork::Data)?.read(&mut bytes)?;
                bytes[40] = 1;
                save(system, &bytes)?;
                system.processes()
                      .activate(ProcessId(u64::from_le_bytes(bytes[..8].try_into().unwrap())))?;
                system.log("FINDER_RECOVERY_PRESSURE_READY\n")?;
            }
            Event::KeyDown { key,
                             .. } if key.character() == 'q' =>
            {
                system.log("FINDER_RECOVERY_WORKER_EXIT\n")?;
                return Ok(());
            }
            Event::KeyDown { key,
                             .. } if key.character() == 'x' =>
            {
                system.log("FINDER_RECOVERY_WORKER_INPUT_ACK\n")?
            }
            Event::Update { window,
                            .. } =>
            {
                let update = system.windows().update(window)?;
                update.finish()?;
            }
            _ => ()
        }
    }
}
