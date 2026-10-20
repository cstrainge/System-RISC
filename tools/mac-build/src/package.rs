use std::{env, fs, path::Path, process::Command};

use mac_formats::{elf,
                  macbinary::{self, MacFile},
                  native::{Descriptor, Role, image},
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
    let image = image::decode(code, &descriptor)?;
    Ok(json!({
        "name": text::decode(&file.name), "file_type": text::decode(&file.file_type), "creator": text::decode(&file.creator),
        "role": match descriptor.role { Role::System => "system", Role::Application => "application", Role::CodeResource => "code_resource" },
        "abi_version": descriptor.abi_version, "load_base": descriptor.load_base, "load_limit": descriptor.load_limit,
        "format_version": mac_formats::native::FORMAT_VERSION, "entry": descriptor.entry, "code_sha256": sha256(code), "resource_fork_sha256": sha256(&file.resources), "macbinary_sha256": sha256(bytes),
        "segments": image.segments.iter().map(|s| json!({"address": descriptor.load_base + s.offset, "offset": s.offset, "length": s.length, "flags": s.flags})).collect::<Vec<_>>(),
        "resources": resources.iter().map(|r| json!({"type": text::decode(&r.kind), "id": r.id, "name": r.name.as_deref().map(text::decode), "attributes": r.attributes, "size": r.data.len(), "sha256": sha256(&r.data)})).collect::<Vec<_>>()
    }))
}

pub fn build(manifest_path: &Path, elf_path: &Path, out: &Path) -> Result<Value>
{
    let manifest = Manifest::read(manifest_path)?;
    if manifest.archive
    {
        return Err(error("use rebuild for a lossless archive"));
    }
    let descriptor = manifest.descriptor()?;
    fs::create_dir_all(out)?;
    let debug_elf = fs::read(elf_path)?;
    let runtime = out.join(format!("{}.runtime.elf", manifest.name));
    strip_runtime(elf_path, &runtime)?;
    let elf_bytes = fs::read(&runtime)?;
    elf::decode(&elf_bytes, descriptor.load_base, descriptor.load_limit)?;
    let mut compiled = manifest.compile_resources(manifest_path, &elf_bytes)?;
    if matches!(descriptor.role, Role::Application | Role::CodeResource)
    {
        let image = elf::decode(&debug_elf, descriptor.load_base, descriptor.load_limit)?;
        let data = elf::relocation::extract(&debug_elf, &image, descriptor.load_base)?
            .ok_or_else(|| error("native modules require relocation-capable linking"))?;
        {
            compiled.push(resource::Resource { kind: *b"RLOC",
                                               id: 0,
                                               name: None,
                                               attributes: 0,
                                               data });
        }
    }
    let resources = resource::encode(&compiled)?;
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
/// Rebuild a lossless ResEdit archive without a compiler, Cargo project, or
/// regenerated executable/version resources. Both fork payloads are retained.
pub fn rebuild(manifest_path: &Path, out: &Path) -> Result<Value>
{
    let manifest = Manifest::read(manifest_path)?;
    if !manifest.archive
    {
        return Err(error("rebuild requires an archive manifest"));
    }
    let resources = resource::encode(&manifest.compile_resources(manifest_path, &[])?)?;
    let data = if let Some(crate::manifest::Payload::Hex { hex }) = &manifest.data_fork
    {
        if !hex.is_ascii() || !hex.len().is_multiple_of(2)
        {
            return Err(error("invalid archive data fork hex"));
        }
        (0..hex.len()).step_by(2)
                      .map(|at| u8::from_str_radix(&hex[at..at + 2], 16))
                      .collect::<std::result::Result<Vec<_>, _>>()?
    }
    else
    {
        Vec::new()
    };
    let file = MacFile { name: text::encode(&manifest.name)?,
                         file_type: crate::manifest::fourcc(&manifest.file_type)?,
                         creator: crate::manifest::fourcc(&manifest.creator)?,
                         flags: manifest.file_flags,
                         created: manifest.created,
                         modified: manifest.modified,
                         data,
                         resources };
    fs::create_dir_all(out)?;
    // A slash is legal in an HFS catalog name but must never become a host
    // directory or an absolute path. Preserve the Macintosh header verbatim.
    let host_name = if manifest.name.contains(['/', '\\'])
    {
        format!("archive-{}.bin",
                text::encode(&manifest.name)?.iter()
                                             .map(|b| format!("{b:02x}"))
                                             .collect::<String>())
    }
    else
    {
        format!("{}.bin", manifest.name)
    };
    let path = out.join(host_name);
    fs::write(&path, macbinary::encode(&file)?)?;
    Ok(json!({"file":path,"resource_fork_sha256":sha256(&file.resources),"data_fork_sha256":sha256(&file.data),"resources":manifest.resources.len(),"archive":true}))
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
