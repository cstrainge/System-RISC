use std::{env, fs, path::Path};
fn main()
{
    let mut constants = String::new();
    for name in ["SYSTEM", "FINDER"]
    {
        let key = format!("SYSTEM7_{name}_ENTRY");
        println!("cargo:rerun-if-env-changed={key}");
        let value: u64 = env::var(&key).expect("use cargo xtask check to supply native entry \
                                                addresses")
                                       .parse()
                                       .unwrap();
        constants.push_str(&format!("const {name}_ENTRY: usize = {value};\n"));
    }
    fs::write(Path::new(&env::var("OUT_DIR").unwrap()).join("entries.rs"), constants).unwrap();
    let script = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("link.ld");
    println!("cargo:rerun-if-changed={}", script.display());
    println!("cargo:rustc-link-arg=-T{}", script.display());
    println!("cargo:rustc-link-arg=--no-relax");
}
