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

/* The bootstrap translation tables. They are not a section: the entry code
 * writes them with plain stores, so the linker cannot place them and nothing
 * but this script knows where they are. Their address is therefore stated once,
 * here, and the code asks for it rather than repeating it. The assertions below
 * and not a comment are what keeps them where they belong: the stack grows down
 * from `__stack_top`, so tables placed at or below it are overwritten by the
 * first deep call, and the fault then appears as a data abort inside whatever
 * function forced the walk. Both bounds are relative to symbols, so they move
 * with the image instead of pinning a build. */
__tables_start = 0x110000;
__tables_end = __tables_start + 0x5000;
ASSERT(__stack_top <= __tables_start,
       "BOOT_TABLES_OVERLAP_STACK: the translation tables sit inside the stack window")
ASSERT(__tables_end <= __platform_FIRMWARE_HEAP_START,
       "BOOT_TABLES_OVERLAP_HEAP: the translation tables reach the firmware heap")
ASSERT(__tables_start < ORIGIN(ram) + LENGTH(ram),
       "BOOT_TABLES_OUTSIDE_WINDOW: the translation tables leave the boot window")

/* The boot window holds code and data, and the page tables give each half its
 * own permission: code read-only and executable, data writable and not
 * executable. That only works if the boundary the record declares is where the
 * linker actually stops placing code, so it is asserted rather than trusted.
 * Without this, growing the payload past the declared extent would leave the
 * tail of .text inside the writable region - silently, and only a boot would
 * show it. */
ASSERT(ADDR(.data) == ORIGIN(ram) + __platform_FIRMWARE_CODE_BYTES,
       "BOOT_CODE_BOUNDARY_STALE: .data does not begin at the record's firmware_code_bytes; the code/data split would put code in the writable region or data in the executable one")
