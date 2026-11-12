use std::{env, path::Path};

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
    // The selectable range is the record's initial mode and its declared
    // maximum; a driver must not invent either.
    assert!(max_width >= width && max_height >= height, "display maximum below the initial mode");
    let generated = format!("// Generated from the checked platform record.\npub const \
                             DISPLAY_WIDTH: u32 = {width};\npub const DISPLAY_HEIGHT: u32 = \
                             {height};\npub const DISPLAY_MAX_WIDTH: u32 = {max_width};\npub \
                             const DISPLAY_MAX_HEIGHT: u32 = {max_height};\n");
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
