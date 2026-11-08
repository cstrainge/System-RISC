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

use crate::machine::traps::TrapFrame;

/// The cause the shared dispatcher classifies: `scause` already names a call.
pub(crate) fn cause(scause: usize, _from_user: bool) -> usize
{
    scause
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
