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
    let uart = record["hardware"]["devices"].as_array()
                                            .expect("hardware.devices")
                                            .iter()
                                            .find(|device| device["kind"] == "uart")
                                            .expect("the platform must declare a UART peripheral");
    let uart_base = uart["base"].as_str().expect("the UART must state its base");
    let out = Path::new(&env::var("OUT_DIR").expect("Cargo supplies OUT_DIR")).to_path_buf();
    // Nothing is generated here. The board's firmware loads the board's own
    // device tree and hands it over in x0, so embedding a second description
    // would give one machine two trees and a consumer a choice.
    println!("cargo:rerun-if-changed={}", record_path.display());
    fs::write(out.join("platform-hardware.inc"),
              format!(".equ PL011_BASE, {uart_base}\n"))
        .expect("hardware include");
    let memory = &record["memory"];
    let ram_base = memory["ram_base"].as_str().expect("memory.ram_base");
    let ram_bytes = memory["minimum_ram_bytes"].as_str().expect("memory.minimum_ram_bytes");
    let physmap_base = record["virtual_memory"]["physmap"]["start"].as_str()
                             .expect("virtual_memory.physmap.start");
    let devices = record["hardware"]["devices"].as_array().expect("hardware.devices");
    let base = |kind: &str, model: &str| -> String {
        devices.iter()
               .find(|device| device["kind"] == kind && device["model"] == model)
               .and_then(|device| device["base"].as_str())
               .unwrap_or_else(|| panic!("the platform must declare {kind}/{model}"))
               .to_string()
    };
    let pl011 = base("uart", "pl011");
    // The reservation a firmware-supplied tree must fit inside. A blob that
    // states a larger size is refused rather than read past its end.
    let fdt_tail_bytes = record["memory"]["fdt_tail_bytes"].as_str()
                               .expect("memory.fdt_tail_bytes");
    // Only the console and the boot MMU read generated constants. Every other
    // peripheral address comes from the device tree, so the bootloader stays a
    // consumer of the record rather than a second copy of it.
    fs::write(out.join("platform-memory.rs"),
              format!("pub const PHYSICAL_RAM_BASE: u64 = {ram_base};\n\
                       pub const PHYSICAL_RAM_BYTES: u64 = {ram_bytes};\n\
                       pub const PHYSMAP_BASE: u64 = {physmap_base};\n\
                       pub const PL011_BASE: u64 = {pl011};\n\
                       pub const FDT_TAIL_BYTES: u64 = {fdt_tail_bytes};\n"))
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
    println!("cargo:rustc-link-arg-bin=bootrom-aarch64=-T{}", out.join("boot.ld").display());
    if env::var("TARGET") != env::var("HOST")
    {
        mac_build::platform::validate_pic_flags().expect("native PIC dependency contract");
        mac_build::platform::validate_guest(&env::var("TARGET").expect("target"),
                                            &env::var("CARGO_CFG_TARGET_FEATURE").expect("features"))
            .expect("native platform ISA floor");
    }
}
