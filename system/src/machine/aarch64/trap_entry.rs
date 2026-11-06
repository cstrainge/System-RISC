//! AArch64 exception entry.
//!
//! The vector table has sixteen slots in four groups: synchronous, IRQ, FIQ and
//! SError, each from the current level with SP0, the current level with SPx,
//! a lower level in AArch64, and a lower level in AArch32. Every slot converges
//! on one entry that builds the frame the shared dispatch expects, calls it, and
//! returns.
//!
//! The shared `TrapFrame` is RISC-V-shaped: it carries 30 integer registers and
//! the RISC-V vector state. AArch64 has 31 general registers plus SP, and its
//! vector registers are 128 bits rather than 64, so the floating-point and vector
//! portion of the frame is not yet carried here. That is a real gap: a fault that
//! occurs with live floating-point state will not have it preserved across the
//! report. Integer state, the fault cause, the faulting address and the return
//! state are all carried, which is what the diagnostic path needs.
use core::arch::global_asm;

use crate::machine::traps::{CPU_SLOTS, TrapFrame, hart};

global_asm!(include_str!("trap_entry.S"),
    frame_bytes=const core::mem::size_of::<TrapFrame>(),
    saved_sp=const core::mem::offset_of!(TrapFrame,stack_pointer),
    prior_depth=const core::mem::offset_of!(TrapFrame,prior_depth),
    pc=const core::mem::offset_of!(TrapFrame,pc),
    status=const core::mem::offset_of!(TrapFrame,status),
    cause=const core::mem::offset_of!(TrapFrame,cause),
    value=const core::mem::offset_of!(TrapFrame,value),
    registers=const core::mem::offset_of!(TrapFrame,registers),
    ctl_entering=const core::mem::offset_of!(hart::Control,entering),
    ctl_depth=const core::mem::offset_of!(hart::Control,depth),
    ctl_frames=const core::mem::offset_of!(hart::Control,frames),
    ctl_top=const core::mem::offset_of!(hart::Control,top),
    ctl_sp=const core::mem::offset_of!(hart::Control,interrupted_sp),
    cpu_slots=const CPU_SLOTS,
    uart=const crate::machine::memory::UART_BASE);

/// The vector table this module installs.
pub(crate) fn vector() -> usize
{
    unsafe extern "C" {
        fn __system_trap_vector();
    }
    // SAFETY: taking the address of a defined symbol has no side effects.
    unsafe { __system_trap_vector as *const () as usize }
}
