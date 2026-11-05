/* The boot payload's link layout.
 *
 * The board's firmware copies the payload to the boot window the platform
 * record declares and starts it there, so every section is placed in that one
 * region. Its origin, length and the stack allowance are generated from the
 * record beside this script; nothing here repeats them.
 */
OUTPUT_ARCH(aarch64)
ENTRY(_start)
MEMORY {
    ram (rwx) : ORIGIN = __platform_BOOT_START, LENGTH = __platform_BOOT_END - __platform_BOOT_START
}
SECTIONS {
    . = ORIGIN(ram);
    .text : { KEEP(*(.text.entry)) *(.text .text.*) } > ram
    .rodata : ALIGN(4096) { *(.rodata .rodata.* .srodata .srodata.*) } > ram
    .data : ALIGN(4096) { *(.data .data.* .sdata .sdata.* .got .got.*) } > ram
    .bss (NOLOAD) : ALIGN(4096) {
        __bss_start = .; *(.bss .bss.* .sbss .sbss.*) *(COMMON)
        . = ALIGN(16); __bss_end = .;
    } > ram
    .stack (NOLOAD) : ALIGN(16) {
        . += __platform_BOOT_STACK_BYTES;
        __stack_top = .;
    } > ram
    .ARM.attributes 0 : { *(.ARM.attributes) } :NONE
    /DISCARD/ : { *(.eh_frame .eh_frame_hdr) }
}
