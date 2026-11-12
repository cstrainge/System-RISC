//! Device interrupts: the line table between the machine's interrupt
//! controller and the drivers that asked to be told.
//!
//! The trap path does three things and no more: the controller backend says
//! which line fired and masks that one line, [`raised`] marks it pending here,
//! and the trap asks for a reschedule. Nothing in that path takes a lock,
//! allocates, or runs a driver. [`pump`] then runs on an ordinary turn of the
//! scheduler, beside the driver job pump, and turns each pending line into one
//! call: a kernel resident's handler, or SERVICE on the interrupt class of the
//! module that took the line. When the call returns the line is unmasked.
//!
//! So a line is masked from the moment it fires until its driver has run, a
//! level-triggered device cannot storm the trap path, and a driver has nothing
//! to acknowledge. What a driver can still do is return without quieting its
//! device; the line then fires again at once, and one that keeps doing so
//! faster than any device would is left masked and reported.
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use mac_abi::{ACCESS_DENIED, NO_MEMORY, NOT_FOUND, NOT_SUPPORTED, SERVICE_BUSY, Status};
use mac_interrupt_interface::{ALARM, CLASS, SERVICE, VERSION};

const SLOTS: usize = 32;
const FREE: u32 = u32::MAX;
/// `platform::ticks` runs at 10 MHz; the storm window is one second of it.
const WINDOW_TICKS: u64 = 10_000_000;
/// More deliveries than this in one window is a driver that is not quieting
/// its device, not a device: a full-speed USB controller's start-of-frame
/// interrupt, the busiest ordinary source, is a thousand a second.
const STORM: u32 = 50_000;

struct Slot
{
    line:     AtomicU32,
    /// The module that took the line, or 0 for a kernel resident.
    module:   AtomicU64,
    /// A kernel resident's handler, as a `fn(u32)`, or 0.
    handler:  AtomicUsize,
    pending:  AtomicBool,
    /// Left masked after a storm or a delivery that cannot succeed.
    parked:   AtomicBool,
    waiting:  AtomicBool,
    /// When the owner's alarm falls due, in `platform::ticks`, or 0 for none.
    alarm:    AtomicU64,
    window:   AtomicU64,
    count:    AtomicU32,
    serviced: AtomicU64
}
impl Slot
{
    const fn new() -> Self
    {
        Self { line:     AtomicU32::new(FREE),
               module:   AtomicU64::new(0),
               handler:  AtomicUsize::new(0),
               pending:  AtomicBool::new(false),
               parked:   AtomicBool::new(false),
               waiting:  AtomicBool::new(false),
               alarm:    AtomicU64::new(0),
               window:   AtomicU64::new(0),
               count:    AtomicU32::new(0),
               serviced: AtomicU64::new(0) }
    }
}
static TABLE: [Slot; SLOTS] = [const { Slot::new() }; SLOTS];
static ANY_PENDING: AtomicBool = AtomicBool::new(false);
static STRAY_REPORTED: AtomicBool = AtomicBool::new(false);
/// Whether any slot has an alarm set. Alarms are set and delivered on
/// scheduler turns only, never from a trap.
static ANY_ALARM: AtomicBool = AtomicBool::new(false);
/// `platform::ticks` per microsecond.
const TICKS_PER_MICROSECOND: u64 = 10;

fn slot_of(line: u32) -> Option<&'static Slot>
{
    TABLE.iter().find(|slot| slot.line.load(Ordering::Acquire) == line)
}

/// The trap path's whole part. The backend has already masked `line`.
pub(crate) fn raised(line: u32)
{
    match slot_of(line)
    {
        Some(slot) =>
        {
            slot.pending.store(true, Ordering::Release);
            ANY_PENDING.store(true, Ordering::Release);
        }
        None =>
        {
            // Nobody took it, so nobody will unmask it: it stays masked.
            if !STRAY_REPORTED.swap(true, Ordering::Relaxed)
            {
                crate::machine::emergency::serial(format_args!("DEVICE_INTERRUPT_STRAY line={line}: \
                                                                no driver took this line; left \
                                                                masked\n"));
            }
        }
    }
}

fn take(line: u32, module: u64, handler: usize) -> Result<(), Status>
{
    if line == FREE
    {
        return Err(NOT_SUPPORTED);
    }
    super::interrupts::with_masked(|| {
        if slot_of(line).is_some()
        {
            return Err(ACCESS_DENIED);
        }
        let slot = TABLE.iter()
                        .find(|slot| slot.line.load(Ordering::Acquire) == FREE)
                        .ok_or(NO_MEMORY)?;
        slot.module.store(module, Ordering::Release);
        slot.handler.store(handler, Ordering::Release);
        slot.pending.store(false, Ordering::Release);
        slot.parked.store(false, Ordering::Release);
        slot.waiting.store(false, Ordering::Release);
        slot.alarm.store(0, Ordering::Release);
        slot.count.store(0, Ordering::Release);
        slot.serviced.store(0, Ordering::Release);
        slot.line.store(line, Ordering::Release);
        if !super::interrupts::enable_line(line)
        {
            slot.line.store(FREE, Ordering::Release);
            return Err(NOT_SUPPORTED);
        }
        Ok(())
    })
}

/// A driver module takes a line. System calls its interrupt class from then on.
pub(crate) fn take_for_module(line: u32, module: u64) -> Result<(), Status>
{
    take(line, module, 0)?;
    serial!("DEVICE_INTERRUPT_TAKEN line={line} module={module}");
    Ok(())
}

/// A kernel resident takes a line. `handler` runs from [`pump`], never from the
/// trap, and the line is unmasked when it returns.
pub(crate) fn take_for_kernel(line: u32, handler: fn(u32)) -> Result<(), Status>
{
    take(line, 0, handler as usize)?;
    serial!("DEVICE_INTERRUPT_TAKEN line={line} resident");
    Ok(())
}

fn free(slot: &Slot)
{
    let line = slot.line.load(Ordering::Acquire);
    super::interrupts::mask_line(line);
    slot.line.store(FREE, Ordering::Release);
    slot.pending.store(false, Ordering::Release);
    slot.alarm.store(0, Ordering::Release);
}

/// Sets, replaces or (with zero) cancels the alarm of the module holding
/// `line`. It is delivered from [`pump`], on the first scheduler turn at or
/// after the time asked for.
pub(crate) fn set_alarm(line: u32, module: u64, microseconds: u64) -> Result<(), Status>
{
    let slot = slot_of(line).ok_or(NOT_FOUND)?;
    if slot.module.load(Ordering::Acquire) != module
    {
        return Err(ACCESS_DENIED);
    }
    if microseconds == 0
    {
        slot.alarm.store(0, Ordering::Release);
        return Ok(());
    }
    let due = crate::platform::ticks()
              .saturating_add(microseconds.saturating_mul(TICKS_PER_MICROSECOND))
              .max(1);
    slot.alarm.store(due, Ordering::Release);
    ANY_ALARM.store(true, Ordering::Release);
    Ok(())
}

/// Delivers every alarm that has fallen due.
fn pump_alarms()
{
    if !ANY_ALARM.load(Ordering::Acquire)
    {
        return;
    }
    let now = crate::platform::ticks();
    for slot in &TABLE
    {
        let due = slot.alarm.load(Ordering::Acquire);
        let line = slot.line.load(Ordering::Acquire);
        if due == 0 || line == FREE || now < due
        {
            continue;
        }
        slot.alarm.store(0, Ordering::Release);
        let module = slot.module.load(Ordering::Acquire);
        match call_module(module, ALARM)
        {
            Ok(()) => (),
            // In another call: it is due, so it is offered again next turn,
            // unless the driver set a new one in the meantime.
            Err(SERVICE_BUSY) | Err(NOT_FOUND) =>
            {
                if slot.alarm.load(Ordering::Acquire) == 0
                {
                    slot.alarm.store(now.max(1), Ordering::Release);
                }
            }
            Err(status) =>
            {
                serial!("DEVICE_ALARM_REFUSED line={line} module={module} status={status}: the \
                         module's interrupt class refused its alarm; it is dropped");
            }
        }
    }
    // A driver sets its next alarm from inside the call, so what is left is
    // read after every delivery has returned.
    ANY_ALARM.store(TABLE.iter().any(|slot| slot.alarm.load(Ordering::Acquire) != 0),
                    Ordering::Release);
}

/// A module gives one line back.
pub(crate) fn release(line: u32, module: u64) -> Result<(), Status>
{
    super::interrupts::with_masked(|| {
        let slot = slot_of(line).ok_or(NOT_FOUND)?;
        if slot.module.load(Ordering::Acquire) != module
        {
            return Err(ACCESS_DENIED);
        }
        free(slot);
        Ok(())
    })
}

/// A claim is being released or cleaned up: the line its device states is
/// masked and freed if a module held it. A kernel resident's line is not a
/// claim's and is left alone.
pub(crate) fn release_line(line: u32)
{
    super::interrupts::with_masked(|| {
        if let Some(slot) = slot_of(line)
           && slot.module.load(Ordering::Acquire) != 0
        {
            free(slot);
        }
    });
}

fn call_module(module: u64, line: u32) -> Result<(), Status>
{
    let provider = crate::drivers::module_interface(module, CLASS)?;
    let subject = crate::drivers::jobs::subjects::resident()?.admit(
        &crate::toolbox::handles::subject::Authority::Client(crate::toolbox::handles::system_owner()?),
        crate::allocation::Target::System(crate::allocation::SystemClass::Ordinary))?;
    let mut input = alloc::vec::Vec::new();
    input.try_reserve_exact(4).map_err(|_| NO_MEMORY)?;
    input.extend_from_slice(&line.to_le_bytes());
    let mut job =
        crate::drivers::submit_background_to(&subject, provider, CLASS, VERSION, SERVICE, input, 0)?;
    let completed = job.run_once()?.ok_or(SERVICE_BUSY)?;
    match completed.result
    {
        Ok(_) => Ok(()),
        Err(crate::drivers::jobs::Failure::Service(status)) => Err(status),
        Err(crate::drivers::jobs::Failure::Cancelled) => Err(mac_abi::OPERATION_CANCELLED)
    }
}

fn again(slot: &Slot)
{
    slot.pending.store(true, Ordering::Release);
    ANY_PENDING.store(true, Ordering::Release);
}

/// Whether this delivery is one too many for the window it falls in.
fn storming(slot: &Slot) -> bool
{
    let now = crate::platform::ticks();
    if now.wrapping_sub(slot.window.load(Ordering::Relaxed)) > WINDOW_TICKS
    {
        slot.window.store(now, Ordering::Relaxed);
        slot.count.store(0, Ordering::Relaxed);
    }
    slot.count.fetch_add(1, Ordering::Relaxed) >= STORM
}

/// Delivers every pending line. Runs on an ordinary scheduler turn.
pub(crate) fn pump()
{
    pump_alarms();
    if !ANY_PENDING.swap(false, Ordering::AcqRel)
    {
        return;
    }
    for slot in &TABLE
    {
        let line = slot.line.load(Ordering::Acquire);
        if line == FREE || !slot.pending.swap(false, Ordering::AcqRel)
        {
            continue;
        }
        if slot.parked.load(Ordering::Acquire)
        {
            continue;
        }
        if storming(slot)
        {
            slot.parked.store(true, Ordering::Release);
            serial!("DEVICE_INTERRUPT_STORM line={line} module={}: more than {STORM} deliveries \
                     in a second; the driver is not quieting its device, so the line stays masked",
                    slot.module.load(Ordering::Acquire));
            continue;
        }
        let handler = slot.handler.load(Ordering::Acquire);
        if handler != 0
        {
            // SAFETY: the only writer stores a `fn(u32)` taken by
            // `take_for_kernel`, and the slot's line was checked live above.
            let handler: fn(u32) = unsafe { core::mem::transmute(handler) };
            handler(line);
        }
        else
        {
            let module = slot.module.load(Ordering::Acquire);
            match call_module(module, line)
            {
                Ok(()) => (),
                // The module is in another call, or has not finished starting.
                // The line stays masked and is offered again on the next turn.
                Err(SERVICE_BUSY) | Err(NOT_FOUND) =>
                {
                    if !slot.waiting.swap(true, Ordering::Relaxed)
                    {
                        serial!("DEVICE_INTERRUPT_WAITING line={line} module={module}: the \
                                 module is not ready to be called; the line stays masked until \
                                 it is");
                    }
                    again(slot);
                    continue;
                }
                Err(status) =>
                {
                    slot.parked.store(true, Ordering::Release);
                    serial!("DEVICE_INTERRUPT_REFUSED line={line} module={module} \
                             status={status}: the module's interrupt class refused the call; the \
                             line stays masked");
                    continue;
                }
            }
        }
        if slot.serviced.fetch_add(1, Ordering::Relaxed) == 0
        {
            // Said once per line: the whole path from the device to its
            // driver and back has run.
            serial!("DEVICE_INTERRUPT_SERVICED line={line} module={}: first delivery returned; \
                     line unmasked",
                    slot.module.load(Ordering::Acquire));
        }
        // Still this owner's line: a release during the call already masked it.
        if slot.line.load(Ordering::Acquire) == line
        {
            super::interrupts::unmask_line(line);
        }
    }
}
