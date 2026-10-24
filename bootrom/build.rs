use std::{env, fs, path::Path};
fn main()
{
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_FEATURE");
    println!("cargo:rerun-if-env-changed={}", mac_build::platform::PLATFORM_ENV);
    let platform = mac_build::platform::current();
    let definition = Path::new(env!("CARGO_MANIFEST_DIR")).join("../platforms")
                                                          .join(format!("{}.json", platform.name));
    println!("cargo:rerun-if-changed={}", definition.display());
    if env::var("TARGET") != env::var("HOST")
    {
        mac_build::platform::validate_guest(&env::var("TARGET").unwrap(), &env::var("CARGO_CFG_TARGET_FEATURE").unwrap()).expect("native platform ISA floor; use cargo xtask");
    }
    let template = Path::new(env!("CARGO_MANIFEST_DIR")).join("link.ld");
    println!("cargo:rerun-if-changed={}", template.display());
    let (_, symbols) = mac_build::platform::guest_artifacts().expect("checked platform symbols");
    let (_, constants) = mac_build::platform::memory_artifacts().expect("checked boot memory map");
    let out = env::var("OUT_DIR").unwrap();
    let out = Path::new(&out);
    fs::write(out.join("platform-memory.rs"), constants).unwrap();
    let script = out.join("boot.ld");
    fs::write(&script, symbols + &fs::read_to_string(template).unwrap()).unwrap();
    println!("cargo:rustc-link-arg-bin=bootrom=-T{}", script.display());
    println!("cargo:rustc-link-arg-bin=bootrom=--no-relax");
}
