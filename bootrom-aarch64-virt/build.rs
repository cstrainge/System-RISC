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
    let devices = record["hardware"]["devices"].as_array().expect("hardware.devices");
    let console_base = devices.iter()
                              .find(|device| device["kind"] == "uart")
                              .and_then(|device| device["base"].as_str())
                              .expect("the record must declare a UART peripheral")
                              .to_string();
    fs::write(out.join("platform-hardware.inc"),
              format!(".equ PL011_BASE, {console_base}\n"))
        .expect("hardware include");
    // Nothing is generated here. This machine supplies its own device tree and
    // the bootloader reads the pointer it passes, so embedding a second
    // description would give one machine two trees and a consumer a choice.
    let fdt_tail_bytes = memory["fdt_tail_bytes"].as_str().expect("memory.fdt_tail_bytes");
    fs::write(out.join("platform-memory.rs"),
              format!("pub const PHYSICAL_RAM_BASE: u64 = {ram_base};\n\
                       pub const PHYSICAL_RAM_BYTES: u64 = {ram_bytes};\n\
                       pub const PHYSMAP_BASE: u64 = {physmap_base};\n\
                       pub const PL011_BASE: u64 = {console_base};\n\
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
                       pub const FDT_TAIL_BYTES: u64 = {fdt_tail_bytes};\n",
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
