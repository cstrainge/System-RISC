//! Replace only the probe CDEF on a private System package; no source/base mutation.
use std::{env, fs,
          path::{Path, PathBuf}};
fn main() -> mac_build::Result<()>
{
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() != 3 && args.len() != 5
    {
        return Err(mac_build::error("expected PROBE_ELF BASE_SYSTEM_BIN OUTPUT_SYSTEM_BIN \
                                     [PRIVATE_SYSTEM_ELF BASE_ARTIFACT_DIR]"));
    }
    let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources.json");
    let manifest = mac_build::Manifest::read(&manifest_path)?;
    let elf = PathBuf::from(&args[0]);
    let compiled = manifest.compile_resources(&manifest_path, &fs::read(&elf)?, elf.parent())?;
    let replacement = compiled.into_iter()
                              .find(|r| r.kind == *b"CDEF" && r.id == -32759)
                              .ok_or_else(|| mac_build::error("probe CDEF did not compile"))?;
    let mut system = mac_formats::macbinary::decode(&fs::read(&args[1])?)?;
    let mut resources = mac_formats::resource::decode(&system.resources)?;
    let original = resources.clone();
    let mut replacements = 0;
    if args.len() == 5
    {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../system/resources.json");
        let manifest = mac_build::Manifest::read(&path)?;
        let compiled =
            manifest.compile_resources(&path, &fs::read(&args[3])?, Some(Path::new(&args[4])))?;
        for kind in [*b"RV64", *b"NABI", *b"RLOC"]
        {
            let replacement =
                compiled.iter().find(|resource| resource.kind == kind && resource.id == 0);
            let slot =
                resources.iter_mut().find(|resource| resource.kind == kind && resource.id == 0);
            match (replacement, slot)
            {
                (Some(replacement), Some(slot)) =>
                {
                    *slot = replacement.clone();
                    replacements += 1;
                }
                (None, None) =>
                {}
                _ =>
                {
                    return Err(mac_build::error("private System and base code resource sets \
                                                 differ; rebuild the whole check image"));
                }
            }
        }
    }
    let slot =
        resources.iter_mut()
                 .find(|r| r.kind == replacement.kind && r.id == replacement.id)
                 .ok_or_else(|| {
                     mac_build::error("base System has no Static Text CDEF to replace")
                 })?;
    *slot = replacement;
    replacements += 1;
    let mut unchanged = 0;
    for resource in &resources
    {
        let is_replaced = resource.kind == *b"CDEF" && resource.id == -32759
                          || args.len() == 5
                             && resource.id == 0
                             && [*b"RV64", *b"NABI", *b"RLOC"].contains(&resource.kind);
        if !is_replaced
        {
            let before =
                original.iter()
                        .find(|before| before.kind == resource.kind && before.id == resource.id)
                        .ok_or_else(|| {
                            mac_build::error("private System introduced an unrelated resource")
                        })?;
            if before != resource
            {
                return Err(mac_build::error("private System changed unrelated resource bytes \
                                             or metadata"));
            }
            unchanged += 1;
        }
    }
    fs::write(Path::new(&args[2]).with_extension("mutation.json"),
              format!("{{\"resource_count\":{},\"unchanged\":{},\"replaced\":{},\"\
                       private_system\":{}}}\n",
                      resources.len(),
                      unchanged,
                      replacements,
                      args.len() == 5))?;
    system.resources = mac_formats::resource::encode_native(&resources)?;
    fs::write(&args[2], mac_formats::macbinary::encode(&system)?)?;
    Ok(())
}
