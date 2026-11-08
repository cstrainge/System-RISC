//! The AArch64 statement of the shared trap-frame call contract.
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
//! Interrupt causes are deliberately absent. AArch64 delivers them through the
//! GIC and never writes `ESR_EL1` for one, so the shared software-interrupt and
//! timer comparisons cannot match an AArch64 frame. That is also why this board
//! must not unmask IRQ yet: with a stale `ESR_EL1` still holding a call's
//! exception class, an interrupt would be classified as a call. Recording the
//! vector slot belongs with the GIC path in the multi-scheduler extension.

use crate::machine::traps::TrapFrame;

/// `ESR_EL1.EC`, the exception class, in bits 31:26.
const EC_SHIFT: usize = 26;
const EC_MASK: usize = 0x3f;
/// A supervisor call executed in AArch64 state.
const EC_SVC_AARCH64: usize = 0x15;

/// The cause the shared dispatcher classifies.
///
/// The dispatcher names the call cause it handles for each origin; AArch64
/// reports a call as an exception class instead. Translate exactly that class
/// and return every other `ESR_EL1` unchanged, so a genuine fault is still
/// reported with the value the hardware produced.
pub(crate) fn cause(esr: usize, from_user: bool) -> usize
{
    if (esr >> EC_SHIFT) & EC_MASK == EC_SVC_AARCH64
    {
        if from_user { 8 } else { 9 }
    }
    else
    {
        esr
    }
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
