//! Shared native application panic handler. System owns the report window and
//! its input handling; the failed application only streams text and sleeps.
use core::{fmt::{self, Write},
           panic::PanicInfo,
           sync::atomic::{AtomicBool, AtomicPtr, Ordering}};

use crate::{Client, Status, SystemServices,
            crashes::{self, Crashes}};

static SERVICES: AtomicPtr<SystemServices> = AtomicPtr::new(core::ptr::null_mut());
static PANICKING: AtomicBool = AtomicBool::new(false);

/// Install the loader's service table before executing application code.
///
/// # Safety
/// The table and callbacks must remain valid until every application thread has
/// terminated, including while its panic handler is running.
pub unsafe fn install(services: *const SystemServices) -> Result<(), Status>
{
    unsafe { Client::from_raw(services) }.map_err(|error| error.0)?;
    SERVICES.store(services.cast_mut(), Ordering::Release);
    Ok(())
}

struct Writer<'a>
{
    client:   Client<'a>,
    buffer:   [u8; 512],
    used:     usize,
    resident: bool,
    flags:    u32
}
impl Writer<'_>
{
    fn flush(&mut self)
    {
        // write_str only copies complete UTF-8 scalars into this buffer.
        let text = core::str::from_utf8(&self.buffer[..self.used]).unwrap_or("");
        let _ = self.client.log(text);
        if self.resident && Crashes::new(self.client).append(text).is_err()
        {
            self.resident = false;
            self.flags |= crashes::TRUNCATED;
        }
        self.used = 0;
    }
}
impl Write for Writer<'_>
{
    fn write_str(&mut self, mut text: &str) -> fmt::Result
    {
        while !text.is_empty()
        {
            let mut count = text.len().min(self.buffer.len() - self.used);
            while !text.is_char_boundary(count)
            {
                count -= 1;
            }
            if count == 0
            {
                self.flush();
                continue;
            }
            self.buffer[self.used..self.used + count].copy_from_slice(&text.as_bytes()[..count]);
            self.used += count;
            text = &text[count..];
            if self.used == self.buffer.len()
            {
                self.flush();
            }
        }
        Ok(())
    }
}

/// Stream complete diagnostics without allocating in the application. All text
/// also goes to the serial log. System retains up to1MiB and explicitly marks a
/// retention/formatting failure instead of silently presenting a shortened report.
/// Exposed for userspace conformance and custom application failure entrypoints.
pub fn report(client: Client<'_>, arguments: fmt::Arguments<'_>)
              -> crate::ToolboxResult<crashes::CrashInfo>
{
    let crashes = Crashes::new(client);
    let begin = crashes.begin();
    let mut writer = Writer { client,
                              buffer: [0; 512],
                              used: 0,
                              resident: begin.is_ok(),
                              flags: 0 };
    let mut process = mac_abi::processes::ProcessCall::default();
    if client.process(&mut process).is_ok()
    {
        let _ = writeln!(writer,
                         "Application: {}\nApp: {}  Thread: {}",
                         process.info.name(),
                         process.info.id,
                         begin.as_ref().map_or(process.info.thread, |info| info.thread));
    }
    if writer.write_fmt(arguments).is_err()
    {
        writer.flags |= crashes::FORMAT_FAILED;
        let _ = writer.write_str("\n[Formatting the panic information failed.]\n");
    }
    let _ = writer.write_str("\n");
    writer.flush();
    begin?;
    crashes.show(writer.flags)
}

/// Called by the application's single panic handler. No application events or
/// drawing callbacks run after a panic; native event waits park the OS thread.
pub fn handle(info: &PanicInfo<'_>) -> !
{
    let pointer = SERVICES.load(Ordering::Acquire);
    // install's lifetime contract is stronger than this entire handler.
    let Ok(client) = (unsafe { Client::from_raw(pointer) })
    else
    {
        loop
        {
            core::hint::spin_loop();
        }
    };
    if PANICKING.swap(true, Ordering::AcqRel)
    {
        let _ = client.log("APPLICATION_PANIC_RECURSIVE\n");
        park(client, false);
    }
    let _ = client.log("APPLICATION_PANIC_BEGIN\n");
    let shown = report(client, format_args!("{info}")).is_ok();
    if !shown
    {
        let _ = client.log("APPLICATION_PANIC_VIEWER_UNAVAILABLE; see serial diagnostics\n");
    }
    park(client, shown)
}

fn park(client: Client<'_>, shown: bool) -> !
{
    loop
    {
        let dismissed = !shown || Crashes::new(client).info().is_ok_and(|info| info.dismissed != 0);
        if dismissed
        {
            let mut call = mac_abi::processes::ProcessCall { operation: mac_abi::processes::EXIT,
                                                             ..Default::default() };
            let _ = client.process(&mut call);
        }
        // A zero mask runs no app handlers. System services physical input and
        // paints the report; waiting yields to other applications and the kernel.
        let _ = client.wait_event(0, 60);
    }
}

/// Define the common handler once in each no_std native application binary.
#[macro_export]
macro_rules! panic_handler {
    () => {
        #[cfg(not(test))]
        #[panic_handler]
        fn panic(info: &core::panic::PanicInfo<'_>) -> !
        {
            $crate::panic::handle(info)
        }
    };
}
