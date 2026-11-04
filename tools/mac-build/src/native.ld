OUTPUT_ARCH(riscv)
ENTRY(_start)
MEMORY { ram (rwx) : ORIGIN = @BASE@, LENGTH = @LENGTH@ }
PHDRS {
    text PT_LOAD FLAGS(5);
    rodata PT_LOAD FLAGS(4);
    data PT_LOAD FLAGS(6);
}
SECTIONS {
    . = ORIGIN(ram);
    .text : { KEEP(*(.text.entry)) *(.text .text.*) } > ram :text
    .rodata : ALIGN(4096) { *(.rodata .rodata.* .srodata .srodata.*) } > ram :rodata
    .system_risc_module : ALIGN(8) { KEEP(*(.system_risc_module)) } > ram :rodata
    /* Actual std callback/return addresses remain read-only after RLOC fixups. */
    .system_risc_user_runtime : ALIGN(8) { KEEP(*(.system_risc_user_runtime)) } > ram :rodata
    ASSERT(SIZEOF(.system_risc_user_runtime) == 0 || SIZEOF(.system_risc_user_runtime) == 16,
           "user runtime table must contain exactly two native addresses")
    .data : ALIGN(4096) { *(.sdata .sdata.* .data .data.* .got .got.*) } > ram :data
    .bss (NOLOAD) : ALIGN(4096) {
        __bss_start = .;
        *(.sbss .sbss.* .bss .bss.*) *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    } > ram :data
    .riscv.attributes 0 : { *(.riscv.attributes) } :NONE
    /DISCARD/ : { *(.eh_frame .eh_frame_hdr) }
}
