.equ PL011_DR,   0x00
.equ PL011_FR,   0x18
.equ PL011_IBRD, 0x24
.equ PL011_FBRD, 0x28
.equ PL011_LCRH, 0x2C
.equ PL011_CR,   0x30

.section .text.entry
.global _start
_start:
    // A 'B' on the serial line proves _start runs, even if the LED code
    // faults on its first GPIO write. The firmware configures the UART
    // (enable_uart=1), so a direct data-register write transmits without
    // calling uart_init. Only registers are touched — no stack exists yet —
    // and x0 (the device-tree address) and x19 are preserved; x1/x2/x3
    // are the scratch. Label 9 is free in this file, so the poll loop
    // cannot resolve to another label.
    ldr x1, =PL011_BASE
    mov w2, #0x42              // 'B'
9:  ldr w3, [x1, #PL011_FR]
    tbnz w3, #5, 9b            // TXFF: wait until the transmit FIFO is not full
    str w2, [x1, #PL011_DR]
    // The machine's device tree arrives in x0. It is captured before anything
    // else runs, because the very next instruction reuses x0, and x19 is
    // callee-saved so it survives every call below.
    mov x19, x0
    // The LED is initialised before the first blink, while x19 already holds
    // the tree address; led_init is a leaf that touches no stack, so the
    // stack not being set up yet does not matter.
    bl led_init
    // Blink 0: the bootrom entered _start and the LED is initialised. A
    // blink here means the kernel image was loaded and started at all —
    // the solid blink seen first on the board is the firmware's power-on
    // LED, not this one; this one only fires once the bootrom itself
    // runs. led_blink is a leaf like led_init (no stack), so calling it
    // before the stack exists is safe. The count is the last stage reached.
    bl led_blink
    mrs x0, mpidr_el1
    and x0, x0, #0xff
    cbnz x0, parked
    mrs x1, currentel
    lsr x1, x1, #2
    cmp x1, #3
    b.ne 1f
    mov x0, #0x401
    msr scr_el3, x0
    mov x0, #0x80000000
    msr hcr_el2, x0
    mov x0, #0x3c5
    msr spsr_el3, x0
    adr x0, el1_entry
    msr elr_el3, x0
    eret
1:  cmp x1, #2
    b.ne el1_entry
    mov x0, #0x80000000
    msr hcr_el2, x0
    mov x0, #0x3c5
    msr spsr_el2, x0
    adr x0, el1_entry
    msr elr_el2, x0
    eret

el1_entry:
    ldr x0, =__stack_top
    mov sp, x0
    ldr x0, =vectors
    msr vbar_el1, x0
    isb
    // The platform's instruction profile includes NEON, and the compiler uses
    // it for ordinary slice copies. FPEN is 0 at reset, so without this the
    // first vector instruction raises a trap.
    mov x0, #(3 << 20)
    msr cpacr_el1, x0
    isb
    // One blink per stage passed; the count is the last stage reached.
    // Stages in entry.S: 0 _start entered, 1 EL1 reached, 2 UART output
    // done, 3 MMU on, 4 physmap checked.
    bl led_blink
    bl uart_init
    // Reported before anything can depend on it: a machine that supplies no
    // tree is a named fact at entry, not a mystery later.
    adr x0, x0_label
    bl uart_puts
    mov x0, x19
    bl uart_puthex
    adr x0, newline
    bl uart_puts
    mrs x0, currentel
    cmp x0, #0x4
    b.ne el_bad
    adr x0, el1_marker
    bl uart_puts
    // Blink 2: the EL1 marker printed, so the UART path works; the next
    // blink only fires once mmu_init has returned.
    bl led_blink
    bl mmu_init
    bl led_blink
    adr x0, mmu_marker
    bl uart_puts
    bl physmap_check
    cbz x0, physmap_bad
    adr x0, physmap_marker
    bl uart_puts
    b physmap_done
physmap_bad:
    adr x0, physmap_bad_marker
    bl uart_puts
    b idle
physmap_done:
    // The tree captured at entry is the probe's only source of peripheral
    // addresses; nothing about it is trusted, so the probe checks it.
    bl led_blink
    mov x0, x19
    bl sdhci_probe
    cbz x0, sdhci_bad
    adr x0, ready_marker
    bl uart_puts
    b idle
sdhci_bad:
    // The probe halts by itself on every refusal it can detect, so this branch is
    // the defence for a zero that got past it. Before it existed, a refused probe
    // printed its detail and then reached the ready marker, so a card that was
    // never read produced the same transcript as one that was.
    adr x0, sdhci_bad_marker
    bl uart_puts
idle:
2:  wfe
    b 2b

el_bad:
    adr x0, el_bad_marker
    bl uart_puts
3:  wfe
    b 3b

parked:
    wfe
    b parked

uart_init:
    ldr x1, =PL011_BASE
    mov w0, #0
    str w0, [x1, #PL011_CR]
    mov w0, #26
    str w0, [x1, #PL011_IBRD]
    mov w0, #3
    str w0, [x1, #PL011_FBRD]
    mov w0, #0x70
    str w0, [x1, #PL011_LCRH]
    mov w0, #0x301
    str w0, [x1, #PL011_CR]
    ret

uart_putc:
    // A line feed alone leaves a terminal's cursor where the last line ended,
    // so a carriage return goes out ahead of each one. `putc` in main.rs does
    // the same. Only x1 and x3 are free to this function.
    ldr x1, =PL011_BASE
    cmp w0, #10
    b.ne 2f
1:  ldr w3, [x1, #PL011_FR]
    tbnz w3, #5, 1b
    mov w3, #13
    str w3, [x1, #PL011_DR]
2:  ldr w3, [x1, #PL011_FR]
    tbnz w3, #5, 2b
    str w0, [x1, #PL011_DR]
    ret

uart_puts:
    stp x29, x30, [sp, #-16]!
    mov x2, x0
3:  ldrb w0, [x2], #1
    cbz w0, 4f
    bl uart_putc
    b 3b
4:  ldp x29, x30, [sp], #16
    ret

uart_puthex:
    stp x29, x30, [sp, #-16]!
    mov x4, x0
    mov w5, #60
6:  lsr x0, x4, x5
    and w0, w0, #0xf
    cmp w0, #10
    b.lt 7f
    add w0, w0, #87
    b 8f
7:  add w0, w0, #48
8:  bl uart_putc
    subs w5, w5, #4
    b.ge 6b
    ldp x29, x30, [sp], #16
    ret

.balign 2048
vectors:
    .rept 16
    .balign 0x80
    b trap
    .endr

trap:
    bl uart_init
    adr x0, trap_marker
    bl uart_puts
    adr x0, esr_label
    bl uart_puts
    mrs x0, esr_el1
    bl uart_puthex
    adr x0, elr_label
    bl uart_puts
    mrs x0, elr_el1
    bl uart_puthex
    adr x0, far_label
    bl uart_puts
    mrs x0, far_el1
    bl uart_puthex
    adr x0, newline
    bl uart_puts
5:  b 5b

.section .rodata
x0_label:
    .asciz "BOOT_AARCH64_X0: "
el1_marker:
    .asciz "BOOT_AARCH64_EL1_READY: running at EL1\n"
el_bad_marker:
    .asciz "BOOT_AARCH64_EL_BAD: did not reach EL1\n"
mmu_marker:
    .asciz "BOOT_AARCH64_MMU_READY: 39-bit three-level translation active\n"
physmap_marker:
    .asciz "BOOT_AARCH64_PHYSMAP_READY: physmap aliases physical RAM\n"
physmap_bad_marker:
    .asciz "BOOT_AARCH64_PHYSMAP_BAD: physmap does not alias physical RAM\n"
sdhci_bad_marker:
    .asciz "BOOT_AARCH64_PROBE_FAILED: the probe returned failure to the entry code\n"
ready_marker:
    .asciz "BOOT_AARCH64_READY: board bootloader reached its idle loop\n"
trap_marker:
    .asciz "BOOT_TRAP_EL1: exception taken\n"
esr_label:
    .asciz "ESR_EL1="
elr_label:
    .asciz " ELR_EL1="
far_label:
    .asciz " FAR_EL1="
newline:
    .asciz "\n"