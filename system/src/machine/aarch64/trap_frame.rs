//! The AArch64 statement of the shared trap-frame contract.
//!
//! `machine::traps` and the scheduler describe a service call as a number, a
//! request pointer, its byte length and a result, and they name the frame slots
//! that carry them. Those slots are RISC-V's: `a0` and `a1` in slots 8 and 9,
//! `a7` in slot 15, `a0` again for the result, `scause` values 8 and 9 for a
//! call, and an `ecall` that the entry must step over.
//!
//! AArch64 keeps the number in `x8`, the argument and its length in `x0`/`x1`
//! and the result in `x0`; it reports a call through `ESR_EL1`'s exception class
//! rather than through a cause value; and its `ELR_EL1` already holds the
//! instruction after `svc`. This module is the one place that states that
//! difference, so no shared caller names an architecture.
//!
//! It also answers what RISC-V answers with its cause word: which exception kind
//! a frame came from, and what an interrupt means. An AArch64 interrupt writes no
//! `ESR_EL1`, so the kind is recorded by the vector slot that took it and the
//! source is read from the controller. The dispatcher is given an [`Interrupt`]
//! and never compares a cause word to a bit pattern to decide whether a trap was
//! an interrupt at all.

use crate::machine::gic;
use crate::machine::traps::{Interrupt, TrapFrame};

/// The vector slot's exception kind. The sixteen slots are four kinds from four
/// sources, so the kind is the slot number modulo four, and the table states it
/// per slot from these constants rather than from its own copy of the numbers.
pub(crate) const EXCEPTION_SYNCHRONOUS: usize = 0;
pub(crate) const EXCEPTION_IRQ: usize = 1;
pub(crate) const EXCEPTION_FIQ: usize = 2;
pub(crate) const EXCEPTION_SERROR: usize = 3;

/// `ESR_EL1.EC`, the exception class, in bits 31:26.
const EC_SHIFT: usize = 26;
const EC_MASK: usize = 0x3f;
/// A supervisor call executed in AArch64 state.
const EC_SVC_AARCH64: usize = 0x15;

/// `SPSR_EL1.M[3:0]`: the exception level and stack-pointer selection an
/// `eret` returns to. `M[3:2]` is the level — `0b00` is EL0, `0b01` is EL1 —
/// and `M[1:0]` selects SP_EL0 at a lower level and SP_ELx at the current one.
const MODE_MASK: usize = 0xf;
const MODE_EL0T: usize = 0b0000;
const MODE_EL1H: usize = 0b0101;

/// The status a System-owned frame resumes at: EL1h with DAIF unmasked, so the
/// idle and module-worker frames run privileged with the timer able to
/// preempt them.
///
/// This is a real `SPSR_EL1` word. It used to be RISC-V's `sstatus` value
/// `0x66120`, whose `M[3:0]` is zero: System's own frames were entered at
/// `EL0t` and reached kernel memory only because the page encoder granted EL0
/// every supervisor page. The word's `SPP` bit (bit 8) was being read as if it
/// were `SPSR_EL1.A` to call those frames privileged.
pub(crate) const INITIAL_STATUS: usize = MODE_EL1H;

/// The status an application frame resumes at: `EL0t` with DAIF unmasked, so a
/// running application can be preempted by the timer.
pub(crate) const USER_STATUS: usize = MODE_EL0T;

// The two words must stay on the side of the mode test their name claims.
const _: () = assert!(INITIAL_STATUS & MODE_MASK == MODE_EL1H
                      && USER_STATUS & MODE_MASK == MODE_EL0T);

/// Whether this frame was taken from unprivileged execution.
///
/// The architectural test is `SPSR_EL1.M[3:0]`. It is not `SPSR_EL1.A`, which
/// was used while the frame words were RISC-V `sstatus` values: `A` is the
/// SError mask, it is always clear at EL0, and it is clear at EL1 whenever
/// SError happens to be unmasked, so an EL1 frame would have been classified
/// as an application call.
pub(crate) fn from_user(frame: &TrapFrame) -> bool
{
    frame.status & MODE_MASK == MODE_EL0T
}

/// Whether the interrupted context had interrupts enabled, so this frame may be
/// suspended or moved.
///
/// `SPSR_EL1` holds the interrupted `PSTATE`, so bit 7 is `PSTATE.I` as it was
/// and a clear bit means IRQ was unmasked. This is the same bit
/// `page_layout::interrupts_enabled` reads, but the two are stated separately:
/// that one answers for a live `DAIF` read, this one for a saved frame, and
/// RISC-V's two answers are genuinely different bits.
pub(crate) fn interrupts_were_enabled(frame: &TrapFrame) -> bool
{
    frame.status & (1 << 7) == 0
}

/// The cause the shared dispatcher classifies.
///
/// The dispatcher names the call cause it handles for each origin; AArch64
/// reports a call as an exception class instead. Translate exactly that class,
/// and only from a synchronous trap: an IRQ, FIQ or SError writes no `ESR_EL1`,
/// so the word in the frame is whatever the last synchronous trap left there,
/// and reading it as an exception class is how an interrupt would be classified
/// as a service call. Every other cause is returned unchanged, so a genuine
/// fault is still reported with the value the hardware produced.
pub(crate) fn cause(frame: &TrapFrame) -> usize
{
    if frame.exception != EXCEPTION_SYNCHRONOUS
    {
        return frame.cause;
    }
    if (frame.cause >> EC_SHIFT) & EC_MASK == EC_SVC_AARCH64
    {
        if from_user(frame) { 8 } else { 9 }
    }
    else
    {
        frame.cause
    }
}

/// The interrupt this frame was entered for, if it was entered for one.
///
/// The source is read from the controller and completed there, so the caller
/// never sees a half-serviced interrupt. A source nothing in the resident path
/// enabled is still completed, and is reported as unhandled rather than as a
/// tick.
pub(crate) fn interrupt(frame: &TrapFrame) -> Option<Interrupt>
{
    if frame.exception != EXCEPTION_IRQ
    {
        return None;
    }
    Some(gic::service())
}

/// The service number, which this architecture carries in `x8`.
pub(crate) fn number(frame: &TrapFrame) -> u32
{
    frame.registers[8] as u32
}

/// The request pointer, which this architecture carries in `x0`.
pub(crate) fn argument(frame: &TrapFrame) -> *mut ()
{
    frame.registers[0] as *mut ()
}

/// The request length, which this architecture carries in `x1`.
pub(crate) fn bytes(frame: &TrapFrame) -> usize
{
    frame.registers[1]
}

/// The address of the call instruction itself. `ELR_EL1` holds the instruction
/// after the `svc`, which is the address this frame resumes at.
pub(crate) fn call_site(frame: &TrapFrame) -> usize
{
    frame.pc.wrapping_sub(4)
}

/// The result already committed to the frame, from `x0`.
pub(crate) fn result(frame: &TrapFrame) -> usize
{
    frame.registers[0]
}

/// Commit a result to the frame's return-value slot, `x0`.
pub(crate) fn set_result(frame: &mut TrapFrame, value: usize)
{
    frame.registers[0] = value;
}

/// Set the link a newly entered context returns through. RISC-V holds it in
/// `ra`, which is `registers[0]`; this architecture holds it in `x30`, which is
/// its own frame field and what the entry restores into `x30`.
pub(crate) fn set_return_link(frame: &mut TrapFrame, value: usize)
{
    frame.link_register = value;
}

/// Step over the call instruction. `ELR_EL1` already points at the instruction
/// after `svc`, so this frame's `pc` needs no adjustment.
pub(crate) fn advance(_frame: &mut TrapFrame)
{
}

/// Put the frame back on the `svc` a parked continuation must execute again.
/// `ELR_EL1` holds the instruction after the `svc`, so a replay steps back one
/// instruction; `advance` is the opposite of this and is a no-op here because a
/// completed call already resumes past the call.
pub(crate) fn rewind(frame: &mut TrapFrame)
{
    frame.pc = frame.pc.wrapping_sub(4);
}
