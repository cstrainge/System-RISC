//! RISC-V trap entry assembly.
//!
//! The frame layout and the control-block layout stay in shared code because they
//! are data structures; this module supplies the assembly that writes them, and
//! takes every offset from those types so the two cannot disagree. A second
//! architecture supplies its own entry that produces the same frames.
use core::arch::global_asm;

use crate::machine::traps::{CPU_SLOTS, TrapFrame, hart};

global_asm!(include_str!("traps.S"),
    frame_bytes=const core::mem::size_of::<TrapFrame>(),
    fp=const core::mem::offset_of!(TrapFrame,floating),
    fcsr=const core::mem::offset_of!(TrapFrame,fcsr),
    vl=const core::mem::offset_of!(TrapFrame,vl),
    vtype=const core::mem::offset_of!(TrapFrame,vtype),
    vstart=const core::mem::offset_of!(TrapFrame,vstart),
    vcsr=const core::mem::offset_of!(TrapFrame,vcsr),
    saved_sp=const core::mem::offset_of!(TrapFrame,stack_pointer),
    prior_depth=const core::mem::offset_of!(TrapFrame,prior_depth),
    ctl_entering=const core::mem::offset_of!(hart::Control,entering),
    ctl_depth=const core::mem::offset_of!(hart::Control,depth),
    ctl_frames=const core::mem::offset_of!(hart::Control,frames),
    ctl_stride=const core::mem::offset_of!(hart::Control,stride),
    ctl_floor=const core::mem::offset_of!(hart::Control,floor),
    ctl_top=const core::mem::offset_of!(hart::Control,top),
    ctl_tmp0=const core::mem::offset_of!(hart::Control,temporary0),
    ctl_tmp1=const core::mem::offset_of!(hart::Control,temporary1),
    ctl_tmp2=const core::mem::offset_of!(hart::Control,temporary2),
    ctl_sp=const core::mem::offset_of!(hart::Control,interrupted_sp),
    cpu_slots=const CPU_SLOTS);

/// The trap vector this assembly installs, so the shared code can install it
/// without naming a symbol the architecture owns.
pub(crate) fn vector() -> usize
{
    unsafe extern "C" {
        fn __system_trap_vector();
    }
    // SAFETY: taking the address of a defined symbol has no side effects.
    unsafe { __system_trap_vector as *const () as usize }
}
