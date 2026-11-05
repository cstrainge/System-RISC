// AArch64 exception entry.
//
// The vector table must be 2 KiB aligned and occupies sixteen 128-byte slots:
// four exception kinds (synchronous, IRQ, FIQ, SError) from four sources
// (current EL with SP0, current EL with SPx, lower EL AArch64, lower EL AArch32).
// Every slot branches to the shared entry below.
//
// Frame layout is the shared TrapFrame: integer registers, then the fields the
// parameters below name. The floating-point and vector portion is not carried
// yet; see the module comment in trap_entry.rs.

    .section .text.trap_entry, "ax"
    .balign 2048
    .global __system_trap_vector
__system_trap_vector:
    .rept 16
    .balign 0x80
    b __system_trap_common
    .endr

// On entry: SP is whatever the interrupted context had. ELR_EL1, SPSR_EL1,
// ESR_EL1 and FAR_EL1 hold the fault. The per-CPU control block is in TPIDR_EL1
// and carries the frame stack this path allocates from.
__system_trap_common:
    // Reserve room for the two saved scratch registers below the frame.
    sub sp, sp, #16
    stp x0, x1, [sp]

    // Load the control block. A zero pointer means no per-CPU state was
    // published, so there is nowhere to record a frame; report and halt rather
    // than dereferencing it.
    mrs x0, tpidr_el1
    cbz x0, __system_trap_no_control

    // x1 = depth. Frames are a fixed stride from `frames`, indexed by depth.
    ldr x1, [x0, #{ctl_depth}]

    // Bounds check before computing the frame address: depth must be below the
    // slot count, and the frame must stay below `top`.
    cmp x1, #{cpu_slots}
    b.hs __system_trap_no_control
    ldr x2, [x0, #{ctl_frames}]
    mov x3, #{frame_bytes}
    madd x2, x1, x3, x2
    ldr x3, [x0, #{ctl_top}]
    cmp x2, x3
    b.hs __system_trap_no_control

    // x2 is now the frame base. Restore the two registers saved above.
    ldp x4, x5, [sp]
    add sp, sp, #16
    stp x4, x5, [x2, #{registers}]

    // Save x2..x29. x0..x1 came from the stack, so start at x2.
    stp x2, x3, [x2, #{registers} + 16]
    stp x4, x5, [x2, #{registers} + 32]
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

    // The fault state, which is what the diagnostic path reports.
    mrs x4, elr_el1
    str x4, [x2, #{pc}]
    mrs x4, spsr_el1
    str x4, [x2, #{status}]
    mrs x4, esr_el1
    str x4, [x2, #{cause}]
    mrs x4, far_el1
    str x4, [x2, #{value}]

    // The interrupted stack pointer. On an exception from a lower or same level
    // with SPx, the interrupted SP is not directly readable, so the control
    // block's recorded value is used where the entry saved it.
    ldr x4, [x0, #{ctl_sp}]
    str x4, [x2, #{saved_sp}]

    // Publish this frame as the current one before any handler runs, so a nested
    // fault can find its way back.
    ldr x4, [x2, #{prior_depth}]
    cmp x4, #0
    b.ne 1f
    // First frame: record the entering context.
    mov x5, #1
    str x5, [x0, #{ctl_entering}]
1:
    add x5, x1, #1
    str x5, [x0, #{ctl_depth}]

    // The shared dispatch takes the frame in x0 and returns a dispatch decision.
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
    ldp x2, x3, [x2, #{registers} + 16]
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
    eret

// No per-CPU state: there is no frame to record and no way to report through the
// shared path. Halt rather than dereference a null control block.
__system_trap_no_control:
    msr daifset, #2
2:  wfi
    b 2b

// Enter a freshly built frame rather than returning to an interrupted context.
// The frame carries the stack pointer the new context must run on, so unlike an
// exception return this path has to install it explicitly before eret.
    .global __system_start_frame
__system_start_frame:
    ldr x4, [x0, #{saved_sp}]
    mov sp, x4
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
