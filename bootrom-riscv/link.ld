OUTPUT_ARCH(riscv)
ENTRY(_start)
MEMORY {
    flash (rx) : ORIGIN = __platform_FLASH_START, LENGTH = __platform_FLASH_END - __platform_FLASH_START
    ram (rw) : ORIGIN = __platform_BOOT_START, LENGTH = __platform_BOOT_END - __platform_BOOT_START
    scratch (rw) : ORIGIN = __platform_FIRMWARE_HEAP_START, LENGTH = __platform_FIRMWARE_HEAP_END - __platform_FIRMWARE_HEAP_START
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
    .heap (NOLOAD) : ALIGN(16) { __heap_start = .; . += LENGTH(scratch); __heap_end = .; } > scratch
    .stack (NOLOAD) : ALIGN(16) { . += __platform_BOOT_STACK_BYTES; __stack_top = .; } > ram
    /DISCARD/ : { *(.eh_frame .eh_frame_hdr) }
}
