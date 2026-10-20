//! Host-only resource compiler and guest build support.
//! Binary encoders/decoders live in the no_std mac-formats crate.
pub mod manifest;
pub mod package;

use std::{env, fs, path::Path};

pub use manifest::Manifest;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn error(message: impl Into<String>) -> Box<dyn std::error::Error>
{
    std::io::Error::other(message.into()).into()
}

/// Called by each guest crate's build.rs. The manifest is the single source
/// for resource IDs, the ABI descriptor, and the linker memory reservation.
pub fn build_guest() -> Result<()>
{
    if env::var("TARGET")? != "riscv64imac-unknown-none-elf"
    {
        return Err(error("guest binaries require --target riscv64imac-unknown-none-elf; use \
                          cargo xtask build"));
    }
    let root = env::var("CARGO_MANIFEST_DIR")?;
    let path = Path::new(&root).join("resources.json");
    let manifest = Manifest::read(&path)?;
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");
    for parent in Path::new(&root).ancestors()
    {
        let cargo = parent.join("Cargo.toml");
        if cargo.exists()
        {
            println!("cargo:rerun-if-changed={}", cargo.display());
        }
    }
    let out = env::var("OUT_DIR")?;
    let out = Path::new(&out);
    println!("cargo:rerun-if-changed={}", path.display());
    for source in manifest.source_files(&path)
    {
        println!("cargo:rerun-if-changed={}", source.display());
    }
    fs::write(out.join("resource_ids.rs"), manifest.rust_ids())?;
    let descriptor = manifest.descriptor()?;
    let script =
        include_str!("native.ld").replace("@BASE@", &format!("0x{:x}", descriptor.load_base))
                                 .replace("@LENGTH@",
                                          &format!("0x{:x}",
                                                   descriptor.load_limit - descriptor.load_base));
    let script_path = out.join("native.ld");
    fs::write(&script_path, script)?;
    println!("cargo:rustc-link-arg-bin={}=-T{}", manifest.binary_name(), script_path.display());
    // Module calls use PC-relative addressing, avoiding per-image gp state.
    println!("cargo:rustc-link-arg-bin={}=--no-relax", manifest.binary_name());
    // Packaging retains a compact relocation table for independent app copies.
    if matches!(manifest.role.as_str(), "application" | "code_resource")
    {
        println!("cargo:rustc-link-arg-bin={}=--emit-relocs", manifest.binary_name());
        println!("cargo:rustc-link-arg-bin={}=--defsym=__system_risc_relocation_v1=1",
                 manifest.binary_name());
    }
    Ok(())
}
