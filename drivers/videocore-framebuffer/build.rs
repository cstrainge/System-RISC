use std::{env, path::Path};

/// The peripheral model this driver claims. The platform record names the same
/// model on the device it declares, so the window below is the record's, not a
/// copy kept here.
const MAILBOX_MODEL: &str = "bcm2835-mailbox";

fn hex(value: &str) -> u64
{
    let text = value.trim();
    let digits = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")).unwrap_or(text);
    u64::from_str_radix(digits, 16).expect("platform quantities are hexadecimal")
}

fn main()
{
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_FEATURE");
    println!("cargo:rerun-if-env-changed={}", mac_build::platform::PLATFORM_ENV);
    let platform = mac_build::platform::current();
    let definition = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../platforms")
                                                          .join(format!("{}.json", platform.name));
    println!("cargo:rerun-if-changed={}", definition.display());
    let (width, height, max_width, max_height) =
        platform.memory().expect("checked platform memory map").display_bounds();
    // The window is the record's `hardware.devices` entry for this model. A
    // platform that does not declare the part gets a false flag rather than a
    // silently wrong address; the module then refuses to compile for it.
    let window =
        platform.hardware
                .as_ref()
                .and_then(|hardware| {
                    hardware.devices.iter().find(|device| device.model == MAILBOX_MODEL)
                })
                .and_then(|device| {
                    Some((hex(device.base.as_deref()?), hex(device.bytes.as_deref()?)))
                });
    if let Some((_, bytes)) = window
    {
        assert!(bytes >= 0x40, "the mailbox register window is 0x40 bytes");
    }
    // The selectable range is the record's initial mode and its declared
    // maximum; a driver must not invent either.
    assert!(max_width >= width && max_height >= height, "display maximum below the initial mode");
    let (base, bytes) = window.unwrap_or((0, 0));
    let generated = format!("// Generated from the checked platform record.\npub const \
                             DISPLAY_WIDTH: u32 = {width};\npub const DISPLAY_HEIGHT: u32 = \
                             {height};\npub const DISPLAY_MAX_WIDTH: u32 = {max_width};\npub \
                             const DISPLAY_MAX_HEIGHT: u32 = {max_height};\npub const \
                             MAILBOX_BASE: usize = {base:#x};\npub const MAILBOX_BYTES: usize = \
                             {bytes:#x};\npub const PLATFORM_DECLARES_MAILBOX: bool = {};\n",
                            window.is_some());
    let out = env::var("OUT_DIR").expect("cargo provides OUT_DIR");
    mac_build::write_if_changed(Path::new(&out).join("platform-display.rs"), generated)
        .expect("generated platform display constants");
    // The machine adapter needs the physical map, so write the same constants
    // `build_guest` would. Same list, same file, so the content is identical.
    let virtual_map =
        platform.virtual_memory()
                .expect("checked virtual map")
                .rust_constants_for(&["VM_PHYSMAP_START", "VM_PHYSMAP_END",
                                      "VM_PHYSMAP_PHYSICAL_BASE"])
                .expect("checked bootstrap virtual symbols");
    mac_build::write_if_changed(Path::new(&out).join("platform-virtual.rs"), virtual_map)
        .expect("generated platform virtual constants");
    if env::var("TARGET").as_deref() == Ok(platform.rust_target.as_str())
    {
        mac_build::build_guest().expect("VideoCore Framebuffer contract and relocatable image \
                                         must validate");
    }
}
