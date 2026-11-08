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
