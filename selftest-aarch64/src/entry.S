//! Entry from the board's first-stage firmware, which loads the flat image at
//! the platform record's boot base and jumps to its first byte. The pattern is
//! the bootrom's: park every secondary core, drop to EL1, install a vector
//! table, then hand control to Rust with the MMU and caches still off. The
//! self-test enables them itself once the mailbox work is done, so the cache
//! experiment is a deliberate step rather than a state inherited from boot.
.equ PL011_DR,   0x00
.equ PL011_FR,   0x18
.equ PL011_IBRD, 0x24
.equ PL011_FBRD, 0x28
.equ PL011_LCRH, 0x2C
.equ PL011_CR,   0x30

.section .text.entry
.global _start
_start:
    msr daifset, #0xf
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
    // The compiler may use the vector unit for ordinary copies. FPEN is 0 at
    // reset, so without this the first such instruction traps.
    mov x0, #(3 << 20)
    msr cpacr_el1, x0
    isb
    // Statics are read before they are written; the firmware does not promise
    // a zeroed image, so clear the declared BSS range here.
    ldr x0, =__bss_start
    ldr x1, =__bss_end
    mov x2, #0
2:  cmp x0, x1
    b.hs 3f
    str x2, [x0], #8
    b 2b
3:  dsb sy
    bl uart_init
    bl rust_entry
4:  wfe
    b 4b

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
5:  ldr w3, [x1, #PL011_FR]
    tbnz w3, #5, 5b
    str w0, [x1, #PL011_DR]
    ret

uart_puts:
    stp x29, x30, [sp, #-16]!
    mov x2, x0
6:  ldrb w0, [x2], #1
    cbz w0, 7f
    bl uart_putc
    b 6b
7:  ldp x29, x30, [sp], #16
    ret

uart_puthex:
    stp x29, x30, [sp, #-16]!
    mov x4, x0
    mov w5, #60
8:  lsr x0, x4, x5
    and w0, w0, #0xf
    cmp w0, #10
    b.lt 9f
    add w0, w0, #87
    b 10f
9:  add w0, w0, #48
10: bl uart_putc
    subs w5, w5, #4
    b.ge 8b
    ldp x29, x30, [sp], #16
    ret

.balign 2048
vectors:
    .rept 16
    .balign 0x80
    b trap
    .endr

// Every vector reports the fault registers on the console and stops. A trap
// means the self-test's own state is unknown, so continuing would be a lie.
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
11: wfe
    b 11b

.section .rodata
trap_marker:
    .asciz "SELFTEST_TRAP: exception taken\n"
esr_label:
    .asciz "ESR_EL1="
elr_label:
    .asciz " ELR_EL1="
far_label:
    .asciz " FAR_EL1="
newline:
    .asciz "\n"
