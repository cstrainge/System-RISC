//! The RISC-V statement of the shared trap-frame call contract.
//!
//! Every slot the shared dispatcher names, and both call causes it accepts, are
//! this architecture's own convention: `a0`/`a1` carry the argument and its
//! length in slots 8 and 9, `a7` carries the number in slot 15, `a0` carries the
//! result, `scause` already reports a call as 8 from user mode and 9 from
//! supervisor mode, and `sepc` holds the `ecall` itself, which the dispatcher
//! must step over. Every accessor is therefore the identity. The module exists
//! so that the shared callers have one statement to read rather than a
//! conditional of their own.
//!
//! Interrupts need no translation here either: `scause` states the kind in its
//! own high bit and names the source in the low bits, so this module reads the
//! two sources the scheduler acts on out of it and completes the software one,
//! which is a pending bit this architecture owns.

use crate::machine::cpu;
use crate::machine::traps::{Interrupt, TrapFrame};

/// `scause` sets bit 63 for an interrupt and carries the code below it.
const INTERRUPT_BIT: usize = 1 << 63;
/// Supervisor software interrupt: a peer asking this hart to look at the
/// scheduler.
const CAUSE_SOFTWARE: usize = INTERRUPT_BIT | 1;
/// Supervisor timer interrupt: the running quantum expired.
const CAUSE_TIMER: usize = INTERRUPT_BIT | 5;
/// A supervisor external interrupt: a device's line, through the
/// platform-level interrupt controller.
const CAUSE_EXTERNAL: usize = INTERRUPT_BIT | 9;

/// `sstatus` bit 8 is `SPP`: set when the trap was taken from supervisor mode,
/// clear from user mode.
const SPP: usize = 1 << 8;
/// `sstatus` bit 5 is `SPIE`: the interrupt-enable state before the trap.
const SPIE: usize = 1 << 5;

/// The `sstatus` a System-owned frame resumes at: supervisor mode (`SPP`), the
/// previous interrupt-enable bit set so `sret` leaves interrupts on, and the
/// floating-point state marked dirty. `SIE` itself is clear while the handler
/// runs; `sret` copies `SPIE` into it.
pub(crate) const INITIAL_STATUS: usize = 0x120 | 0x6600;

/// The `sstatus` an application frame resumes at: user mode (`SPP` clear),
/// interrupts enabled after `sret`, and `SUM`/`MXR` clear so the kernel cannot
/// read application memory through this word.
pub(crate) const USER_STATUS: usize = (INITIAL_STATUS & !SPP) & !((1 << 18) | (1 << 19));

// The two words must stay on the side of the origin test their name claims.
const _: () = assert!(INITIAL_STATUS & SPP != 0 && USER_STATUS & SPP == 0);

/// Whether this frame was taken from unprivileged execution.
pub(crate) fn from_user(frame: &TrapFrame) -> bool
{
    frame.status & SPP == 0
}

/// Whether the interrupted context had interrupts enabled, so this frame may be
/// suspended or moved.
///
/// `sstatus.SIE` is cleared by the trap before the entry reads the register, so
/// a saved frame carries the pre-trap state in `SPIE` and a clear `SIE`.
/// `page_layout::interrupts_enabled` answers the other question — whether the
/// status word *now* has interrupts enabled — and would refuse every saved
/// frame if it were used here. That substitution was made and caught by the
/// `qemu_riscv` gate: every application call was refused as `WrongThread`.
pub(crate) fn interrupts_were_enabled(frame: &TrapFrame) -> bool
{
    frame.status & SPIE != 0
}

/// The cause the shared dispatcher classifies: `scause` already names a call.
pub(crate) fn cause(frame: &TrapFrame) -> usize
{
    frame.cause
}

/// The interrupt this frame was entered for, if it was entered for one.
///
/// The software source is a pending bit rather than a controller latch, so it is
/// cleared here - before the wake is acknowledged, and before any frame
/// reference survives into that acknowledgement - or the return would retrap
/// immediately.
pub(crate) fn interrupt(frame: &TrapFrame) -> Option<Interrupt>
{
    match frame.cause
    {
        CAUSE_SOFTWARE =>
        {
            cpu::clear_software_interrupt();
            Some(Interrupt::Wake)
        }
        CAUSE_TIMER => Some(Interrupt::Preempt),
        CAUSE_EXTERNAL => Some(super::interrupts::service()),
        _ => None
    }
}

/// The service number, which this architecture carries in `a7`.
pub(crate) fn number(frame: &TrapFrame) -> u32
{
    frame.registers[15] as u32
}

/// The request pointer, which this architecture carries in `a0`.
pub(crate) fn argument(frame: &TrapFrame) -> *mut ()
{
    frame.registers[8] as *mut ()
}

/// The request length, which this architecture carries in `a1`.
pub(crate) fn bytes(frame: &TrapFrame) -> usize
{
    frame.registers[9]
}

/// The address of the call instruction itself, which `sepc` holds.
pub(crate) fn call_site(frame: &TrapFrame) -> usize
{
    frame.pc
}

/// The result already committed to the frame, in `a0`.
pub(crate) fn result(frame: &TrapFrame) -> usize
{
    frame.registers[8]
}

/// Commit a result to the frame's return-value slot, `a0`.
pub(crate) fn set_result(frame: &mut TrapFrame, value: usize)
{
    frame.registers[8] = value;
}

/// Set the link a newly entered context returns through, `ra`.
pub(crate) fn set_return_link(frame: &mut TrapFrame, value: usize)
{
    frame.registers[0] = value;
}

/// Step over the `ecall`, which `sepc` still points at.
pub(crate) fn advance(frame: &mut TrapFrame)
{
    frame.pc = frame.pc.wrapping_add(4);
}

/// Leave the frame on the `ecall` a parked continuation must execute again.
/// `sepc` already names the call instruction, so a replay steps nowhere;
/// `advance` is the opposite of this and moves past the call.
pub(crate) fn rewind(_frame: &mut TrapFrame)
{
}

/// The cause, named as a reader of this architecture expects to find it.
///
/// `scause` already carries this architecture's own cause numbers, so the
/// diagnostic states them as they are: the number *is* the description.
pub(crate) fn write_exception(output: &mut core::fmt::Formatter<'_>, frame: &TrapFrame)
                              -> core::fmt::Result
{
    let cause = match frame.cause
    {
        0 => "misaligned instruction address",
        1 => "instruction access fault",
        2 => "illegal or privileged instruction",
        3 => "breakpoint",
        4 => "misaligned load",
        5 => "load access fault",
        6 => "misaligned store",
        7 => "store access fault",
        12 => "instruction page fault",
        13 => "load page fault",
        15 => "store page fault",
        _ => "unhandled user exception"
    };
    writeln!(output, "User fault: {cause} (cause {}).", frame.cause)
}

/// The interrupted frame under this architecture's own names: `sepc`, the
/// faulting address, `sp`, `sstatus`, then the register file in the order the
/// 30 shared slots hold it — `ra` first, `sp` and `x0` omitted.
pub(crate) fn write_frame(output: &mut core::fmt::Formatter<'_>, frame: &TrapFrame)
                          -> core::fmt::Result
{
    writeln!(output, "PC: {:#018x}  address: {:#018x}", frame.pc, frame.value)?;
    writeln!(output, "SP: {:#018x}  status: {:#018x}", frame.stack_pointer, frame.status)?;
    for (name, value) in ["ra", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2",
                          "a3", "a4", "a5", "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7",
                          "s8", "s9", "s10", "s11", "t3", "t4", "t5", "t6"].into_iter()
                                                                           .zip(frame.registers)
    {
        writeln!(output, "{name}: {value:#018x}")?;
    }
    Ok(())
}

/// Whether this cause names an attempt to reach memory the context may not
/// touch, as opposed to a deliberate or undefined instruction.
pub(crate) fn is_access_fault(frame: &TrapFrame) -> bool
{
    matches!(frame.cause, 0 | 1 | 4 | 5 | 6 | 7 | 12 | 13 | 15)
}

/// The termination code this exception stands for: 128 plus the cause, the
/// shell convention for a signal-like death, bounded by the cause's own range.
pub(crate) fn abort_code(frame: &TrapFrame) -> i32
{
    128 + frame.cause.min(127) as i32
}
