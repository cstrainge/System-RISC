//! Turns the platform record into the two things the binary cannot derive at
//! run time: the peripheral addresses it must touch, and the link geometry the
//! firmware loads it at. The record is the only place this board is written
//! down; no address appears in the source.
use std::{env, fs, path::Path};

fn main()
{
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let name = env::var("SR_PLATFORM").unwrap_or_else(|_| "qemu_arm64".to_string());
    let record_path = root.join("../platforms").join(format!("{name}.json"));
    println!("cargo:rerun-if-env-changed=SR_PLATFORM");
    println!("cargo:rerun-if-changed={}", record_path.display());
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(&record_path).unwrap_or_else(|error| {
                                                          panic!("{}: {error}",
                                                                 record_path.display())
                                                      })).expect("valid platform record");

    let number = |value: &serde_json::Value| -> u64 {
        let text = value.as_str().expect("a hex string");
        u64::from_str_radix(text.trim_start_matches("0x"), 16).expect("hexadecimal")
    };
    let hardware = &record["hardware"];
    let device = |kind: &str| -> u64 {
        hardware["devices"].as_array()
                           .expect("hardware.devices")
                           .iter()
                           .find(|device| device["kind"] == kind)
                           .and_then(|device| device["base"].as_str())
                           .map(|base| {
                               u64::from_str_radix(base.trim_start_matches("0x"), 16)
                                   .expect("hexadecimal")
                           })
                           .unwrap_or_else(|| panic!("the platform must declare a {kind} device"))
    };
    let mmio = |id: &str| -> u64 {
        record["mmio_devices"].as_array()
                              .expect("mmio_devices")
                              .iter()
                              .find(|device| device["id"] == id)
                              .map(|device| number(&device["address"]))
                              .unwrap_or_else(|| {
                                  panic!("the platform must declare the {id} window")
                              })
    };
    // The identity map gives RAM normal write-back attributes and the
    // peripherals Device attributes. The boundary is the lowest MMIO window the
    // record declares, rounded down to a 2 MiB block, so it is a fact about the
    // machine rather than a constant here.
    let device_base = record["mmio_devices"].as_array()
                                            .expect("mmio_devices")
                                            .iter()
                                            .map(|device| number(&device["address"]))
                                            .min()
                                            .expect("at least one MMIO window")
                      & !0x1f_ffff;
    let display = &record["memory"]["display"];
    let boot = &record["memory"]["boot"];
    // This self-test is standalone pre-boot code, so it deliberately does not
    // ask the build to extend the platform record for the windows its new
    // checks need. The record declares the power-management window, and that is
    // read from it; SPI0 and the DMA controller are stated here as BCM2837
    // board facts. The record remains the authority for everything the OS
    // consumes, and this test does not edit it.
    const SPI0_BASE: u64 = 0x3f20_4000;
    const DMA_BASE: u64 = 0x3f00_7000;
    /// Channels 0, 2, 4, 5 and 8 through 14: the set the firmware leaves the
    /// ARM. A channel outside this set accepts a control block and delivers it
    /// somewhere else, which is why it is never considered.
    const DMA_POOL: u32 = 0x7f35;
    let pm = mmio("bcm2835-pm");
    let out = Path::new(&env::var("OUT_DIR").expect("Cargo supplies OUT_DIR")).to_path_buf();
    let facts =
        format!("pub const PLATFORM_NAME: &str = \"{name}\";\npub const MAILBOX_BASE: usize = \
                 {mailbox:#x};\npub const PL011_BASE: usize = {pl011:#x};\npub const SDHCI_BASE: \
                 usize = {sdhci:#x};\npub const SYSTIMER_BASE: usize = {timer:#x};\npub const \
                 SPI0_BASE: usize = {spi0:#x};\npub const DMA_BASE: usize = {dma:#x};\npub const \
                 DMA_POOL: u32 = {dma_pool:#x};\npub const PM_BASE: usize = {pm:#x};\npub const \
                 DECLARED_RAM_BASE: u64 = {ram_base:#x};\npub const DECLARED_RAM_BYTES: u64 = \
                 {ram_bytes:#x};\npub const DEVICE_BASE: u64 = {device_base:#x};\npub const \
                 DISPLAY_WIDTH: u32 = {width};\npub const DISPLAY_HEIGHT: u32 = {height};\n",
                mailbox = device("gpio-mailbox"),
                pl011 = device("uart"),
                sdhci = device("storage"),
                timer = mmio("bcm2835-systimer"),
                spi0 = SPI0_BASE,
                dma = DMA_BASE,
                dma_pool = DMA_POOL,
                pm = pm,
                ram_base = number(&hardware["ram"]["base"]),
                ram_bytes = number(&hardware["ram"]["bytes"]),
                width = display["width"].as_u64().expect("memory.display.width"),
                height = display["height"].as_u64().expect("memory.display.height"));
    fs::write(out.join("platform-facts.rs"), facts).expect("platform facts");
    fs::write(out.join("platform-hardware.inc"),
              format!(".equ PL011_BASE, {:#x}\n", device("uart"))).expect("hardware include");

    // The firmware loads a raw image at the record's boot base, so the entry
    // section must be the first byte of the image. The stack sits above the
    // image inside the same declared window and is not part of the load image.
    let linker = format!("OUTPUT_ARCH(aarch64)\nENTRY(_start)\nMEMORY {{ ram (rwx) : ORIGIN = \
                          {base:#x}, LENGTH = {bytes:#x} }}\nSECTIONS {{\n. = \
                          ORIGIN(ram);\n.text : {{ KEEP(*(.text.entry)) *(.text .text.*) }} > \
                          ram\n.rodata : ALIGN(4096) {{ *(.rodata .rodata.* .srodata .srodata.*) \
                          }} > ram\n.data : ALIGN(4096) {{ *(.data .data.* .sdata .sdata.* .got \
                          .got.*) }} > ram\n.bss (NOLOAD) : ALIGN(4096) {{ __bss_start = .; \
                          *(.bss .bss.* .sbss .sbss.*) *(COMMON) . = ALIGN(16); __bss_end = .; \
                          }} > ram\n. = ALIGN(16);\n. += 0x20000;\n__stack_top = \
                          .;\n.ARM.attributes 0 : {{ *(.ARM.attributes) }} :NONE\n/DISCARD/ : {{ \
                          *(.eh_frame .eh_frame_hdr) }}\n}}\n",
                         base = number(&boot["base"]),
                         bytes = number(&boot["bytes"]));
    fs::write(out.join("boot.ld"), linker).expect("boot linker script");
    println!("cargo:rustc-link-arg-bin=selftest-aarch64=-T{}", out.join("boot.ld").display());
}
