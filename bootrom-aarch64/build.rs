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
    // The ACT LED's GPIO block. A platform that declares no such controller
    // gets a base of zero and the LED code is a no-op there, so the bootloader
    // still builds for a machine without this block (QEMU's virt platform).
    // That is why this lookup takes a default instead of the required-device
    // refusal the console uses above.
    let gpio_base = record["hardware"]["devices"].as_array()
                                                 .and_then(|devices| {
                                                     devices.iter().find(|device| {
                                                                       device["kind"] == "gpio"
                                                                       && device["model"]
                                                                          == "bcm2835-gpio"
                                                                   })
                                                 })
                                                 .and_then(|device| device["base"].as_str())
                                                 .map_or("0", |base| base);
    let out = Path::new(&env::var("OUT_DIR").expect("Cargo supplies OUT_DIR")).to_path_buf();
    // Nothing is generated here. The board's firmware loads the board's own
    // device tree and hands it over in x0, so embedding a second description
    // would give one machine two trees and a consumer a choice.
    println!("cargo:rerun-if-changed={}", record_path.display());
    fs::write(out.join("platform-hardware.inc"),
              format!(".equ PL011_BASE, {uart_base}\n.equ GPIO_BASE, {gpio_base}\n"))
        .expect("hardware include");
    let memory = &record["memory"];
    let ram_base = memory["ram_base"].as_str().expect("memory.ram_base");
    let ram_bytes = memory["minimum_ram_bytes"].as_str().expect("memory.minimum_ram_bytes");
    let physmap_base =
        record["virtual_memory"]["physmap"]["start"].as_str()
                                                    .expect("virtual_memory.physmap.start");
    let firmware_heap = &memory["firmware_heap"];
    let firmware_heap_base = firmware_heap["base"].as_str().expect("memory.firmware_heap.base");
    let firmware_heap_bytes = firmware_heap["bytes"].as_str().expect("memory.firmware_heap.bytes");
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
    let fdt_tail_bytes =
        record["memory"]["fdt_tail_bytes"].as_str().expect("memory.fdt_tail_bytes");
    // Only the console and the boot MMU read generated constants. Every other
    // peripheral address comes from the device tree, so the bootloader stays a
    // consumer of the record rather than a second copy of it. The display mode
    // is the one exception beside them: the mailbox request needs the record's
    // geometry before the tree is parsed, and the mode is a property of the
    // platform, not of any device in the tree.
    let (display_width, display_height, _, _) =
        mac_build::platform::current().memory()
                                      .expect("checked platform memory map")
                                      .display_bounds();
    // Where the peripheral registers begin: the lowest declared MMIO window
    // inside the first gigabyte, taken down to a 2 MiB block because the boot
    // tables map that range in blocks. It is not "where RAM ends": the firmware
    // keeps the memory between the ARM's RAM and the peripherals for the
    // VideoCore, and the framebuffer it allocates lives there, so that gap is
    // ordinary memory to map, not device registers.
    let device_aperture_base =
        record["mmio_devices"].as_array()
                              .expect("mmio_devices")
                              .iter()
                              .filter_map(|device| device["address"].as_str())
                              .filter_map(|text| {
                                  u64::from_str_radix(text.trim_start_matches("0x"), 16).ok()
                              })
                              .filter(|address| *address < 0x4000_0000)
                              .min()
                              .expect("the platform must declare at least one MMIO window")
        & !0x1f_ffff;
    fs::write(out.join("platform-memory.rs"),
              format!("pub const PHYSICAL_RAM_BASE: u64 = {ram_base};\n\
                       pub const DEVICE_APERTURE_BASE: u64 = {device_aperture_base:#x};\n\
                       pub const PHYSICAL_RAM_BYTES: u64 = {ram_bytes};\n\
                       pub const PHYSMAP_BASE: u64 = {physmap_base};\n\
                       pub const FIRMWARE_HEAP_BASE: u64 = {firmware_heap_base};\n\
                       pub const FIRMWARE_HEAP_BYTES: u64 = {firmware_heap_bytes};\n\
                       pub const PL011_BASE: u64 = {pl011};\n\
                       pub const GPIO_BASE: u64 = {gpio_base};\n\
                       pub const FDT_TAIL_BYTES: u64 = {fdt_tail_bytes};\n\
                       pub const DISPLAY_WIDTH: u32 = {display_width};\n\
                       pub const DISPLAY_HEIGHT: u32 = {display_height};\n"))
        .expect("memory constants");
    // How much of the boot window is code. The window holds both code and data,
    // and one permission cannot be right for both, so the boot page tables split
    // it at this boundary. It is a declared value: `link.ld` asserts that `.data`
    // begins exactly here, so the mapping and the record cannot drift apart when
    // the payload grows.
    let firmware_code_bytes =
        memory["firmware_code_bytes"].as_str().expect("memory.firmware_code_bytes");
    // The link layout is the platform record's, not a second copy of it: the
    // checked memory map is turned into linker symbols and the script beside
    // this build file places every section and the stack in that one window.
    // The code boundary is a linker symbol too, so the mapping code reads the
    // same value the assertion checks rather than a second copy of it.
    let template = Path::new(env!("CARGO_MANIFEST_DIR")).join("link.ld");
    println!("cargo:rerun-if-changed={}", template.display());
    let (_, symbols) = mac_build::platform::firmware_artifacts().expect("checked boot window");
    let script = out.join("boot.ld");
    let contents = format!("{symbols}__platform_FIRMWARE_CODE_BYTES = {firmware_code_bytes};\n{}",
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
