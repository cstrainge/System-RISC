use std::{env, fs, path::Path};

/// The peripheral model this driver claims, matching `module.json`'s match key.
const SPI_MODEL: &str = "bcm2835-spi0";
/// The feeder's inventory identity, whose `pool` is the channel mask the board
/// leaves to the ARM.
const DMA_ID: &str = "bcm2835-dma";

fn hex(value: &str) -> u64
{
    let text = value.trim();
    let digits = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")).unwrap_or(text);
    u64::from_str_radix(digits, 16).expect("platform quantities are hexadecimal")
}

fn key(text: &str) -> Option<[u8; 16]>
{
    if text.len() != 32
    {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate()
    {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

fn main()
{
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_FEATURE");
    println!("cargo:rerun-if-env-changed={}", mac_build::platform::PLATFORM_ENV);
    let platform = mac_build::platform::current();
    let record_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../platforms")
                                                           .join(format!("{}.json",
                                                                         platform.name));
    println!("cargo:rerun-if-changed={}", record_path.display());
    // The record is the author of this machine, and a driver's build script is
    // one more consumer of it: reading the record rather than re-deriving the
    // numbers keeps one declaration of each fact.
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(&record_path).expect("platform record"))
            .expect("valid platform record");
    // A platform that declares no hardware section has no board inventory, so
    // this driver has no window there and says so by generating a false flag
    // rather than by failing to build: the module is simply refused at load.
    let window = record["hardware"].get("devices")
                                   .and_then(serde_json::Value::as_array)
                                   .and_then(|devices| {
                                       devices.iter()
                                              .find(|device| device["model"] == SPI_MODEL)
                                              .and_then(|device| {
                                                  Some((hex(device["base"].as_str()?),
                                                        hex(device["bytes"].as_str()?)))
                                              })
                                   });
    if let Some((_, bytes)) = window
    {
        assert!(bytes >= 0x20, "the SPI register window covers at least the status register");
    }
    // The channel pool is the record's, not a constant here: which channels the
    // board leaves to the ARM is a fact about the board, and a driver that
    // assumed all fifteen would take one the firmware is using.
    let dma = record["mmio_devices"].as_array()
                                    .and_then(|devices| {
                                        devices.iter().find(|device| device["id"] == DMA_ID)
                                    });
    let pool_key = dma.and_then(|device| key(device["key"].as_str()?)).unwrap_or([0; 16]);
    let pool_mask = dma.and_then(|device| device["pool"].as_u64()).unwrap_or(0);
    let (base, bytes) = window.unwrap_or((0, 0));
    let generated = format!("// Generated from the checked platform record.\n\
                             pub const SPI_BASE: usize = {base:#x};\n\
                             pub const SPI_BYTES: usize = {bytes:#x};\n\
                             pub const PLATFORM_DECLARES_SPI: bool = {};\n\
                             pub const POOL_KEY: [u8; 16] = {pool_key:?};\n\
                             pub const POOL_MASK: u64 = {pool_mask:#x};\n\
                             pub const POOL_DECLARED: bool = {};\n",
                            window.is_some(), pool_mask != 0);
    let out = env::var("OUT_DIR").expect("cargo provides OUT_DIR");
    mac_build::write_if_changed(Path::new(&out).join("platform-spi.rs"), generated)
        .expect("generated platform SPI constants");
    if env::var("TARGET").as_deref() == Ok(platform.rust_target.as_str())
    {
        mac_build::build_guest().expect("SPI0 Timed Output contract and relocatable image must \
                                         validate");
    }
}
