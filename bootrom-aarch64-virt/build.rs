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
    let out = Path::new(&env::var("OUT_DIR").expect("Cargo supplies OUT_DIR")).to_path_buf();
    // The console and the boot MMU are the only generated constants. Every
    // peripheral address comes from the device tree the emulator generates, so
    // this bootloader stays a consumer of the machine rather than a second
    // description of it. The console is the PL011 the virt machine provides.
    fs::write(out.join("platform-hardware.inc"),
              ".equ PL011_BASE, 0x09000000\n")
        .expect("hardware include");
    fs::write(out.join("platform-memory.rs"),
              format!("pub const PHYSICAL_RAM_BASE: u64 = {ram_base};\n\
                       pub const PHYSICAL_RAM_BYTES: u64 = {ram_bytes};\n\
                       pub const PHYSMAP_BASE: u64 = {physmap_base};\n\
                       pub const PL011_BASE: u64 = 0x0900_0000;\n\
                       // The boot page tables live in RAM below the loaded image. They are\
                       // derived from the record rather than fixed, because a machine whose\
                       // RAM does not start at zero has no usable low addresses at all.\n\
                       pub const L1_LOW: u64 = {};\n\
                       pub const L2_LOW: u64 = {};\n\
                       pub const L1_HIGH: u64 = {};\n\
                       pub const L2_PHYSMAP: u64 = {};\n\
                       pub const L2_LOWDEV: u64 = {};\n",
              format!("{ram_base}u64 + 0x1000"),
              format!("{ram_base}u64 + 0x2000"),
              format!("{ram_base}u64 + 0x3000"),
              format!("{ram_base}u64 + 0x4000"),
              format!("{ram_base}u64 + 0x5000")))
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
