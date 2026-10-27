//! Host-only resource compiler and guest build support.
//! Binary encoders/decoders live in the no_std mac-formats crate.
pub mod manifest;
pub mod module;
pub mod package;
pub mod paths;
pub mod platform;

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
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_FEATURE");
    println!("cargo:rerun-if-env-changed={}", platform::PLATFORM_ENV);
    let definition =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../platforms")
                                             .join(format!("{}.json", platform::current().name));
    println!("cargo:rerun-if-changed={}", definition.display());
    platform::validate_guest(&env::var("TARGET")?, &env::var("CARGO_CFG_TARGET_FEATURE")?)?;
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
    // Cargo may add package/hash directories below profile/build. Resolve
    // that boundary instead of assuming a fixed OUT_DIR ancestor depth.
    // Watching nonexistent definition paths makes every build dirty and also
    // misses the actual artifacts on a changed definition.
    let artifacts = paths::artifact_profile(out)?;
    for source in manifest.source_files(&path, artifacts)
    {
        println!("cargo:rerun-if-changed={}", source.display());
    }
    fs::write(out.join("resource_ids.rs"), manifest.rust_ids())?;
    if module::kind(&manifest).is_some()
    {
        println!("cargo:rerun-if-changed={}", Path::new(&root).join("module.json").display());
        fs::write(out.join("module_contract.bin"), module::declaration(&path, &manifest)?)?;
    }
    let descriptor = manifest.descriptor()?;
    platform::current().validate_image(descriptor.role, descriptor.load_base,
                                       descriptor.load_limit)?;
    let (constants, platform_symbols) = platform::guest_artifacts()?;
    fs::write(out.join("platform.rs"), constants)?;
    let (system_memory, _) = platform::memory_artifacts()?;
    fs::write(out.join("platform-memory.rs"), system_memory)?;
    fs::write(out.join("platform-virtual.rs"),
              platform::current().virtual_memory()?
                                 .rust_constants_for(&["VM_SYSTEM_IMAGE_BASE",
                                                       "VM_SYSTEM_IMAGE_LIMIT",
                                                       "VM_PHYSMAP_START", "VM_PHYSMAP_END",
                                                       "VM_PHYSMAP_PHYSICAL_BASE",
                                                       "VM_SYSTEM_STACKS_START"])?)?;
    let mut script =
        include_str!("native.ld").replace("@BASE@", &format!("0x{:x}", descriptor.load_base))
                                 .replace("@LENGTH@",
                                          &format!("0x{:x}",
                                                   descriptor.load_limit - descriptor.load_base));
    script.push_str(&platform_symbols);
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
