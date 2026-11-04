.equ PL011_DR,   0x00
.equ PL011_FR,   0x18
.equ PL011_IBRD, 0x24
.equ PL011_FBRD, 0x28
.equ PL011_LCRH, 0x2C
.equ PL011_CR,   0x30

.section .text.entry
.global _start
_start:
    mrs x0, mpidr_el1
    and x0, x0, #0xff
    cbnz x0, parked
    ldr x0, =__stack_top
    mov sp, x0
    ldr x0, =vectors
    msr vbar_el1, x0
    isb
    bl uart_init
    adr x0, ready_marker
    bl uart_puts
1:  wfe
    b 1b

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
    ldr x1, =PL011_BASE
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
ready_marker:
    .asciz "BOOT_AARCH64_READY: qemu_arm64 EL1 entry reached\n"
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