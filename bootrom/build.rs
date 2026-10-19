use std::{env, path::Path};
fn main()
{
    let script = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("link.ld");
    println!("cargo:rerun-if-changed={}", script.display());
    println!("cargo:rustc-link-arg=-T{}", script.display());
    println!("cargo:rustc-link-arg=--no-relax");
}
