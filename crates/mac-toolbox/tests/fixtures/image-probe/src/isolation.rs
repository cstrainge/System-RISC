//! Deliberate target instructions in the existing native probe. These are real
//! U-mode operations, not host models, kernel fault injection or a trap substitute.
use alloc::boxed::Box;
use core::{arch::asm,
           mem::size_of,
           sync::atomic::{AtomicU64, Ordering}};

use mac_toolbox::{Error, System,
                  abi::{self,
                        traps::{LOG, LogCall}}};

fn target(system: &System<'_>) -> Result<usize, Error>
{
    let image = system.processes().current()?.image.ok_or(Error::BadAbi)?;
    let mut bytes = [0; 32];
    let text = system.resources().image(image).string(2002, &mut bytes)?;
    let digits = text.strip_prefix("0x").ok_or(Error::BadArgument)?;
    usize::from_str_radix(digits, 16).map_err(|_| Error::BadArgument)
}

/// The request address is deliberately an integer: no forged Rust reference is
/// formed for malformed top-level/nested requests. Only actual ECALL sees it.
unsafe fn raw_log(address: usize) -> i64
{
    let mut result = address;
    unsafe {
        asm!("ecall", in("a7") LOG, inlateout("a0") result,
             in("a1") size_of::<LogCall>(), clobber_abi("C"));
    }
    result as i64
}

fn refused(address: usize, expected: abi::Status) -> Result<(), Error>
{
    // SAFETY: this diagnostic intentionally supplies invalid addresses through
    // the real architectural boundary, never to a Rust dereference.
    let actual = unsafe { raw_log(address) };
    if actual != i64::from(expected)
    {
        return Err(Error::BadAbi);
    }
    Ok(())
}

fn checked_transport(system: &System<'_>, supervisor: usize) -> Result<(), Error>
{
    let text = b"ISOLATION_TRANSPORT_VALID_REQUEST\n";
    let mut call = LogCall { bytes:  text.as_ptr(),
                             length: text.len() };
    let address = core::ptr::from_mut(&mut call).addr();
    refused(0, abi::BAD_ARGUMENT)?;
    refused(address + 1, abi::BAD_ARGUMENT)?;
    // Align within an actual initialized executable span; permission, not an
    // accidental alignment failure, must reject this writable request snapshot.
    let instruction_address = (run as *const () as usize + 7) & !7;
    refused(instruction_address, abi::ACCESS_DENIED)?;
    call.bytes = (usize::MAX - 7) as *const u8;
    call.length = 16;
    refused(core::ptr::from_mut(&mut call).addr(), abi::BAD_ARGUMENT)?;
    call.bytes = 4096usize as *const u8;
    refused(core::ptr::from_mut(&mut call).addr(), abi::ACCESS_DENIED)?;
    call.bytes = supervisor as *const u8;
    refused(core::ptr::from_mut(&mut call).addr(), abi::ACCESS_DENIED)?;
    system.log("ISOLATION_TRANSPORT_REFUSALS_COMPLETE\n")?;
    // This succeeds only through the same real production snapshot/copy path.
    call.bytes = text.as_ptr();
    call.length = text.len();
    refused(core::ptr::from_mut(&mut call).addr(), abi::OK)?;
    system.log("ISOLATION_TRANSPORT_FOLLOWING_SERVICE\n")
}

/// Writable image state starts nonzero. A second instance of the same image
/// must observe that initial value even after the first instance changed its own.
static STATE: AtomicU64 = AtomicU64::new(0x1111_1111);

struct Line
{
    bytes:  [u8; 128],
    length: usize
}
impl core::fmt::Write for Line
{
    fn write_str(&mut self, text: &str) -> core::fmt::Result
    {
        let end = self.length.checked_add(text.len()).ok_or(core::fmt::Error)?;
        let target = self.bytes.get_mut(self.length..end).ok_or(core::fmt::Error)?;
        target.copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
    }
}
pub(super) fn log_value(system: &System<'_>, owner: mac_toolbox::ProcessId, label: &str,
                        value: u64)
                        -> Result<(), Error>
{
    use core::fmt::Write;
    let mut line = Line { bytes:  [0; 128],
                          length: 0 };
    writeln!(&mut line, "{label} app={} value=0x{value:016x}", owner.0).map_err(|_| Error::BadAbi)?;
    // One bounded log request avoids interleaving another instance between
    // a label and its value. Nothing is emitted inside the spin workload.
    system.log(core::str::from_utf8(&line.bytes[..line.length]).map_err(|_| Error::BadAbi)?)
}

pub(super) fn log_failure(system: &System<'_>, stage: &str, error: Error)
{
    use core::fmt::Write;
    let mut line = Line { bytes:  [0; 128],
                          length: 0 };
    if writeln!(&mut line, "PROCESS_FAIL stage={stage} status={} cause={error}", error.status()).is_ok()
       && let Ok(text) = core::str::from_utf8(&line.bytes[..line.length])
    {
        let _ = system.log(text);
    }
}

fn writable_state(system: &System<'_>, owner: mac_toolbox::ProcessId) -> Result<(), Error>
{
    let before = STATE.load(Ordering::SeqCst);
    log_value(system, owner, "ISOLATION_STATE_BEFORE", before)?;
    if before != 0x1111_1111
    {
        return Err(Error::BadAbi);
    }
    let expected = 0xa5a5_0000_0000_0000 ^ owner.0;
    STATE.store(expected, Ordering::SeqCst);
    log_value(system, owner, "ISOLATION_STATE_WRITTEN", STATE.load(Ordering::SeqCst))?;
    let mut spins = 0u64;
    while spins < 900_000_000
    {
        spins += 1;
        core::hint::spin_loop();
    }
    let after = STATE.load(Ordering::SeqCst);
    log_value(system, owner, "ISOLATION_STATE_AFTER_SPIN", after)?;
    if after != expected
    {
        return Err(Error::BadAbi);
    }
    Ok(())
}

/// A genuine managed capture is cancelled by real Process deactivation.
/// This is intentionally not cancellation of a broker-pending syscall envelope.
fn capture_cancel(system: &System<'_>) -> Result<(), Error>
{
    use mac_toolbox::{Point, Rect, TypeCode,
                      controls::{Part, Reflow, StandardKind, StandardState},
                      resources::ResourceKey,
                      windows::WindowSpec};

    let mut stage = "capture-process";
    let result = (|| -> Result<(), Error> {
        let processes = system.processes();
        let process = processes.current()?;
        let image = process.image.ok_or(Error::BadAbi)?;
        stage = "capture-resource";
        let resource =
            system.resources().image(image).get(ResourceKey { kind: TypeCode::new(*b"CDEF"),
                                                               id:   0 })?;
        if resource.size() == 0
        {
            return Err(Error::BadAbi);
        }
        drop(resource);
        stage = "capture-window";
        let mut window = system.windows()
                               .create_owned(WindowSpec::new(Rect::new(100, 100, 460, 280),
                                                             "Capture cancellation"))?;
        processes.activate(process.id)?;
        window.select()?;
        let controls = system.controls();
        stage = "capture-create";
        let state = StandardState::new(Rect::new(12, 12, 180, 44), "Cancel capture");
        let button =
            controls.create_standard(window.id(), StandardKind::Button, state, Reflow::No)?;
        stage = "capture-draw";
        button.control().draw()?;
        stage = "capture-original-source";
        // No app WDEF is packaged for this selector; this first requested
        // executable resource must be the original app-owned production CDEF.
        super::owned_definition(system, 1)?;
        stage = "capture-geometry";
        let info = button.control().info()?;
        let bounds = info.standard.bounds;
        if info.window != window.id() || bounds.is_empty()
        {
            return Err(Error::BadAbi);
        }
        let point = Point { h: i16::try_from((i32::from(bounds.left) + i32::from(bounds.right))
                                             / 2).map_err(|_| Error::BadAbi)?,
                            v: i16::try_from((i32::from(bounds.top) + i32::from(bounds.bottom))
                                             / 2).map_err(|_| Error::BadAbi)? };
        stage = "capture-begin";
        let begin = button.control().begin_tracking(point, system.events().uptime_ticks()?)?;
        stage = "capture-begin-result";
        if !begin.tracking
           || !begin.handled
           || begin.part != Part::Button
           || begin.standard.highlighted == 0
        {
            return Err(Error::BadAbi);
        }
        stage = "capture-hide";
        processes.hide(process.id)?;
        stage = "capture-hidden-owner";
        let hidden = processes.current()?;
        if hidden.id != process.id || !hidden.hidden || hidden.foreground
        {
            return Err(Error::BadAbi);
        }
        stage = "capture-hidden-poll";
        let cancelled =
            button.control().poll_tracking(point, true, system.events().uptime_ticks()?)?;
        stage = "capture-cancel-result";
        if cancelled.tracking
           || cancelled.action
           || cancelled.part != Part::None
           || cancelled.standard.highlighted != 0
        {
            return Err(Error::BadAbi);
        }
        stage = "capture-cancel-state";
        let after = button.control().info()?;
        if after.tracking || after.standard.highlighted != 0
        {
            return Err(Error::BadAbi);
        }
        stage = "capture-reactivate";
        processes.show_all()?;
        processes.activate(process.id)?;
        window.select()?;
        let restored = processes.current()?;
        if restored.id != process.id || restored.hidden || !restored.foreground
        {
            return Err(Error::BadAbi);
        }
        stage = "capture-reuse-begin";
        let again = button.control().begin_tracking(point, system.events().uptime_ticks()?)?;
        if !again.tracking || !again.handled || again.part != Part::Button
        {
            return Err(Error::BadAbi);
        }
        stage = "capture-reuse-release";
        let released =
            button.control().poll_tracking(point, false, system.events().uptime_ticks()?)?;
        if released.tracking || !released.action || released.standard.highlighted != 0
        {
            return Err(Error::BadAbi);
        }
        stage = "capture-dispose";
        let original = button.into_id();
        controls.control(original).dispose()?;
        super::stale_control(system, original)?;
        stage = "capture-recreate";
        let replacement =
            controls.create_standard(window.id(), StandardKind::Button, state, Reflow::No)?;
        if replacement.id() == original
        {
            return Err(Error::BadAbi);
        }
        replacement.control().draw()?;
        super::stale_control(system, original)?;
        replacement.control().info()?;
        stage = "capture-replacement-dispose";
        let next = replacement.into_id();
        controls.control(next).dispose()?;
        super::stale_control(system, original)?;
        super::stale_control(system, next)?;
        stage = "capture-close";
        window.close()?;
        system.log("IMAGE_PROBE_CAPTURE_CANCEL_PASS deactivated=true reuse=true \
                    stale=refused\nIMAGE_PROBE_PASS\n")
    })();
    if let Err(error) = result
    {
        log_failure(system, stage, error);
    }
    result
}

/// Missing selector preserves the existing public-SDK image/CDEF workload.
/// The host packager selects exactly one destructive scenario per private app.
pub(super) fn run(system: &System<'_>) -> Result<bool, Error>
{
    let process = system.processes().current()?;
    let image = process.image.ok_or(Error::BadAbi)?;
    let mut bytes = [0; 64];
    let mode = match system.resources().image(image).string(2001, &mut bytes)
    {
        Ok(mode) => mode,
        Err(Error::NotFound) => return Ok(false),
        Err(error) => return Err(error)
    };
    use core::fmt::Write;
    let mut line = Line { bytes:  [0; 128],
                          length: 0 };
    writeln!(&mut line, "ISOLATION_PROBE_BEGIN {mode} app={}", process.id.0).map_err(|_| {
                                                                                Error::BadAbi
                                                                            })?;
    system.log(core::str::from_utf8(&line.bytes[..line.length]).map_err(|_| Error::BadAbi)?)?;
    if mode == "state"
    {
        return writable_state(system, process.id).map(|()| true);
    }
    if mode == "capture-cancel"
    {
        capture_cancel(system)?;
        return Ok(true);
    }
    if mode == "checked-transport"
    {
        checked_transport(system, target(system)?)?;
        return Ok(true);
    }
    if matches!(mode, "offscreen-controls" | "offscreen-controls-stock")
    {
        super::offscreen_controls::run(system, mode == "offscreen-controls")?;
        return Ok(true);
    }
    if matches!(mode,
                "window-definition"
                | "window-definition-fault"
                | "window-definition-draw-fault"
                | "window-definition-calculate-fault"
                | "window-definition-dispose-fault")
    {
        super::window_definition::run(system, mode)?;
        return Ok(true);
    }
    if mode == "control-definition-dispose-panic"
    {
        let window=system.windows().create_owned(mac_toolbox::windows::WindowSpec::new(
            mac_toolbox::Rect::new(100,100,460,300),"Owned CDEF disposal panic"))?;
        let control = system.controls()
                            .create_static_text(window.id(),
                                                mac_toolbox::Rect::new(8, 8, 220, 28),
                                                "dispose-probe/panic",
                                                Default::default(),
                                                mac_toolbox::controls::TextAlignment::Left)?;
        control.draw()?;
        let id = control.into_control().into_id();
        system.log("ISOLATION_CDEF_DISPOSE_BEGIN create_draw_calls_returned=true\n")?;
        let result = system.controls().control(id).dispose();
        // The explicit Result must never be mistaken for a Drop marker or
        // successful disposal. A contained author panic stops this Process.
        system.log(if result.is_ok()
                   {
                       "ISOLATION_CDEF_DISPOSE_UNEXPECTED_RETURN result=OK\n"
                   }
                   else
                   {
                       "ISOLATION_CDEF_DISPOSE_UNEXPECTED_RETURN result=ERR\n"
                   })?;
        if let Err(error) = result
        {
            log_failure(system, "cdef-dispose-unexpected-return", error);
            return Err(error);
        }
        return Err(Error::BadAbi);
    }
    if mode == "control-definition-panic"
    {
        let window = system.windows().create_owned(mac_toolbox::windows::WindowSpec::new(
            mac_toolbox::Rect::new(100, 100, 460, 300), "Owned CDEF panic"))?;
        // The existing packaged diagnostic CDEF panics in its safe author
        // VALIDATE body for this title. The actual callback CRT must report it;
        // no resident test configuration or injected System trap is involved.
        let _control = system.controls()
                             .create_static_text(window.id(),
                                                 mac_toolbox::Rect::new(8, 8, 220, 28),
                                                 "panic-probe",
                                                 Default::default(),
                                                 mac_toolbox::controls::TextAlignment::Left)?;
        system.log("ISOLATION_PROBE_UNCONTAINED_DEFINITION_RETURNED\n")?;
        return Err(Error::BadAbi);
    }
    // All operations below intentionally attack this private guest's actual
    // privilege/page boundary. No Rust memory access relies on a forged pointer.
    unsafe {
        match mode
        {
            "privileged" => asm!("csrr a0, satp", lateout("a0") _),
            "breakpoint" => asm!("ebreak"),
            "null-read" => asm!("ld a0, 0(zero)", lateout("a0") _),
            "kernel-read" => asm!("ld a0, 0(t0)", in("t0") target(system)?, lateout("a0") _),
            "kernel-write" => asm!("sb zero, 0(t0)", in("t0") target(system)?),
            "text-write" => asm!("sb zero, 0(t0)", in("t0") run as *const ()),
            "heap-execute" =>
            {
                let instructions = Box::new([0x0000_0513u32, 0x0000_8067]);
                asm!("fence.i", "jalr ra, 0(t0)", in("t0") instructions.as_ptr(), clobber_abi("C"));
            }
            "stack-guard" =>
            {
                asm!("2:", "addi sp, sp, -2048", "sd zero, 0(sp)", "j 2b", options(noreturn))
            }
            _ => return Err(Error::BadArgument)
        }
    }
    system.log("ISOLATION_PROBE_UNCONTAINED_INSTRUCTION_RETURNED\n")?;
    Err(Error::BadAbi)
}
