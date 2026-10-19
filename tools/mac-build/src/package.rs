use std::{env, fs, path::Path, process::Command};

use mac_formats::{elf,
                  macbinary::{self, MacFile},
                  native::{Descriptor, Role},
                  resource, text};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{Manifest, Result, error};

pub fn sha256(bytes: &[u8]) -> String
{
    format!("{:x}", Sha256::digest(bytes))
}

pub fn strip_runtime(source: &Path, destination: &Path) -> Result<()>
{
    let status =
        Command::new("llvm-objcopy").arg("--strip-all").arg(source).arg(destination).status()?;
    if !status.success()
    {
        return Err(error("llvm-objcopy failed to strip runtime executable"));
    }
    Ok(())
}

pub fn inspect(bytes: &[u8]) -> Result<Value>
{
    let file = macbinary::decode(bytes)?;
    let resources = resource::decode(&file.resources)?;
    let find = |kind| {
        resources.iter()
                 .find(|r| r.kind == kind && r.id == 0)
                 .ok_or_else(|| error("missing native code or ABI resource"))
    };
    let descriptor = Descriptor::decode(&find(*b"NABI")?.data)?;
    let code = &find(*b"RV64")?.data;
    let image = elf::decode(code, descriptor.load_base, descriptor.load_limit)?;
    Ok(json!({
        "name": text::decode(&file.name), "file_type": text::decode(&file.file_type), "creator": text::decode(&file.creator),
        "role": match descriptor.role { Role::System => "system", Role::Application => "application", Role::CodeResource => "code_resource" },
        "abi_version": descriptor.abi_version, "load_base": descriptor.load_base, "load_limit": descriptor.load_limit,
        "entry": image.entry, "elf_sha256": sha256(code), "resource_fork_sha256": sha256(&file.resources), "macbinary_sha256": sha256(bytes),
        "segments": image.segments.iter().map(|s| json!({"address": s.address, "offset": s.offset, "file_size": s.file_size, "memory_size": s.memory_size, "flags": s.flags, "alignment": s.alignment})).collect::<Vec<_>>(),
        "resources": resources.iter().map(|r| json!({"type": text::decode(&r.kind), "id": r.id, "name": r.name.as_deref().map(text::decode), "attributes": r.attributes, "size": r.data.len(), "sha256": sha256(&r.data)})).collect::<Vec<_>>()
    }))
}

pub fn build(manifest_path: &Path, elf_path: &Path, out: &Path) -> Result<Value>
{
    let manifest = Manifest::read(manifest_path)?;
    let descriptor = manifest.descriptor()?;
    fs::create_dir_all(out)?;
    let debug_elf = fs::read(elf_path)?;
    let runtime = out.join(format!("{}.runtime.elf", manifest.name));
    strip_runtime(elf_path, &runtime)?;
    let elf_bytes = fs::read(&runtime)?;
    elf::decode(&elf_bytes, descriptor.load_base, descriptor.load_limit)?;
    let resources = resource::encode(&manifest.compile_resources(manifest_path, &elf_bytes)?)?;
    let seconds: u64 = env::var("SOURCE_DATE_EPOCH").unwrap_or_else(|_| "0".into()).parse()?;
    let date = mac_date(seconds)?;
    let file = MacFile { name: text::encode(&manifest.name)?,
                         file_type: crate::manifest::fourcc(&manifest.file_type)?,
                         creator: crate::manifest::fourcc(&manifest.creator)?,
                         flags: if manifest.resources.iter().any(|r| r.kind == "BNDL")
                         {
                             0x2000
                         }
                         else
                         {
                             0
                         },
                         created: date,
                         modified: date,
                         data: vec![],
                         resources };
    let binary = macbinary::encode(&file)?;
    let mut report = inspect(&binary)?;
    report["cargo_version"] = manifest.cargo_version.into();
    fs::create_dir_all(out)?;
    for (suffix, bytes) in
        [("elf", &debug_elf), ("data", &file.data), ("rsrc", &file.resources), ("bin", &binary)]
    {
        fs::write(out.join(format!("{}.{suffix}", manifest.name)), bytes)?;
    }
    fs::write(out.join(format!("{}.json", manifest.name)), serde_json::to_vec_pretty(&report)?)?;
    Ok(report)
}

fn mac_date(unix_seconds: u64) -> Result<u32>
{
    Ok(u32::try_from(unix_seconds.checked_add(2_082_844_800)
                                 .ok_or_else(|| error("SOURCE_DATE_EPOCH overflow"))?)?)
}
#[cfg(test)]
mod tests
{
    #[test]
    fn source_dates_support_2026_and_reject_classic_overflow()
    {
        for (unix, mac) in [(1_767_225_600, 3_850_070_400),
                            (1_792_326_896, 3_875_171_696),
                            (1_798_761_599, 3_881_606_399),
                            (2_212_122_495, u32::MAX)]
        {
            assert_eq!(super::mac_date(unix).unwrap(), mac);
        }
        assert!(super::mac_date(2_212_122_496).is_err());
        assert!(super::mac_date(u64::MAX).is_err());
    }
}
