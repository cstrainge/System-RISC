//! RISC-V trap/register boundary; diagnostic formatting is architecture independent.
use core::arch::asm;

use mac_abi::definitions::DefinitionPanic;

pub(super) fn report(diagnostic: &mut DefinitionPanic) -> !
{
    let mut result = core::ptr::from_mut(diagnostic).addr();
    // SAFETY: the synchronous trap borrows this initialized exclusive stack
    // record. No author-provided pointer or instruction encoding reaches here.
    unsafe {
        asm!("ecall", in("a7") mac_abi::traps::DEFINITION_PANIC,
            inlateout("a0") result, in("a1") core::mem::size_of::<DefinitionPanic>(),
            clobber_abi("C"));
    }
    let _ = result;
    // An authenticated panic service cannot return. A broken gate becomes an
    // explicit machine fault, not undefined behaviour or a silent spin.
    unsafe {
        asm!("unimp", options(noreturn));
    }
}
