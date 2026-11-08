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
//! vector registers are 128 bits rather than 64. The return link (`x30`) and the
//! software thread pointer (`TPIDR_EL0`) have dedicated frame fields because the
//! shared 30-slot file cannot hold them; they are saved before the first call
//! and restored before `eret`, so a preempted context resumes with its own TLS
//! base and return address. The floating-point and vector portion of the frame
//! is still not carried here. That is a real gap: a fault that occurs with live
//! floating-point state will not have it preserved across the report. Integer
//! state, the fault cause, the faulting address and the return state are all
//! carried, which is what the diagnostic path needs.
use core::arch::global_asm;

use crate::machine::traps::{CPU_SLOTS, TrapFrame, hart};
use crate::machine::trap_frame::{EXCEPTION_FIQ, EXCEPTION_IRQ, EXCEPTION_SERROR,
                                 EXCEPTION_SYNCHRONOUS};

global_asm!(include_str!("trap_entry.S"),
    frame_bytes=const core::mem::size_of::<TrapFrame>(),
    saved_sp=const core::mem::offset_of!(TrapFrame,stack_pointer),
    prior_depth=const core::mem::offset_of!(TrapFrame,prior_depth),
    pc=const core::mem::offset_of!(TrapFrame,pc),
    status=const core::mem::offset_of!(TrapFrame,status),
    cause=const core::mem::offset_of!(TrapFrame,cause),
    value=const core::mem::offset_of!(TrapFrame,value),
    registers=const core::mem::offset_of!(TrapFrame,registers),
    thread_pointer=const core::mem::offset_of!(TrapFrame,thread_pointer),
    link_register=const core::mem::offset_of!(TrapFrame,link_register),
    exception=const core::mem::offset_of!(TrapFrame,exception),
    exception_synchronous=const EXCEPTION_SYNCHRONOUS,
    exception_irq=const EXCEPTION_IRQ,
    exception_fiq=const EXCEPTION_FIQ,
    exception_serror=const EXCEPTION_SERROR,
    ctl_entering=const core::mem::offset_of!(hart::Control,entering),
    ctl_depth=const core::mem::offset_of!(hart::Control,depth),
    ctl_frames=const core::mem::offset_of!(hart::Control,frames),
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
