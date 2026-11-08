// AArch64 exception entry.
//
// The vector table must be 2 KiB aligned and occupies sixteen 128-byte slots:
// four exception kinds (synchronous, IRQ, FIQ, SError) from four sources
// (current EL with SP0, current EL with SPx, lower EL AArch64, lower EL AArch32).
// The slot number is the source times four plus the kind, so which kind a slot
// is entered through is a property of the slot and not something the hardware
// records. Each slot therefore states its own kind in x3 before converging on
// the shared entry, and the entry stores it in the frame: an IRQ writes no
// ESR_EL1, so without that record the dispatcher would have to read whatever
// exception class the last synchronous trap left behind.
//
// Frame layout is the shared TrapFrame: integer registers, then the fields the
// parameters below name. The floating-point and vector portion is not carried
// yet; see the module comment in trap_entry.rs.

    .section .text.trap_entry, "ax"
    .balign 2048
    .global __system_trap_vector
__system_trap_vector:
    // Current EL with SP0: unreachable at EL1 with a stack, but the hardware
    // takes these slots if the entry ever runs on SP_EL0, so they are real
    // entries rather than a trap.
    .balign 0x80
    sub sp, sp, #16
    stp x2, x3, [sp]
    mov x3, #{exception_synchronous}
    b __system_trap_common
    .balign 0x80
    sub sp, sp, #16
    stp x2, x3, [sp]
    mov x3, #{exception_irq}
    b __system_trap_common
    .balign 0x80
    sub sp, sp, #16
    stp x2, x3, [sp]
    mov x3, #{exception_fiq}
    b __system_trap_common
    .balign 0x80
    sub sp, sp, #16
    stp x2, x3, [sp]
    mov x3, #{exception_serror}
    b __system_trap_common
    // Current EL with SPx: every privileged trap System takes.
    .balign 0x80
    sub sp, sp, #16
    stp x2, x3, [sp]
    mov x3, #{exception_synchronous}
    b __system_trap_common
    .balign 0x80
    sub sp, sp, #16
    stp x2, x3, [sp]
    mov x3, #{exception_irq}
    b __system_trap_common
    .balign 0x80
    sub sp, sp, #16
    stp x2, x3, [sp]
    mov x3, #{exception_fiq}
    b __system_trap_common
    .balign 0x80
    sub sp, sp, #16
    stp x2, x3, [sp]
    mov x3, #{exception_serror}
    b __system_trap_common
    // Lower EL, AArch64: application calls, faults and the idle loop's
    // interrupts all arrive here.
    .balign 0x80
    sub sp, sp, #16
    stp x2, x3, [sp]
    mov x3, #{exception_synchronous}
    b __system_trap_common
    .balign 0x80
    sub sp, sp, #16
    stp x2, x3, [sp]
    mov x3, #{exception_irq}
    b __system_trap_common
    .balign 0x80
    sub sp, sp, #16
    stp x2, x3, [sp]
    mov x3, #{exception_fiq}
    b __system_trap_common
    .balign 0x80
    sub sp, sp, #16
    stp x2, x3, [sp]
    mov x3, #{exception_serror}
    b __system_trap_common
    // Lower EL, AArch32: nothing this kernel runs is 32-bit, so these record a
    // kind and take the same path rather than branching into an unrelated slot.
    .balign 0x80
    sub sp, sp, #16
    stp x2, x3, [sp]
    mov x3, #{exception_synchronous}
    b __system_trap_common
    .balign 0x80
    sub sp, sp, #16
    stp x2, x3, [sp]
    mov x3, #{exception_irq}
    b __system_trap_common
    .balign 0x80
    sub sp, sp, #16
    stp x2, x3, [sp]
    mov x3, #{exception_fiq}
    b __system_trap_common
    .balign 0x80
    sub sp, sp, #16
    stp x2, x3, [sp]
    mov x3, #{exception_serror}
    b __system_trap_common

// On entry: SP is whatever the interrupted context had, x3 holds the kind the
// vector slot was entered through, and the interrupted x2 and x3 are in the
// sixteen bytes the slot reserved. ELR_EL1, SPSR_EL1 and ESR_EL1 hold the
// fault. The per-CPU control block is in TPIDR_EL1 and carries the frame stack
// this path allocates from.
__system_trap_common:
    // Reserve the rest of the scratch area, so this entry owns the same 48
    // bytes as it did when every slot shared one prologue.
    sub sp, sp, #32
    stp x0, x1, [sp]
    stp x4, x5, [sp, #16]
    // The kind travels in x3, whose interrupted value is already on the stack.
    mov x4, x3

    // Load the control block. A zero pointer means no per-CPU state was
    // published, so there is nowhere to record a frame; report and halt rather
    // than dereferencing it.
    mrs x0, tpidr_el1
    cbz x0, __system_trap_no_frame

    // The entering flag is set only while an entry or a restore is in progress.
    // A trap that arrives while it is set cannot be recorded on the fixed frame
    // stack without overwriting a frame that is still live.
    ldr x5, [x0, #{ctl_entering}]
    cbnz x5, __system_trap_no_frame
    mov x5, #1
    str x5, [x0, #{ctl_entering}]

    // x1 = depth. Frames are a fixed stride from `frames`, indexed by depth.
    ldr x1, [x0, #{ctl_depth}]

    // The frame array holds exactly `cpu_slots` frames of `frame_bytes`, so the
    // depth is the whole bound. `top` is the System stack's limit, which is a
    // different allocation from the frame array; bounding one against the other
    // could never succeed.
    cmp x1, #{cpu_slots}
    b.hs __system_trap_no_frame
    ldr x2, [x0, #{ctl_frames}]
    cbz x2, __system_trap_no_frame
    mov x3, #{frame_bytes}
    madd x2, x1, x3, x2

    // x2 is now the frame base. Write the interrupted context into its own
    // slots, restoring the registers saved above.
    ldp x3, x5, [sp]
    stp x3, x5, [x2, #{registers}]
    ldp x3, x5, [sp, #32]
    stp x3, x5, [x2, #{registers} + 16]
    ldp x3, x5, [sp, #16]
    stp x3, x5, [x2, #{registers} + 32]

    // Which exception kind this frame came from, which is what lets the
    // dispatcher tell an interrupt from a call and a fault from either.
    str x4, [x2, #{exception}]

    // The interrupted stack pointer. From EL0 the hardware switched to SP_EL1,
    // so the interrupted value is SP_EL0; from EL1 an exception does not switch
    // stacks, so it is this sp plus the entry's own reservation.
    mrs x4, spsr_el1
    and x4, x4, #0xf
    cbnz x4, .Lentry_sp_privileged
    mrs x4, sp_el0
    b .Lentry_sp_ready
.Lentry_sp_privileged:
    add x4, sp, #48
.Lentry_sp_ready:
    str x4, [x2, #{saved_sp}]
    add sp, sp, #48

    // Save x6..x29.
    stp x6, x7, [x2, #{registers} + 48]
    stp x8, x9, [x2, #{registers} + 64]
    stp x10, x11, [x2, #{registers} + 80]
    stp x12, x13, [x2, #{registers} + 96]
    stp x14, x15, [x2, #{registers} + 112]
    stp x16, x17, [x2, #{registers} + 128]
    stp x18, x19, [x2, #{registers} + 144]
    stp x20, x21, [x2, #{registers} + 160]
    stp x22, x23, [x2, #{registers} + 176]
    stp x24, x25, [x2, #{registers} + 192]
    stp x26, x27, [x2, #{registers} + 208]
    stp x28, x29, [x2, #{registers} + 224]

    // The two values the shared 30-slot register file cannot hold: AArch64's
    // return link and its software thread pointer. Both are captured before the
    // first `bl`, which overwrites x30. TPIDR_EL1 keeps System's control block,
    // so TPIDR_EL0 is the interrupted context's alone.
    str x30, [x2, #{link_register}]
    mrs x4, tpidr_el0
    str x4, [x2, #{thread_pointer}]

    // The fault state, which is what the diagnostic path reports.
    mrs x4, elr_el1
    str x4, [x2, #{pc}]
    mrs x4, spsr_el1
    str x4, [x2, #{status}]
    mrs x4, esr_el1
    str x4, [x2, #{cause}]
    mrs x4, far_el1
    str x4, [x2, #{value}]

    // The interrupted stack pointer is recorded above from the entry's own
    // stack; nothing else in this entry reads the control block's copy.

    // Record this frame's own nesting depth, then publish the next one. The
    // restore path puts the counter back from this field, so nesting cannot
    // accumulate across returns.
    str x1, [x2, #{prior_depth}]
    add x5, x1, #1
    str x5, [x0, #{ctl_depth}]

    // The handler runs with the entering flag clear, which is the state the
    // restore path publishes for interrupted code; `captured_outer` requires it.
    str xzr, [x0, #{ctl_entering}]
    mov x0, x2
    bl __system_trap_dispatch

    // Restore the integer state and return to the interrupted context.
    // Reached by fall-through from dispatch and by `__system_start_frame`, which
    // supplies a freshly built frame rather than an interrupted one.
    .global __system_restore_frame
__system_restore_frame:
    mov x2, x0
    ldr x4, [x2, #{pc}]
    msr elr_el1, x4
    ldr x4, [x2, #{status}]
    msr spsr_el1, x4
    // Publish the state interrupted code resumes in: the entering flag set, and
    // the nesting counter back at the depth this frame interrupted. Without the
    // counter restore it would accumulate and the next trap would look nested.
    mrs x5, tpidr_el1
    mov x4, #1
    str x4, [x5, #{ctl_entering}]
    ldr x4, [x2, #{prior_depth}]
    str x4, [x5, #{ctl_depth}]
    // Restore the interrupted stack pointer, thread pointer and return link while
    // x2 still holds the frame base; the register reload below overwrites it.
    // A return to EL0 uses SP_EL0; a return to EL1 uses sp.
    ldr x5, [x2, #{status}]
    and x5, x5, #0xf
    ldr x4, [x2, #{saved_sp}]
    cbnz x5, .Lrestore_sp_privileged
    msr sp_el0, x4
    b .Lrestore_sp_ready
.Lrestore_sp_privileged:
    mov sp, x4
.Lrestore_sp_ready:
    ldr x4, [x2, #{thread_pointer}]
    msr tpidr_el0, x4
    ldr x30, [x2, #{link_register}]
    ldp x4, x5, [x2, #{registers} + 32]
    ldp x6, x7, [x2, #{registers} + 48]
    ldp x8, x9, [x2, #{registers} + 64]
    ldp x10, x11, [x2, #{registers} + 80]
    ldp x12, x13, [x2, #{registers} + 96]
    ldp x14, x15, [x2, #{registers} + 112]
    ldp x16, x17, [x2, #{registers} + 128]
    ldp x18, x19, [x2, #{registers} + 144]
    ldp x20, x21, [x2, #{registers} + 160]
    ldp x22, x23, [x2, #{registers} + 176]
    ldp x24, x25, [x2, #{registers} + 192]
    ldp x26, x27, [x2, #{registers} + 208]
    ldp x28, x29, [x2, #{registers} + 224]
    ldp x0, x1, [x2, #{registers}]
    // x2 is the address every reload above reads through, so the pair that
    // restores x2 itself has to be the last load. The entering flag is cleared
    // just before it, as nothing between here and `eret` can trap.
    mrs x4, tpidr_el1
    str xzr, [x4, #{ctl_entering}]
    ldp x2, x3, [x2, #{registers} + 16]
    eret

// No usable frame: either no per-CPU state was published, the entering flag says
// another entry or restore is still in progress, or the control block names no
// frame for this depth. Undo the entry's own reservation so the report states
// the interrupted stack pointer, and fall through to the one reporting path.
__system_trap_no_frame:
    add sp, sp, #48
    b __system_trap_no_control

// No per-CPU state: there is no frame to record and no way to report through the
// shared path. Write the fault registers straight to the bootstrap console and
// halt.
//
// This path must not be silent. It is reached precisely when System has installed
// its vector table but has not yet published a per-hart control block, so it is
// the fault path for every failure during admission - the one window where no
// other diagnostic channel exists. A spin here is indistinguishable from a hang,
// and a fault that says nothing costs a build cycle per guess. The console is
// written directly rather than through the shared serial path for the same
// reason: that path needs state this path does not have.
__system_trap_no_control:
    msr daifset, #2
    mrs x19, esr_el1
    mrs x20, elr_el1
    mrs x21, far_el1
    mrs x22, mpidr_el1
    // VBAR says which table took this exception; SP says which stack the handler
    // is on; x30 is the faulting context's link register, still intact because
    // nothing has called anything yet on this path.
    mrs x23, vbar_el1
    mov x24, sp
    mov x25, x30
    // Both translation base registers as they stood when the fetch faulted. A
    // report that does not say which root was active cannot distinguish "the
    // switch did not hold" from "the switch held and the tables are wrong", and
    // those are different defects.
    mrs x26, ttbr0_el1
    mrs x27, ttbr1_el1
    // SCTLR_EL1: bit 19 is WXN, which forces every writable page execute-never
    // and overrides PXN. A writable, descriptor-executable page that still
    // faults on fetch is exactly that signature.
    mrs x28, sctlr_el1
    // TPIDR_EL1 separates "no per-CPU state was published" from "the control
    // block exists but has no frame for this depth"; both reach this report.
    mrs x29, tpidr_el1
    ldr x0, =no_control_text
    bl no_control_puts
    mov x0, x22
    bl no_control_hex
    ldr x0, =no_control_esr_text
    bl no_control_puts
    mov x0, x19
    bl no_control_hex
    ldr x0, =no_control_elr_text
    bl no_control_puts
    mov x0, x20
    bl no_control_hex
    ldr x0, =no_control_far_text
    bl no_control_puts
    mov x0, x21
    bl no_control_hex
    ldr x0, =no_control_vbar_text
    bl no_control_puts
    mov x0, x23
    bl no_control_hex
    ldr x0, =no_control_ttbr0_text
    bl no_control_puts
    mov x0, x26
    bl no_control_hex
    ldr x0, =no_control_ttbr1_text
    bl no_control_puts
    mov x0, x27
    bl no_control_hex
    ldr x0, =no_control_sp_text
    bl no_control_puts
    mov x0, x24
    bl no_control_hex
    ldr x0, =no_control_lr_text
    bl no_control_puts
    mov x0, x25
    bl no_control_hex
    ldr x0, =no_control_sctlr_text
    bl no_control_puts
    mov x0, x28
    bl no_control_hex
    ldr x0, =no_control_tpidr_text
    bl no_control_puts
    mov x0, x29
    bl no_control_hex
    ldr x0, =no_control_nl
    bl no_control_puts
2:  wfi
    b 2b

// x0 = byte. Polls the PL011 flag register until the transmit FIFO has room.
no_control_putc:
    ldr x1, ={uart}
3:  ldr w2, [x1, #0x18]
    tbnz w2, #5, 3b
    str w0, [x1]
    ret

// x0 = NUL-terminated string.
no_control_puts:
    stp x29, x30, [sp, #-16]!
    mov x3, x0
4:  ldrb w0, [x3], #1
    cbz w0, 5f
    bl no_control_putc
    b 4b
5:  ldp x29, x30, [sp], #16
    ret

// x0 = value, printed as sixteen lower-case hexadecimal digits.
no_control_hex:
    stp x29, x30, [sp, #-16]!
    mov x4, x0
    mov x5, #60
6:  lsr x0, x4, x5
    and x0, x0, #0xf
    cmp x0, #10
    b.lo 7f
    add x0, x0, #('a' - 10)
    b 8f
7:  add x0, x0, #'0'
8:  bl no_control_putc
    subs x5, x5, #4
    b.pl 6b
    ldp x29, x30, [sp], #16
    ret

// Enter a freshly built frame rather than returning to an interrupted context.
// The frame states the level it resumes at, and the restore path installs the
// matching stack pointer, so this is the same return.
    .global __system_start_frame
__system_start_frame:
    b __system_restore_frame

// Request a trap from an ordinary call frame: (number, argument, length) in
// x0/x1/x2 becomes the x8/x0/x1 convention the exception entry expects.
    .global __system_trap_gateway
__system_trap_gateway:
    mov x8, x0
    mov x0, x1
    mov x1, x2
    svc #0
    ret

.section .rodata
no_control_text:
    .asciz "SYSTEM_TRAP_NO_CONTROL hart="
no_control_esr_text:
    .asciz " ESR="
no_control_elr_text:
    .asciz " ELR="
no_control_far_text:
    .asciz " FAR="
no_control_vbar_text:
    .asciz " VBAR="
no_control_sp_text:
    .asciz " SP="
no_control_ttbr0_text:
    .asciz " TTBR0="
no_control_ttbr1_text:
    .asciz " TTBR1="
no_control_lr_text:
    .asciz " LR="
no_control_sctlr_text:
    .asciz " SCTLR="
no_control_tpidr_text:
    .asciz " TPIDR="
no_control_nl:
    .asciz "\n"
