OUTPUT_ARCH(riscv)
ENTRY(_reset)
MEMORY { reset (rx) : ORIGIN = __platform_RAM_BASE, LENGTH = __platform_PAGE_BYTES
         ram (rwx) : ORIGIN = __platform_APPLICATION_RELOCATION_BASE, LENGTH = 2M }
SECTIONS {
    .reset : { KEEP(*(.text.probe_reset)) } > reset
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
