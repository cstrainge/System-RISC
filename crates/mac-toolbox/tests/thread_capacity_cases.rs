//! Real simultaneous apps, native thread identities and reclaimed stack reuse.
//! All behavioral assertions use the public SDK; host code transports keys only.
#![cfg(target_arch = "riscv64")]
use core::{fmt::{self, Write},
           sync::atomic::{AtomicU64, Ordering}};

use mac_toolbox::{files::ROOT_ID, processes::ProcessInfo, windows::WindowSpec, *};
const WORKERS: usize = 9;
static PRIVATE: AtomicU64 = AtomicU64::new(0);
struct Buffer
{
    bytes:  [u8; 384],
    length: usize
}
impl Write for Buffer
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        let rest = self.bytes.get_mut(self.length..self.length + text.len()).ok_or(fmt::Error)?;
        rest.copy_from_slice(text.as_bytes());
        self.length += text.len();
        Ok(())
    }
}
fn log(system: &System<'_>, args: fmt::Arguments<'_>) -> ToolboxResult<()>
{
    let mut text = Buffer { bytes:  [0; 384],
                            length: 0 };
    text.write_fmt(args).map_err(|_| Error::BadArgument)?;
    system.log(core::str::from_utf8(&text.bytes[..text.length]).map_err(|_| Error::BadAbi)?)
}
fn yield_tick(system: &System<'_>) -> ToolboxResult<()>
{
    if let Event::Update { window,
                           .. } = system.events().wait_event(EventMask::ALL, 1)?
    {
        system.windows().update(window)?.finish()?;
    }
    Ok(())
}
pub fn finder(system: &System<'_>) -> ToolboxResult<()>
{
    let own = system.processes().current()?;
    let _window =
        system.windows()
              .create_owned(WindowSpec::new(Rect::new(30, 80, 270, 150), "Thread capacity"))?;
    system.log("DESKTOP_READY: public dynamic thread capacity acceptance\n")?;
    let volume = system.files().volume_at(0)?.id;
    let apps = system.files().lookup(volume, ROOT_ID, "Applications")?;
    let mut previous = [ProcessInfo::default(); WORKERS];
    for phase in 0..2
    {
        let mut workers = [ProcessInfo::default(); WORKERS];
        for index in 0..WORKERS
        {
            let mut name = Buffer { bytes:  [0; 384],
                                    length: 0 };
            write!(&mut name, "Capacity {index}").map_err(|_| Error::BadAbi)?;
            let name = core::str::from_utf8(&name.bytes[..name.length]).map_err(|_| Error::BadAbi)?;
            let file = system.files().lookup(volume, apps.id, name)?;
            let mut admitted = None;
            for _ in 0..300
            {
                match system.processes().launch(volume, file.id)
                {
                    Ok(info) =>
                    {
                        admitted = Some(info);
                        break;
                    }
                    Err(Error::ServiceBusy) => yield_tick(system)?,
                    Err(error) => return Err(error)
                }
            }
            let info = admitted.ok_or(Error::ServiceBusy)?;
            assert!(info.thread.0 > u64::from(u32::MAX),
                    "thread identity still projected into fixed slots");
            assert!(info.image.is_some(), "admitted process has no registered image");
            assert_eq!(system.processes().launch(volume, file.id)?.id,
                       info.id,
                       "same file spawned twice");
            for prior in &workers[..index]
            {
                assert_ne!(prior.id, info.id);
                assert_ne!(prior.thread, info.thread);
                assert_ne!(prior.image, info.image);
            }
            if phase != 0
            {
                assert_eq!(system.processes().activate(previous[index].id),
                           Err(Error::NotFound),
                           "reused thread slot revived old process ID");
                assert_ne!(info.id, previous[index].id);
                assert_ne!(info.thread, previous[index].thread);
                assert_ne!(info.image, previous[index].image);
            }
            workers[index] = info;
        }
        assert_eq!(system.processes().count()?,
                   (WORKERS + 1) as u32,
                   "fixed scheduler/application cap remains");
        log(system, format_args!("THREAD_CAPACITY_LIVE phase={phase} apps={}\n", WORKERS + 1))?;
        for index in (0..WORKERS).rev()
        {
            system.processes().activate(workers[index].id)?;
            log(system, format_args!("THREAD_CAPACITY_INPUT phase={phase} index={index}\n"))?;
            for _ in 0..600
            {
                if !system.processes().is_running(volume, workers[index].cnid)?
                {
                    break;
                }
                yield_tick(system)?;
            }
            assert!(!system.processes().is_running(volume, workers[index].cnid)?,
                    "physical key did not reach independent app");
            let refusal = system.processes().activate(workers[index].id);
            assert!(matches!(refusal, Err(Error::NotFound | Error::ServiceBusy)),
                    "stopped process ID was accepted or lost its cleanup refusal: {refusal:?}");
            assert_eq!(system.processes().current()?.id, own.id, "context identity crossed apps");
        }
        assert_eq!(system.processes().count()?,
                   1,
                   "stopped apps remain in public process enumeration");
        previous = workers;
    }
    system.log("THREAD_CAPACITY_PASS apps=10 incarnations=18 native-ids=passed \
                private-globals=passed input=passed reuse=passed\n")?;
    loop
    {
        yield_tick(system)?;
    }
}
pub fn worker(system: &System<'_>) -> ToolboxResult<()>
{
    let own = system.processes().current()?;
    assert_eq!(PRIVATE.compare_exchange(0, own.id.0, Ordering::SeqCst, Ordering::SeqCst),
               Ok(0),
               "applications share writable globals");
    let image = own.image.ok_or(Error::BadAbi)?;
    let window =
        system.windows().create_owned(WindowSpec::new(Rect::new(320, 80, 480, 130), own.name()))?;
    let handle = system.memory().new_handle_clear(32)?;
    handle.write(0, &own.id.0.to_le_bytes())?;
    log(system,
        format_args!("THREAD_CAPACITY_WORKER app={} thread={} image={} global={} window={}\n",
                     own.id.0,
                     own.thread.0,
                     image.raw(),
                     &PRIVATE as *const _ as usize,
                     window.id().0))?;
    loop
    {
        match system.events().wait_event(EventMask::ALL, 60)?
        {
            Event::KeyDown { key,
                             .. } if key.character() == 'x' =>
            {
                assert_eq!(PRIVATE.load(Ordering::SeqCst),
                           own.id.0,
                           "another app changed private globals");
                let mut bytes = [0; 8];
                handle.read(0, &mut bytes)?;
                assert_eq!(u64::from_le_bytes(bytes),
                           own.id.0,
                           "handle contents crossed application owners");
                assert_eq!(system.processes().current()?.thread,
                           own.thread,
                           "key serviced in foreign thread context");
                log(system, format_args!("THREAD_CAPACITY_ACK app={}\n", own.id.0))?;
                return Ok(());
            }
            Event::Update { window,
                            .. } => system.windows().update(window)?.finish()?,
            _ => ()
        }
    }
}
