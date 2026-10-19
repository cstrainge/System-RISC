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
    .data : ALIGN(4096) { *(.sdata .sdata.* .data .data.* .got .got.*) } > ram :data
    .bss (NOLOAD) : ALIGN(16) {
        __bss_start = .;
        *(.sbss .sbss.* .bss .bss.*) *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    } > ram :data
    .riscv.attributes 0 : { *(.riscv.attributes) } :NONE
    /DISCARD/ : { *(.eh_frame .eh_frame_hdr) }
}
