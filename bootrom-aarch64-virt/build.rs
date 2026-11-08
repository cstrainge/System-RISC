//! The virt bootloader reads what the virtual platform record declares. It has
//! no `hardware` section to read: a virtual machine states its peripherals in
//! the device tree the emulator generates and in `emulation`, not as a board
//! inventory.
use std::{env, fs, path::Path};

fn main()
{
    println!("cargo:rerun-if-env-changed={}", mac_build::platform::PLATFORM_ENV);
    let platform = mac_build::platform::current();
    let record_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../platforms")
                                                           .join(format!("{}.json", platform.name));
    println!("cargo:rerun-if-changed={}", record_path.display());
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(&record_path).expect("platform record"))
            .expect("valid platform record");
    let boot = &record["memory"]["boot"];
    let boot_base = boot["base"].as_str().expect("memory.boot.base");
    let bytes = boot["bytes"].as_str().expect("memory.boot.bytes");
    let memory = &record["memory"];
    let ram_base = memory["ram_base"].as_str().expect("memory.ram_base");
    let ram_bytes = memory["minimum_ram_bytes"].as_str().expect("memory.minimum_ram_bytes");
    let physmap_base = record["virtual_memory"]["physmap"]["start"].as_str()
                             .expect("virtual_memory.physmap.start");
    let system_image_base = record["virtual_memory"]["system_image_base"].as_str()
                                  .expect("virtual_memory.system_image_base");
    let image_base = memory["images"]["base"].as_str().expect("memory.images.base");
    let image_bytes = memory["images"]["bytes"].as_str().expect("memory.images.bytes");
    let out = Path::new(&env::var("OUT_DIR").expect("Cargo supplies OUT_DIR")).to_path_buf();
    // The console and the boot MMU are the only generated constants. Every
    // peripheral address comes from the device tree the emulator generates, so
    // this bootloader stays a consumer of the machine rather than a second
    // description of it. The console is the PL011 the virt machine provides.
    // The console base is a build-time constant because the earliest output
    // happens before any tree can be read, and a malformed tree has to be
    // reportable somewhere. Everything else comes from the tree.
    // The console is whichever device the record's console declaration names,
    // not whichever device happens to be first. Two selection rules is the
    // defect this replaces: the record states the part once, and both the early
    // output address and the tree lookup derive from that one statement.
    let console = &record["hardware"]["console"];
    let console_kind = console["kind"].as_str().expect("hardware.console.kind");
    let devices = record["hardware"]["devices"].as_array().expect("hardware.devices");
    let device = devices.iter()
                        .find(|device| device["model"].as_str() == Some(console_kind))
                        .expect("the record's console must appear in hardware.devices");
    let console_base = device["base"].as_str()
                             .expect("the console device must declare a base")
                             .to_string();
    // A machine's tree names the part by its standard compatible string, which
    // is often not the record's own name for it. The mapping is declared, so
    // the bootloader asks the tree a question with a stated answer rather than
    // a guessed one.
    let console_compatible = console["compatible"]
        .as_str()
        .unwrap_or(console_kind)
        .to_string();
    fs::write(out.join("platform-hardware.inc"),
              format!(".equ PL011_BASE, {console_base}\n"))
        .expect("hardware include");
    // Nothing is generated here. This machine supplies its own device tree and
    // the bootloader reads the pointer it passes, so embedding a second
    // description would give one machine two trees and a consumer a choice.
    // Whether this machine declares an emulated display. The flag the kernel
    // receives is read from the validated record rather than a second time from
    // the raw JSON, so this build script cannot derive a different answer from
    // the record the rest of the build checked. A record with no `emulation`
    // section is a board, and its silence means headless; a record with one must
    // state its display, and `emulation::Definition::check` refuses it by name
    // otherwise. A boot that claims a graphical start while the record declares
    // no display would ask the kernel to launch a Finder onto nothing.
    let graphical = platform.emulation()
                           .and_then(|emulation| emulation.display.as_ref())
                           .is_some();
    let boot_flags = if graphical { "mac_abi::BOOT_GRAPHICAL" } else { "0" };
    let fdt_tail_bytes = memory["fdt_tail_bytes"].as_str().expect("memory.fdt_tail_bytes");
    // The firmware heap is a region the record declares, not a BSS array: the
    // code resource is several megabytes, so a fixed in-image buffer cannot
    // hold it and the declared region is what the memory map is checked against.
    let heap_base = memory["firmware_heap"]["base"].as_str().expect("memory.firmware_heap.base");
    let heap_bytes = memory["firmware_heap"]["bytes"].as_str().expect("memory.firmware_heap.bytes");
    // How much of the boot window is code. The window holds both code and data,
    // and one permission cannot be right for both: code must be read-only and
    // executable, data writable and not executable. The boundary is declared
    // here rather than inferred from a section symbol, so the region table states
    // it and the mapping code never guesses.
    let firmware_code_bytes = memory["firmware_code_bytes"].as_str()
                                     .expect("memory.firmware_code_bytes");
    let scratch_base = memory["display_scratch"]["base"].as_str()
                              .expect("memory.display_scratch.base");
    let scratch_bytes = memory["display_scratch"]["bytes"].as_str()
                               .expect("memory.display_scratch.bytes");
    fs::write(out.join("platform-memory.rs"),
              format!("pub const PHYSICAL_RAM_BASE: u64 = {ram_base};\n\
                       pub const PHYSICAL_RAM_BYTES: u64 = {ram_bytes};\n\
                       pub const PHYSMAP_BASE: u64 = {physmap_base};\n\
                       pub const PL011_BASE: u64 = {console_base};\n\
                       pub const PL011_COMPATIBLE: &str = \"{console_compatible}\";\n\
                       // The boot page tables live in RAM below the loaded image. They are\
                       // derived from the record rather than fixed, because a machine whose\
                       // RAM does not start at zero has no usable low addresses at all.\n\
                       pub const L1_LOW: u64 = {};\n\
                       pub const L2_LOW: u64 = {};\n\
                       pub const L1_HIGH: u64 = {};\n\
                       pub const L2_PHYSMAP: u64 = {};\n\
                       pub const L2_LOWDEV: u64 = {};\n\
                       pub const L2_IMAGE: u64 = {};\n\
                       pub const L3_BASE: u64 = {};\n\
                       pub const VM_SYSTEM_IMAGE_BASE: u64 = {system_image_base};\n\
                       pub const IMAGE_BASE: u64 = {image_base};\n\
                       pub const IMAGE_BYTES: u64 = {image_bytes};\n\
                       // The reservation a machine-supplied tree must fit inside. A blob that\
                       // states a larger size is refused rather than read past its end.\n\
                       pub const FDT_TAIL_BYTES: u64 = {fdt_tail_bytes};\n\
                       pub const FIRMWARE_CODE_BYTES: u64 = {firmware_code_bytes};\n\
                       pub const BOOT_FLAGS: u32 = {boot_flags};\n\
                       pub const FW_HEAP_BASE: u64 = {heap_base};\n\
                       pub const FW_HEAP_BYTES: u64 = {heap_bytes};\n\
                       pub const BOOT_BASE: u64 = {boot_base};\n\
                       pub const BOOT_BYTES: u64 = {bytes};\n\
                       pub const DISPLAY_SCRATCH_BASE: u64 = {scratch_base};\n\
                       pub const DISPLAY_SCRATCH_BYTES: u64 = {scratch_bytes};\n",
              format!("{ram_base}u64 + 0x1000"),
              format!("{ram_base}u64 + 0x2000"),
              format!("{ram_base}u64 + 0x3000"),
              format!("{ram_base}u64 + 0x4000"),
              format!("{ram_base}u64 + 0x5000"),
              format!("{ram_base}u64 + 0x6000"),
              format!("{ram_base}u64 + 0x1_0000")))
        .expect("memory constants");
    let linker = format!("OUTPUT_ARCH(aarch64)\nENTRY(_start)\nMEMORY {{ ram (rwx) : ORIGIN = \
                          {boot_base}, LENGTH = {bytes} }}\nSECTIONS {{\n. = ORIGIN(ram);\n.text : {{ \
                          KEEP(*(.text.entry)) *(.text .text.*) }} > ram\n.rodata : ALIGN(4096) \
                          {{ *(.rodata .rodata.* .srodata .srodata.*) }} > ram\n.data : \
                          ALIGN(4096) {{ *(.data .data.* .sdata .sdata.* .got .got.*) }} > \
                          ram\n.bss (NOLOAD) : ALIGN(4096) {{ __bss_start = .; *(.bss .bss.* \
                          .sbss .sbss.*) *(COMMON) . = ALIGN(16); __bss_end = .; }} > ram\n. = \
                          ALIGN(16);\n. += 0x10000;\n__stack_top = .;\n.ARM.attributes 0 : {{ \
                          *(.ARM.attributes) }} :NONE\n/DISCARD/ : {{ *(.eh_frame .eh_frame_hdr) \
                          }}\n}}\n");
    fs::write(out.join("boot.ld"), linker).expect("boot linker script");
    println!("cargo:rustc-link-arg-bin=bootrom-aarch64-virt=-T{}", out.join("boot.ld").display());
    if env::var("TARGET") != env::var("HOST")
    {
        mac_build::platform::validate_pic_flags().expect("native PIC dependency contract");
        mac_build::platform::validate_guest(&env::var("TARGET").expect("target"),
                                            &env::var("CARGO_CFG_TARGET_FEATURE").expect("features"))
            .expect("native platform ISA floor");
    }
}
