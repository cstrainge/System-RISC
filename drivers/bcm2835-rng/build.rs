use std::{env, fs, path::Path};

/// The peripheral model this driver claims, matching `module.json`'s match key.
const RNG_MODEL: &str = "bcm2835-rng";
/// The last register this driver touches, exclusive: the interrupt mask at
/// `0x10` is one word, so the window has to reach `0x14`.
///
/// The machine description states a `0x10`-byte extent, which stops before that
/// register. The platform record already widened its window for this reason, and
/// this assertion is what keeps the two from drifting apart again: a record
/// narrow enough to exclude the mask register would let a driver write outside
/// the window it was granted.
const REQUIRED_BYTES: u64 = 0x14;

fn hex(value: &str) -> u64
{
    let text = value.trim();
    let digits = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")).unwrap_or(text);
    u64::from_str_radix(digits, 16).expect("platform quantities are hexadecimal")
}

fn main()
{
    println!("cargo:rerun-if-env-changed={}", mac_build::platform::PLATFORM_ENV);
    let platform = mac_build::platform::current();
    let record_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../platforms")
                                                           .join(format!("{}.json",
                                                                         platform.name));
    println!("cargo:rerun-if-changed={}", record_path.display());
    // The record is the author of this machine, so this build script reads it
    // rather than carrying a second copy of the numbers.
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(&record_path).expect("platform record"))
            .expect("valid platform record");
    // A platform with no board inventory describes no entropy block, so the
    // driver generates a false flag and is refused at load instead of failing to
    // build: the same package has to build for the machines that have no such
    // peripheral.
    let window = record["hardware"].get("devices")
                                   .and_then(serde_json::Value::as_array)
                                   .and_then(|devices| {
                                       devices.iter()
                                              .find(|device| device["model"] == RNG_MODEL)
                                              .and_then(|device| {
                                                  Some((hex(device["base"].as_str()?),
                                                        hex(device["bytes"].as_str()?)))
                                              })
                                   });
    if let Some((_, bytes)) = window
    {
        assert!(bytes >= REQUIRED_BYTES,
                "the declared entropy window is {bytes:#x} bytes but this driver reaches {REQUIRED_BYTES:#x}, \
                 which includes the interrupt-mask register past the extent the machine description states");
    }
    let (base, bytes) = window.unwrap_or((0, 0));
    let generated = format!("// Generated from the checked platform record.\n\
                             pub const RNG_BASE: usize = {base:#x};\n\
                             pub const RNG_BYTES: usize = {bytes:#x};\n\
                             pub const PLATFORM_DECLARES_RNG: bool = {};\n",
                            window.is_some());
    let out = env::var("OUT_DIR").expect("cargo provides OUT_DIR");
    mac_build::write_if_changed(Path::new(&out).join("platform-rng.rs"), generated)
        .expect("generated platform entropy constants");
    if env::var("TARGET").as_deref() == Ok(platform.rust_target.as_str())
    {
        mac_build::build_guest().expect("BCM2837 Entropy contract and relocatable image must \
                                         validate");
    }
}
