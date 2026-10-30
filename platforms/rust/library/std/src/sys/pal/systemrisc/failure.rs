//! Nonallocating default panic/OOM reporting through the existing System CRASH
//! service. Resident code owns the bomb window, input and report retention.
use mac_abi::crashes::{self, CrashCall};
use mac_traps::Client;

use super::runtime;
use crate::{fmt::{self, Write},
            sync::atomic::{AtomicBool, Ordering}};

static REPORTING: AtomicBool = AtomicBool::new(false);

struct Writer
{
    client:   Client<'static>,
    bytes:    [u8; crashes::MAX_APPEND_BYTES],
    used:     usize,
    resident: bool,
    flags:    u32
}
impl Writer
{
    fn flush(&mut self)
    {
        let text = crate::str::from_utf8(&self.bytes[..self.used]).unwrap_or("");
        let _ = self.client.log(text);
        if self.resident
        {
            let mut call = CrashCall { operation: crashes::APPEND,
                                       ..Default::default() };
            if self.client.crash(&mut call, text, &mut []).is_err()
            {
                self.resident = false;
                self.flags |= crashes::TRUNCATED;
            }
        }
        self.used = 0;
    }
}
impl Write for Writer
{
    fn write_str(&mut self, mut text: &str) -> fmt::Result
    {
        while !text.is_empty()
        {
            let mut count = text.len().min(self.bytes.len() - self.used);
            while !text.is_char_boundary(count)
            {
                count -= 1;
            }
            if count == 0
            {
                self.flush();
                continue;
            }
            self.bytes[self.used..self.used + count].copy_from_slice(&text.as_bytes()[..count]);
            self.used += count;
            text = &text[count..];
            if self.used == self.bytes.len()
            {
                self.flush();
            }
        }
        Ok(())
    }
}

pub(super) fn report(arguments: fmt::Arguments<'_>) -> !
{
    let Some(client) = runtime::installed_client()
    else
    {
        mac_traps::emergency::abort(format_args!("STD_FAILURE_BEFORE_SERVICES: {arguments}"));
    };
    if REPORTING.swap(true, Ordering::AcqRel)
    {
        mac_traps::emergency::abort(format_args!("STD_FAILURE_ALREADY_REPORTING"));
    }
    let mut begin = CrashCall { operation: crashes::BEGIN,
                                ..Default::default() };
    let begun = client.crash(&mut begin, "", &mut []);
    let mut writer = Writer { client,
                              bytes: [0; crashes::MAX_APPEND_BYTES],
                              used: 0,
                              resident: begun.is_ok(),
                              flags: 0 };
    if begun.is_ok()
    {
        let _ = writeln!(writer, "App: {}  Thread: {}", begin.info.app, begin.info.thread);
    }
    if writer.write_fmt(arguments).is_err()
    {
        writer.flags |= crashes::FORMAT_FAILED;
        let _ = writer.write_str("\n[Formatting the std diagnostic failed.]\n");
    }
    let _ = writer.write_str("\n");
    writer.flush();
    let mut show = CrashCall { operation: crashes::SHOW,
                               flags: writer.flags,
                               ..Default::default() };
    let shown = begun.and_then(|_| client.crash(&mut show, "", &mut []));
    if let Err(cause) = shown
    {
        mac_traps::emergency::abort(format_args!("STD_REPORT_UNAVAILABLE: {cause}; see serial \
                                                  diagnostic"));
    }
    // System retains the report and owns dismissal and reclamation. The failed
    // author stops immediately and never runs user TLS or stack destructors.
    runtime::terminate(101, mac_traps::native_exit::ExitKind::Abort)
}

/// # Safety
/// Spans come from the pinned std hook, are valid UTF-8, and stay borrowed for
/// the nonreturning report turn. No Rust-layout value crosses this boundary.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __system_risc_panic_v1(message: *const u8, message_length: usize,
                                                file: *const u8, file_length: usize, line: u32,
                                                column: u32)
                                                -> !
{
    unsafe fn text<'a>(pointer: *const u8, length: usize) -> &'a str
    {
        if length == 0
        {
            return "";
        }
        if pointer.is_null() || length > isize::MAX as usize
        {
            mac_traps::emergency::abort(format_args!("STD_PANIC_INVALID_SPAN"));
        }
        match crate::str::from_utf8(unsafe { crate::slice::from_raw_parts(pointer, length) })
        {
            Ok(text) => text,
            Err(_) => mac_traps::emergency::abort(format_args!("STD_PANIC_INVALID_UTF8"))
        }
    }
    let message = unsafe { text(message, message_length) };
    let file = unsafe { text(file, file_length) };
    report(format_args!("Rust panic at {file}:{line}:{column}\n{message}"))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __system_risc_oom_v1(size: usize, alignment: usize) -> !
{
    report(format_args!("Application heap exhausted (status={}): requested {size} bytes, \
                         alignment {alignment}",
                        mac_abi::NO_MEMORY))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __system_risc_abort_v1() -> !
{
    mac_traps::emergency::abort(format_args!("STD_APPLICATION_ABORT"))
}
