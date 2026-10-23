//! Replace only the probe CDEF on a private System package; no source/base mutation.
use std::{env, fs, path::{Path, PathBuf}};
fn main() -> mac_build::Result<()> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() != 3 { return Err(mac_build::error("expected ELF BASE_SYSTEM_BIN OUTPUT_SYSTEM_BIN")); }
    let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources.json");
    let manifest = mac_build::Manifest::read(&manifest_path)?;
    let elf = PathBuf::from(&args[0]);
    let compiled = manifest.compile_resources(&manifest_path, &fs::read(&elf)?, elf.parent())?;
    let replacement = compiled.into_iter().find(|r| r.kind == *b"CDEF" && r.id == -32759)
        .ok_or_else(|| mac_build::error("probe CDEF did not compile"))?;
    let mut system = mac_formats::macbinary::decode(&fs::read(&args[1])?)?;
    let mut resources = mac_formats::resource::decode(&system.resources)?;
    let slot = resources.iter_mut().find(|r| r.kind == replacement.kind && r.id == replacement.id)
        .ok_or_else(|| mac_build::error("base System has no Static Text CDEF to replace"))?;
    *slot = replacement;
    system.resources = mac_formats::resource::encode(&resources)?;
    fs::write(&args[2], mac_formats::macbinary::encode(&system)?)?;
    Ok(())
}
