//! The bootstrap stack trampoline for this architecture.
//!
//! Same contract as the RISC-V one: save the callee-saved set, swap to the
//! supplied stack top, call the entry, restore the frame. AArch64 saves pairs
//! with `stp`/`ldp` and keeps the frame pointer in x29.

core::arch::global_asm!(
    r#"
.global __system_boot_stack
__system_boot_stack:
sub sp, sp, #112
stp x29, x30, [sp, #0]
stp x19, x20, [sp, #16]
stp x21, x22, [sp, #32]
stp x23, x24, [sp, #48]
stp x25, x26, [sp, #64]
stp x27, x28, [sp, #80]
mov x29, sp
mov sp, x2
blr x1
mov sp, x29
ldp x29, x30, [sp, #0]
ldp x19, x20, [sp, #16]
ldp x21, x22, [sp, #32]
ldp x23, x24, [sp, #48]
ldp x25, x26, [sp, #64]
ldp x27, x28, [sp, #80]
add sp, sp, #112
ret
"#
);
