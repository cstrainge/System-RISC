//! Shared native application panic handler. System owns the report window and
//! its input handling; the failed application only streams text and sleeps.
use core::fmt::{self, Write};
#[cfg(not(target_os = "systemrisc"))]
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::{Client, SystemServices,
            crashes::{self, Crashes}};

#[cfg(not(target_os = "systemrisc"))]
static SERVICES: AtomicPtr<SystemServices> = AtomicPtr::new(core::ptr::null_mut());
#[cfg(all(target_arch = "riscv64", not(target_os = "systemrisc")))]
static PANICKING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Install the loader's service table before executing application code.
///
/// # Safety
/// The table and callbacks must remain valid until every application thread has
/// terminated, including while its panic handler is running.
#[cfg(not(target_os = "systemrisc"))]
pub unsafe fn install(services: *const SystemServices) -> Result<(), crate::abi::Status>
{
    if !services.addr().is_multiple_of(core::mem::align_of::<SystemServices>())
    {
        return Err(crate::abi::BAD_ARGUMENT);
    }
    unsafe { Client::from_raw(services) }.map_err(|error| error.status())?;
    SERVICES.compare_exchange(core::ptr::null_mut(),
                              services.cast_mut(),
                              Ordering::AcqRel,
                              Ordering::Acquire)
            .map_err(|_| crate::abi::BAD_ABI)?;
    Ok(())
}

/// Read only the table installed by the genuine entry contract. A nonnull
/// pointer is published only after validation; no separate allocator selection
/// or synthetic current application exists in the SDK.
pub(crate) fn installed_client() -> Option<Client<'static>>
{
    #[cfg(not(target_os = "systemrisc"))]
    let pointer = SERVICES.load(Ordering::Acquire);
    #[cfg(target_os = "systemrisc")]
    let pointer = unsafe {
        // std's platform entry owns the one installation for a std process.
        // Only the C-layout service table crosses the sysroot/Cargo boundary.
        unsafe extern "C" {
            fn __system_risc_services_v1() -> *const SystemServices;
        }
        __system_risc_services_v1()
    };
    // SAFETY: install requires permanent immutable table/callback lifetime.
    unsafe { Client::from_raw(pointer) }.ok()
}

struct Writer<'a>
{
    client:   Client<'a>,
    buffer:   [u8; crashes::MAX_APPEND_BYTES],
    used:     usize,
    resident: bool,
    flags:    crashes::ReportFlags
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
            self.flags |= crashes::ReportFlags::TRUNCATED;
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

#[cfg(all(target_arch = "riscv64", not(target_os = "systemrisc")))]
pub(crate) fn diagnostic(client: Client<'_>, arguments: fmt::Arguments<'_>)
{
    let mut writer = Writer { client,
                              buffer: [0; crashes::MAX_APPEND_BYTES],
                              used: 0,
                              resident: false,
                              flags: crashes::ReportFlags::NONE };
    if writer.write_fmt(arguments).is_err()
    {
        let _ = writer.write_str("\n[Formatting the application diagnostic failed.]\n");
    }
    writer.flush();
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
                              buffer: [0; crashes::MAX_APPEND_BYTES],
                              used: 0,
                              resident: begin.is_ok(),
                              flags: crashes::ReportFlags::NONE };
    if let Ok(process) = crate::processes::Processes::new(client).current()
    {
        let _ = writeln!(writer,
                         "Application: {}\nApp: {}  Thread: {}",
                         process.name(),
                         process.id,
                         begin.as_ref().map_or(process.thread, |info| info.thread));
    }
    if writer.write_fmt(arguments).is_err()
    {
        writer.flags |= crashes::ReportFlags::FORMAT_FAILED;
        let _ = writer.write_str("\n[Formatting the panic information failed.]\n");
    }
    let _ = writer.write_str("\n");
    writer.flush();
    begin?;
    crashes.show(writer.flags)
}

/// Called by the application's single panic handler. No application events or
/// drawing callbacks run after a panic; native event waits park the OS thread.
#[cfg(all(target_arch = "riscv64", not(target_os = "systemrisc")))]
pub fn handle(info: &core::panic::PanicInfo<'_>) -> !
{
    fail(format_args!("{info}"))
}

/// Nonallocating common path for panic, OOM and allocator protocol failures.
/// Current applications have one scheduled author thread; this does not claim
/// a future concurrent application-failure coordinator or arbitrary unwind.
#[cfg(all(target_arch = "riscv64", not(target_os = "systemrisc")))]
pub(crate) fn fail(arguments: fmt::Arguments<'_>) -> !
{
    let Some(client) = installed_client()
    else
    {
        crate::application::abort(format_args!("APPLICATION_FAILURE_BEFORE_SERVICES: {arguments}"));
    };
    if PANICKING.swap(true, Ordering::AcqRel)
    {
        crate::application::abort(format_args!("APPLICATION_FAILURE_ALREADY_REPORTING; \
                                                recursive/concurrent failure is uncontained"));
    }
    let _ = client.log("APPLICATION_PANIC_BEGIN\n");
    let shown = match report(client, arguments)
    {
        Ok(_) => true,
        Err(cause) =>
        {
            diagnostic(client,
                       format_args!("APPLICATION_PANIC_VIEWER_UNAVAILABLE status={} \
                                     cause={cause}; see serial diagnostics\n",
                                    cause.status()));
            false
        }
    };
    park(client, shown)
}

#[cfg(all(target_arch = "riscv64", not(target_os = "systemrisc")))]
fn park(client: Client<'_>, shown: bool) -> !
{
    loop
    {
        let dismissed = !shown || Crashes::new(client).info().is_ok_and(|info| info.dismissed);
        if dismissed
        {
            let mut call = mac_abi::processes::ProcessCall { operation: mac_abi::processes::EXIT,
                                                             ..Default::default() };
            let status =
                client.process(&mut call).map_or_else(|error| error.status(), |_| crate::abi::OK);
            crate::application::exit_returned(i64::from(status));
        }
        // System owns input and reporting. This wait invokes no app handlers.
        if let Err(cause) = client.wait_event(0, 60)
        {
            crate::application::abort(format_args!("APPLICATION_FAILURE_WAIT_REFUSED status={} \
                                                    cause={cause}",
                                                   cause.status()));
        }
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
