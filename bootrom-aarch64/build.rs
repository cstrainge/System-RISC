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
    let uart = record["hardware"]["devices"].as_array()
                                            .expect("hardware.devices")
                                            .iter()
                                            .find(|device| device["kind"] == "uart")
                                            .expect("the platform must declare a UART peripheral");
    let uart_base = uart["base"].as_str().expect("the UART must state its base");
    let out = Path::new(&env::var("OUT_DIR").expect("Cargo supplies OUT_DIR")).to_path_buf();
    // The record is the author of the machine description. Both the bootloader
    // and the kernel consume this blob, so it is emitted once at build time and
    // linked in rather than assembled at runtime.
    let tree = mac_build::devicetree::generate().expect("device tree generation");
    fs::write(out.join("devicetree.dtb"), &tree).expect("device tree blob");
    println!("cargo:rerun-if-changed={}", record_path.display());
    fs::write(out.join("platform-hardware.inc"),
              format!(".equ PL011_BASE, {uart_base}\n"))
        .expect("hardware include");
    let memory = &record["memory"];
    let ram_base = memory["ram_base"].as_str().expect("memory.ram_base");
    let ram_bytes = memory["minimum_ram_bytes"].as_str().expect("memory.minimum_ram_bytes");
    let physmap_base = record["virtual_memory"]["physmap"]["start"].as_str()
                             .expect("virtual_memory.physmap.start");
    let firmware_heap = &memory["firmware_heap"];
    let firmware_heap_base = firmware_heap["base"].as_str().expect("memory.firmware_heap.base");
    let firmware_heap_bytes = firmware_heap["bytes"].as_str()
                                                    .expect("memory.firmware_heap.bytes");
    let devices = record["hardware"]["devices"].as_array().expect("hardware.devices");
    let base = |kind: &str, model: &str| -> String {
        devices.iter()
               .find(|device| device["kind"] == kind && device["model"] == model)
               .and_then(|device| device["base"].as_str())
               .unwrap_or_else(|| panic!("the platform must declare {kind}/{model}"))
               .to_string()
    };
    let pl011 = base("uart", "pl011");
    // Only the console and the boot MMU read generated constants. Every other
    // peripheral address comes from the device tree, so the bootloader stays a
    // consumer of the record rather than a second copy of it.
    fs::write(out.join("platform-memory.rs"),
              format!("pub const PHYSICAL_RAM_BASE: u64 = {ram_base};\n\
                       pub const PHYSICAL_RAM_BYTES: u64 = {ram_bytes};\n\
                       pub const PHYSMAP_BASE: u64 = {physmap_base};\n\
                       pub const FIRMWARE_HEAP_BASE: u64 = {firmware_heap_base};\n\
                       pub const FIRMWARE_HEAP_BYTES: u64 = {firmware_heap_bytes};\n\
                       pub const PL011_BASE: u64 = {pl011};\n"))
        .expect("memory constants");
    // The link layout is the platform record's, not a second copy of it: the
    // checked memory map is turned into linker symbols and the script beside
    // this build file places every section and the stack in that one window.
    let template = Path::new(env!("CARGO_MANIFEST_DIR")).join("link.ld");
    println!("cargo:rerun-if-changed={}", template.display());
    let (_, symbols) = mac_build::platform::firmware_artifacts().expect("checked boot window");
    let script = out.join("boot.ld");
    let contents = format!("{symbols}{}",
                           fs::read_to_string(&template).expect("boot linker template"));
    if fs::read(&script).ok().as_deref() != Some(contents.as_bytes())
    {
        fs::write(&script, contents).expect("boot linker script");
    }
    println!("cargo:rustc-link-arg-bin=bootrom-aarch64=-T{}", script.display());
    if env::var("TARGET") != env::var("HOST")
    {
        mac_build::platform::validate_pic_flags().expect("native PIC dependency contract");
        mac_build::platform::validate_guest(&env::var("TARGET").expect("target"),
                                            &env::var("CARGO_CFG_TARGET_FEATURE").expect("features"))
            .expect("native platform ISA floor");
    }
}
