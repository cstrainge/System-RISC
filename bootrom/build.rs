use std::{env, path::Path};
fn main()
{
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_FEATURE");
    if env::var("TARGET") != env::var("HOST")
    {
        mac_build::platform::validate_guest(&env::var("TARGET").unwrap(),&env::var("CARGO_CFG_TARGET_FEATURE").unwrap()).expect("native platform ISA floor; use cargo xtask");
    }
    let script = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("link.ld");
    println!("cargo:rerun-if-changed={}", script.display());
    println!("cargo:rustc-link-arg=-T{}", script.display());
    println!("cargo:rustc-link-arg=--no-relax");
}
