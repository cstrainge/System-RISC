OUTPUT_ARCH(riscv)
ENTRY(_start)
MEMORY { ram (rwx) : ORIGIN = 0x80000000, LENGTH = 2M }
SECTIONS {
    . = ORIGIN(ram);
    .text : { KEEP(*(.text.entry)) *(.text .text.*) } > ram
    .rodata : ALIGN(16) { *(.rodata .rodata.*) } > ram
    .data : ALIGN(16) { *(.sdata .sdata.* .data .data.*) } > ram
    .bss (NOLOAD) : ALIGN(16) {
        __bss_start = .; *(.sbss .sbss.* .bss .bss.*) *(COMMON)
        . = ALIGN(8); __bss_end = .;
    } > ram
    .stack (NOLOAD) : ALIGN(16) { . += 128K; __stack_top = .; } > ram
    /DISCARD/ : { *(.eh_frame .eh_frame_hdr) }
}
