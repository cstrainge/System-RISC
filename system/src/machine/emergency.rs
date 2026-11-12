//! Terminal failure boundary. This never resumes the suspended execution or
//! unwinds its locks. Bootstrap pins independent display, stack and input
//! backing; only the sole terminal winner may borrow it mutably afterwards.
use core::{cell::UnsafeCell,
           fmt::{self, Write},
           marker::PhantomData};

use mac_kernel_model::emergency::{EmergencyManifest, EmergencyService, Requirement};

use super::{paging::ProtectedRange,
            traps::execution::{TerminalClaim, TerminalOwnership}};
use crate::machine::input;
use crate::emergency::{Action, Cursor, Kind, Record, Screen};

struct State
{
    screen:   Screen,
    cursor:   Cursor,
    decoders: [crate::toolbox::events::input::Decoder; 2]
}
struct Kit
{
    frame:   ProtectedRange,
    stack:   ProtectedRange,
    display: crate::machine::display::PreparedTerminal,
    state:   UnsafeCell<State>
}
// SAFETY: bootstrap constructs the cell before publication. The only subsequent
// mutable access follows the actual sealed cold/hot selection's unique terminal
// claim, with SIE clear and M-monitor stop ACK from every other discovered CPU.
// A distinct peer loser waits for the physical stop; same-hart recursion never
// reenters the kit. The winner never returns or releases the claim. Kit/frame,
// device and DMA storage are globally mapped and pinned until machine reset.
unsafe impl Sync for Kit
{
}
static KIT: spin::Once<Kit> = spin::Once::new();
static CLAIMED: TerminalOwnership = TerminalOwnership::new();
/// Set once the terminal path has taken the machine. It never clears: the path
/// does not return, and re-entering it would recurse into the stop path.
static IN_TERMINAL: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[derive(Debug)]
pub(crate) enum Error
{
    Reserve(crate::machine::paging::Error),
    Display(crate::machine::display::TerminalError),
    Input(input::Error),
    Geometry(crate::emergency::GeometryError),
    Manifest(mac_kernel_model::policy::ModelError),
    Size
}
impl fmt::Display for Error
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Reserve(error) => error.fmt(out),
            Self::Display(error) => error.fmt(out),
            Self::Input(error) => error.fmt(out),
            Self::Geometry(error) => error.fmt(out),
            Self::Manifest(error) => error.fmt(out),
            Self::Size => out.write_str("terminal kit storage size overflows the platform")
        }
    }
}

/// Called after permanent roots/page pools, before GlobalAlloc and ordinary
/// services. This consumes the protected floor for the terminal only; Finder
/// recovery allowance and Save/Alert emergency-service kits remain undefined.
pub(crate) fn prepare() -> Result<(), Error>
{
    KIT.try_call_once(|| -> Result<Kit,Error> {
        let palette = crate::toolbox::theme::palette();
        let screen = Screen::new(crate::display::width(), crate::display::height(), palette)
            .map_err(Error::Geometry)?;
        let cursor = Cursor::new(crate::display::width(), crate::display::height(), palette)
            .map_err(Error::Geometry)?;
        let bytes = crate::display::width().checked_mul(crate::display::height())
            .and_then(|pixels| pixels.checked_mul(size_of::<u32>())).ok_or(Error::Size)?;
        let (frame, stack) = crate::machine::paging::reserve_terminal_storage(
            bytes, crate::machine::memory::SYSTEM_STACK_BYTES as usize).map_err(Error::Reserve)?;
        let input = input::prepare_terminal().map_err(Error::Input)?;
        // SAFETY: exclusive single-hart bootstrap, permanent independent scanout
        // lease and stable CPU-to-bus translation. The sealed capability retains
        // the separate DMA scratch until reset, including on transfer timeout.
        let display = unsafe {
            crate::machine::display::prepare_terminal(crate::machine::memory::FW_CFG_BASE as usize,
                frame.physical().0, crate::display::width(), crate::display::height(),
                |pointer, length| crate::machine::paging::bus_address(pointer as u64, length as u64))
        }.map_err(Error::Display)?;
        // These dedicated statics live in System's pinned image, outside the
        // fixed heap. Screen/Cursor/decoders are already embedded in Kit;
        // Record is created on the protected stack, so neither is added twice.
        let image_storage = (input.bytes as u64)
            .checked_add(size_of::<spin::Once<Kit>>() as u64)
            .and_then(|n| n.checked_add(size_of::<TerminalOwnership>() as u64))
            .and_then(|n| n.checked_add(crate::machine::display::PreparedTerminal::storage_layout().size() as u64))
            .ok_or(Error::Size)?;
        // Exact immutable payloads used by this terminal. Shared code, string
        // literals and the normal/terminal shared RAMFB ownership byte are not
        // extra reserve allocations and are not a dedicated terminal footprint.
        let image_assets = include_bytes!("../../assets/crashes/classic-bomb.icon").len()
            + include_bytes!("../../assets/cursors/arrow.curs").len()
            + include_bytes!("../toolbox/quickdraw/assets/chicago-12.font").len();
        let reserve_bytes = frame.bytes().checked_add(stack.bytes()).ok_or(Error::Size)?;
        let requirement = Requirement {
            service: EmergencyService::SystemPanicRestart,
            bytes: reserve_bytes.checked_add(image_storage)
                .and_then(|n| n.checked_add(image_assets as u64)).ok_or(Error::Size)?,
            alignment: crate::machine::memory::PAGE_BYTES
        };
        let requirements = [requirement];
        let manifest = EmergencyManifest::new(&requirements).map_err(Error::Manifest)?;
        let reserve = crate::machine::paging::protected_heap_statistics().ok_or(Error::Size)?;
        serial(format_args!("EMERGENCY_KIT_READY services=terminal frame_bytes={} \
            frame_backing={} stack_backing={} input_bytes={} input_alignment={} devices={} \
            state_bytes={} dma_bytes={} record_bytes={} screen_bytes={} cursor_bytes={} pinned_bytes={} \
            alignment={} protected_capacity={} protected_used={} image_storage_bytes={} \
            image_asset_payload_bytes={} protected_cpu={:#x} protected_physical={:#x} \
            heap_backing_bytes={} full_recovery_kit=false",
            bytes, frame.bytes(), stack.bytes(), input.bytes, input.alignment,
            input.devices, size_of::<spin::Once<Kit>>() + size_of::<TerminalOwnership>(),
            crate::machine::display::PreparedTerminal::storage_layout().size(),
            size_of::<Record>(), size_of::<Screen>(),
            size_of::<Cursor>(), manifest.bytes(), manifest.alignment(),
            reserve.capacity_bytes, reserve.used_bytes, image_storage, image_assets,
            reserve.cpu_base, reserve.physical_base, reserve.full_backing_bytes));
        Ok(Kit { frame, stack, display,
            state: UnsafeCell::new(State { screen, cursor,
                decoders: [crate::toolbox::events::input::Decoder::new(),
                           crate::toolbox::events::input::Decoder::new()] }) })
    })?;
    Ok(())
}

/// Direct bounded UART output: no firmware callback, allocator, current-context
/// lookup, trace, registry or service lock. It remains usable before kit setup.
/// Whether the bootstrap console can accept a byte.
///
/// Selected from the console kind the record declares rather than from the
/// architecture: a board of any architecture can carry either part, and polling
/// the wrong status bit produces silence instead of an error, which is the worst
/// way for a diagnostic path to fail.
///
/// The two parts do not agree on the sense of bit 0x20, which is the trap here.
/// A 16550's line status bit 5 is *transmitter holding empty*, so it is ready
/// when the bit is set. A PL011's flag register bit 5 is *transmit FIFO full*, so
/// it is ready when the bit is clear. Reading one as the other yields a console
/// that never reports ready, gives up after its bounded spin, and discards every
/// diagnostic without a fault.
fn console_ready() -> bool
{
    // SAFETY: the selected platform's permanently mapped bootstrap console.
    // Volatile access creates no shared register borrow.
    match crate::machine::memory::CONSOLE_KIND
    {
        mac_abi::boot::console::UART_16550 =>
        {
            let status = unsafe {
                core::ptr::read_volatile((crate::machine::memory::UART_BASE + 5) as *const u8)
            };
            status & 0x20 != 0
        }
        mac_abi::boot::console::PL011 =>
        {
            let flags = unsafe {
                core::ptr::read_volatile((crate::machine::memory::UART_BASE + 0x18) as *const u32)
            };
            flags & 0x20 == 0
        }
        _ => false
    }
}

fn console_write(byte: u8)
{
    // SAFETY: as `console_ready`; the data register is word-wide on the PL011
    // and byte-wide on a 16550.
    unsafe {
        match crate::machine::memory::CONSOLE_KIND
        {
            mac_abi::boot::console::UART_16550 =>
            {
                core::ptr::write_volatile(crate::machine::memory::UART_BASE as *mut u8, byte);
            }
            mac_abi::boot::console::PL011 =>
            {
                core::ptr::write_volatile(crate::machine::memory::UART_BASE as *mut u32,
                                          u32::from(byte));
            }
            _ => {}
        }
    }
}

/// Sends the carriage return a line feed needs on a real terminal.
///
/// A line feed alone moves a terminal down without returning it to the left
/// margin, so each line starts where the last one ended. An emulator's console
/// hides this; a serial cable does not. Bounded like every other console write,
/// and a return that cannot be sent is dropped rather than waited on.
fn carriage_return()
{
    let mut sent = false;
    for _ in 0..CONSOLE_SPIN_LIMIT
    {
        if !console_ready()
        {
            core::hint::spin_loop();
            continue;
        }
        if sent
        {
            // Room again, so the line feed the caller writes next has somewhere
            // to go. Writing it straight behind the return lost it on a board:
            // the lines came out on top of one another.
            return;
        }
        console_write(b'\r');
        sent = true;
    }
}

/// How long a console write waits for the transmitter before giving up.
///
/// Bounded on purpose: a console that never reports ready must not hang the
/// kernel, and discarding the bytes is better than spinning forever.
const CONSOLE_SPIN_LIMIT: u32 = 100_000;

/// Writes bytes to System's own console.
///
/// Separate from [`serial`] because a caller that already holds bytes should not
/// round-trip them through formatting. Neither path switches translation roots,
/// which is what makes this usable from the log path: the firmware callback
/// could not report a fault inside its own root switch, so a fault there was
/// silent.
pub(crate) fn write_bytes(bytes: &[u8])
{
    for &byte in bytes
    {
        let mut ready = false;
        for _ in 0..CONSOLE_SPIN_LIMIT
        {
            if console_ready()
            {
                ready = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !ready
        {
            return;
        }
        if byte == b'\n'
        {
            carriage_return();
        }
        console_write(byte);
    }
}

pub(crate) fn serial(arguments: fmt::Arguments<'_>)
{
    struct Uart;
    impl Write for Uart
    {
        fn write_str(&mut self, text: &str) -> fmt::Result
        {
            for byte in text.bytes()
            {
                let mut ready = false;
                for _ in 0..CONSOLE_SPIN_LIMIT
                {
                    if console_ready()
                    {
                        ready = true;
                        break;
                    }
                    core::hint::spin_loop();
                }
                if !ready
                {
                    return Err(fmt::Error);
                }
                if byte == b'\n'
                {
                    carriage_return();
                }
                console_write(byte);
            }
            Ok(())
        }
    }
    let mut uart = Uart;
    let _ = uart.write_fmt(arguments);
    let _ = uart.write_str("\n");
}

fn stop(reason: fmt::Arguments<'_>) -> !
{
    serial(reason);
    super::power::fatal_shutdown()
}

/// Report a bootstrap refusal once, then stop where it is.
///
/// Deliberately not [`stop`]: that asks the board to reset, and a machine whose
/// bootstrap refuses would re-enter the same refusal forever, turning one
/// reported cause into an unreadable stream. Admission cannot be retried over
/// partially published state, so the machine stops and keeps the one message.
pub(crate) fn refuse(reason: fmt::Arguments<'_>) -> !
{
    serial(reason);
    loop
    {
        // SAFETY: nothing below can make progress. `wfi` is a low-power wait on
        // every architecture this kernel runs on, so a refusal does not spin a
        // core at full speed while an operator reads the cause.
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
    }
}

struct Invocation<'a>
{
    kind:      Kind,
    arguments: fmt::Arguments<'a>,
    _hart:     PhantomData<*mut ()>
}
pub(crate) fn enter(kind: Kind, arguments: fmt::Arguments<'_>) -> !
{
    // Never restore the interrupt state, schedule, unwind or resume a suspended
    // Rust/firmware borrow. The current authenticated row, not a hardware label,
    // owns entry.
    super::interrupts::disable();
    let winner = match CLAIMED.claim()
    {
        TerminalClaim::Winner(winner) => winner,
        TerminalClaim::Recursive =>
        {
            stop(format_args!("SYSTEM_EMERGENCY_RECURSIVE: terminal path failed"))
        }
        TerminalClaim::Peer(current) => super::monitor::wait_for_terminal_stop(current)
    };
    if let Err(error) = super::monitor::stop_peers(&winner)
    {
        stop(format_args!("SYSTEM_EMERGENCY_PEER_STOP_REFUSED: {error}"));
    }
    let Some(kit) = KIT.get()
    else
    {
        stop(format_args!("SYSTEM_EMERGENCY_UNAVAILABLE: terminal kit not installed"));
    };
    IN_TERMINAL.store(true, core::sync::atomic::Ordering::Release);
    let invocation = Invocation { kind,
                                  arguments,
                                  _hart: PhantomData };
    // SAFETY: the kit owns this independent stack until reset. Invocation and
    // its format arguments remain on pinned, abandoned storage; no scheduler or
    // teardown resumes to reclaim it. The trampoline cannot return to that stack.
    unsafe {
        // SAFETY: the terminal kit's stack is mapped, writable and owned by this
        // path; `run` never returns.
        super::cpu::terminal_stack(kit.stack.top(),
                                   run,
                                   (&invocation as *const Invocation<'_>).cast())
    }
}

/// Presents the classic "safe to turn off" notice with its Restart button.
///
/// A machine with no power-management device cannot switch itself off, so this
/// is what a shutdown request becomes there. It returns `false` when the
/// terminal path is unavailable or already running, so the caller can report and
/// halt instead of recursing into the stop path.
pub(crate) fn try_safe_to_turn_off() -> bool
{
    if IN_TERMINAL.load(core::sync::atomic::Ordering::Acquire) || KIT.get().is_none()
    {
        return false;
    }
    enter(Kind::SafeToTurnOff,
          format_args!("This machine declares no power-management device, so System cannot switch                         it off."));
}

unsafe extern "C" fn run(pointer: *const ()) -> !
{
    // SAFETY: enter lends the live, pinned invocation until reset.
    let invocation = unsafe { &*pointer.cast::<Invocation<'_>>() };
    let Some(kit) = KIT.get()
    else
    {
        stop(format_args!("SYSTEM_EMERGENCY_UNAVAILABLE"));
    };
    let record = Record::format(invocation.kind, invocation.arguments);
    let marker = match invocation.kind
    {
        Kind::SystemPanic => "SYSTEM_PANIC:",
        Kind::StartupRefused => "SYSTEM_STARTUP_REFUSED:",
        Kind::OutOfMemory => "SYSTEM_ALLOCATION_FAILED:",
        Kind::PrivilegedFault => "SYSTEM_TRAP:",
        Kind::SafeToTurnOff => "SYSTEM_SAFE_TO_TURN_OFF:"
    };
    serial(format_args!("{marker} {}", record.text()));
    // SAFETY: the authenticated terminal claim has one winner, SIE is clear,
    // and every other discovered CPU actually acknowledged irreversible stop,
    // and ordinary code never resumes. Neither region aliases normal scanout,
    // compositor/tracking storage or normal input queues.
    let state = unsafe { &mut *kit.state.get() };
    let pixels = unsafe {
        core::slice::from_raw_parts_mut(kit.frame.cpu_base() as *mut u32, crate::display::pixels())
    };
    if let Err(error) = state.screen.draw(&record, pixels)
    {
        stop(format_args!("SYSTEM_EMERGENCY_PAINT_FAILED: {error}"));
    }
    if let Err(error) = state.cursor.move_to(pixels,
                                             mac_abi::geometry::Point { h: 16, v: 444 })
    {
        stop(format_args!("SYSTEM_EMERGENCY_CURSOR_FAILED: {error}"));
    }
    if let Err(error) = unsafe { kit.display.activate() }
    {
        stop(format_args!("SYSTEM_EMERGENCY_DISPLAY_FAILED: {error}"));
    }
    let mut input = match unsafe { input::terminal_takeover() }
    {
        Ok(input) => input,
        Err(error) => stop(format_args!("SYSTEM_EMERGENCY_INPUT_FAILED: {error}"))
    };
    let button = state.screen.restart_bounds();
    let panel = state.screen.bounds();
    serial(format_args!("SYSTEM_EMERGENCY_READY kind={:?} flags={} restart={},{},{},{} \
                         panel={},{},{},{} ordinary_execution=abandoned",
                        record.kind(),
                        record.flags(),
                        button.left,
                        button.top,
                        button.right,
                        button.bottom,
                        panel.left,
                        panel.top,
                        panel.right,
                        panel.bottom));
    serial(format_args!("SYSTEM_EMERGENCY_DIAGNOSTIC {}", record.text()));
    loop
    {
        let mut restart = false;
        let failures = input.poll(|endpoint, kind, code, value| {
                                let Some(decoder) = state.decoders.get_mut(endpoint)
                                else
                                {
                                    return;
                                };
                                decoder.feed(kind, code, value, |event| {
                                    let update = state.screen.handle(event);
                                    if let Some(damage) = update.damage
                                    {
                                        if let Err(error) = paint_damage(&state.screen,
                                                                         &mut state.cursor,
                                                                         &record, pixels, damage)
                                        {
                                            stop(format_args!("SYSTEM_EMERGENCY_PAINT_FAILED: \
                                                               input damage: {error}"));
                                        }
                                    }
                                    let point = match event
                                    {
                                        crate::toolbox::events::input::Input::Move(point)
                                        | crate::toolbox::events::input::Input::Button { point,
                                                                                       .. } =>
                                        {
                                            Some(point)
                                        }
                                        _ => None
                                    };
                                    if let Some(point) = point
                                       && state.cursor.move_to(pixels, point).is_err()
                                    {
                                        stop(format_args!("SYSTEM_EMERGENCY_CURSOR_FAILED: \
                                                           input motion"));
                                    }
                                    restart |= update.action == Some(Action::Restart);
                                });
                            });
        let retired = failures.iter().any(Option::is_some);
        for error in failures.into_iter().flatten()
        {
            serial(format_args!("SYSTEM_EMERGENCY_INPUT_RETIRED: {error}"));
        }
        if restart
        {
            serial(format_args!("SYSTEM_EMERGENCY_RESTART_REQUEST"));
            super::power::emergency_restart();
        }
        if !input.has_live_device()
        {
            stop(format_args!("SYSTEM_EMERGENCY_INPUT_UNAVAILABLE: all terminal input endpoints \
                               retired; Restart cannot be serviced"));
        }
        if retired
           && let Some(damage) = state.screen.cancel_tracking().damage
           && let Err(error) =
               paint_damage(&state.screen, &mut state.cursor, &record, pixels, damage)
        {
            stop(format_args!("SYSTEM_EMERGENCY_PAINT_FAILED: input retirement: {error}"));
        }
        super::cpu::barrier();
        core::hint::spin_loop();
    }
}

/// Preserve save-under while repainting only a changed terminal button. This
/// never calls a service, allocates, or touches ordinary compositor state.
fn paint_damage(screen: &Screen, cursor: &mut Cursor, record: &Record, pixels: &mut [u32],
                damage: mac_abi::geometry::Rect)
                -> Result<(), crate::emergency::GeometryError>
{
    let point = cursor.position();
    cursor.hide(pixels)?;
    screen.draw_damage(record, pixels, damage)?;
    if let Some(point) = point
    {
        cursor.move_to(pixels, point)?;
    }
    Ok(())
}
