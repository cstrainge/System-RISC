OUTPUT_ARCH(riscv)
ENTRY(_start)
MEMORY {
    flash (rx) : ORIGIN = 0x20000000, LENGTH = 32M
    ram (rw) : ORIGIN = 0x80000000, LENGTH = 2M
    scratch (rw) : ORIGIN = 0x87000000, LENGTH = 8M
}
SECTIONS {
    .text : { KEEP(*(.text.entry)) *(.text .text.*) } > flash
    .rodata : ALIGN(16) { *(.rodata .rodata.* .srodata .srodata.*) } > flash
    .data : ALIGN(16) {
        __data_start = .; *(.data .data.* .sdata .sdata.* .got .got.*)
        . = ALIGN(8); __data_end = .;
    } > ram AT> flash
    __data_load = LOADADDR(.data);
    .bss (NOLOAD) : ALIGN(4096) {
        __bss_start = .; *(.bss .bss.* .sbss .sbss.*) *(COMMON)
        . = ALIGN(8); __bss_end = .;
    } > ram
    .heap (NOLOAD) : ALIGN(16) { __heap_start = .; . += 8M; __heap_end = .; } > scratch
    .stack (NOLOAD) : ALIGN(16) { . += 256K; __stack_top = .; } > ram
    /DISCARD/ : { *(.eh_frame .eh_frame_hdr) }
}
