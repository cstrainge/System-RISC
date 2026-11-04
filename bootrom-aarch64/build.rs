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
    let sdhci = base("storage", "sdhci");
    let mailbox = base("gpio-mailbox", "bcm2835-mailbox");
    fs::write(out.join("platform-memory.rs"),
              format!("pub const PHYSICAL_RAM_BASE: u64 = {ram_base};\n\
                       pub const PHYSICAL_RAM_BYTES: u64 = {ram_bytes};\n\
                       pub const PHYSMAP_BASE: u64 = {physmap_base};\n\
                       pub const PL011_BASE: u64 = {pl011};\n\
                       pub const SDHCI_BASE: u64 = {sdhci};\n\
                       pub const MAILBOX_BASE: u64 = {mailbox};\n"))
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
