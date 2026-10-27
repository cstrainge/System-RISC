//! Build contracts from resolved SDK/target/Cargo information. The linked copy
//! is checked against current inputs before generating the DABI/EABI resource.
use std::{fs, path::Path, process::Command};

use mac_formats::{native::Role, resource::Resource};
use mac_module::{ClassId, Dependency, DeviceMatch, ExtensionTarget, Offer, PackageKind, Service,
                 Spec, Target};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{Manifest, Result, error};
pub const SECTION: &[u8] = b".system_risc_module";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration
{
    identity:     String,
    #[serde(default)]
    heap_bytes:   u32,
    stack_bytes:  u32,
    #[serde(default)]
    interfaces:   Vec<Interface>,
    #[serde(default)]
    dependencies: Vec<Required>,
    #[serde(default)]
    services:     Vec<Required>,
    #[serde(default)]
    matches:      Vec<Match>
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Interface
{
    class:          String,
    version:        u32,
    request_schema: u32,
    reply_schema:   u32,
    max_request:    u32,
    max_reply:      u32,
    #[serde(default)]
    exclusive:      bool,
    #[serde(default)]
    serialized:     bool,
    #[serde(default)]
    capabilities:   u64
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Required
{
    class:    String,
    version:  u32,
    #[serde(default)]
    module:   Option<String>,
    #[serde(default)]
    optional: bool
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Match
{
    bus:   String,
    value: String,
    mask:  String
}
fn id(text: &str) -> Result<[u8; 16]>
{
    if text.len() != 32 || !text.is_ascii()
    {
        return Err(error("module identity/match must be 32 hexadecimal digits"));
    }
    let mut result = [0; 16];
    for (index, byte) in result.iter_mut().enumerate()
    {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)?;
    }
    Ok(result)
}
fn class(text: &str) -> Result<ClassId>
{
    let bytes: [u8; 4] = text.as_bytes()
                             .try_into()
                             .map_err(|_| error("module class ID must be four ASCII bytes"))?;
    if !bytes.iter().all(u8::is_ascii_graphic)
    {
        return Err(error("module class ID must be four ASCII bytes"));
    }
    Ok(ClassId(bytes))
}
pub fn kind(manifest: &Manifest) -> Option<PackageKind>
{
    match manifest.file_type.as_str()
    {
        "DRVR" => Some(PackageKind::Driver),
        "INIT" => Some(PackageKind::Extension),
        _ => None
    }
}
pub fn declaration(path: &Path, manifest: &Manifest) -> Result<Vec<u8>>
{
    let kind =
        kind(manifest).ok_or_else(|| error("module declaration requires DRVR or INIT file type"))?;
    if manifest.descriptor()?.role != Role::CodeResource
    {
        return Err(error("kernel modules require code_resource role"));
    }
    let declared: Declaration =
        serde_json::from_slice(&fs::read(path.parent().unwrap().join("module.json"))?)?;
    let offers =
        declared.interfaces
                .iter()
                .map(|i| {
                    Ok(Offer { class:          class(&i.class)?,
                               version:        i.version,
                               request_schema: i.request_schema,
                               reply_schema:   i.reply_schema,
                               max_request:    i.max_request,
                               max_reply:      i.max_reply,
                               flags:          Offer::KERNEL_ONLY
                                               | if i.exclusive { Offer::EXCLUSIVE } else { 0 }
                                               | if i.serialized { Offer::SERIALIZED } else { 0 },
                               capabilities:   i.capabilities })
                })
                .collect::<Result<Vec<_>>>()?;
    let dependencies = declared.dependencies
                               .iter()
                               .map(|d| {
                                   Ok(Dependency { module:   d.module
                                                              .as_deref()
                                                              .map(id)
                                                              .transpose()?
                                                              .unwrap_or([0; 16]),
                                                   class:    class(&d.class)?,
                                                   version:  d.version,
                                                   optional: d.optional })
                               })
                               .collect::<Result<Vec<_>>>()?;
    let services = declared.services
                           .iter()
                           .map(|s| {
                               if s.module.is_some()
                               {
                                   return Err(error("kernel service requirement cannot select \
                                                     a module"));
                               }
                               Ok(Service { service:  class(&s.class)?,
                                            version:  s.version,
                                            optional: s.optional })
                           })
                           .collect::<Result<Vec<_>>>()?;
    let devices = declared.matches
                          .iter()
                          .map(|m| {
                              Ok(DeviceMatch { bus:   class(&m.bus)?,
                                               value: id(&m.value)?,
                                               mask:  id(&m.mask)? })
                          })
                          .collect::<Result<Vec<_>>>()?;
    let rustc =
        Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into())).arg("--version")
                                                                                 .output()?;
    if !rustc.status.success()
    {
        return Err(error("cannot obtain module compiler provenance"));
    }
    let rustc = String::from_utf8(rustc.stdout)?;
    let provenance = format!("{}; profile={}; ABI=LP64; {}",
                             crate::platform::current().rust_target,
                             crate::platform::current().llvm_profile,
                             rustc.trim());
    let spec = Spec { kind,
                      kernel_abi: mac_formats::native::ABI_VERSION,
                      target: Target::RVA23U64,
                      identity: id(&declared.identity)?,
                      code_id: manifest.module_code_id(),
                      code_digest: [0; 32],
                      heap_bytes: declared.heap_bytes,
                      stack_bytes: declared.stack_bytes,
                      name: &manifest.name,
                      version: &manifest.cargo_version,
                      provenance: &provenance,
                      offers: &offers,
                      dependencies: &dependencies,
                      services: &services,
                      devices: &devices };
    let mut bytes =
        vec![0; spec.encoded_len().map_err(|e| error(format!("module contract: {e:?}")))?];
    spec.encode(&mut bytes).map_err(|e| error(format!("module contract: {e:?}")))?;
    Ok(bytes)
}
pub fn finish(path: &Path, manifest: &Manifest, elf: &[u8], resources: &mut Vec<Resource>)
              -> Result<()>
{
    let kind = kind(manifest).ok_or_else(|| error("not a kernel module"))?;
    let linked = mac_formats::elf::relocation::named_section(elf, SECTION)?.ok_or_else(|| {
                     error("module lacks linked SDK contract; rebuild using mac-driver")
                 })?;
    let mut metadata = declaration(path, manifest)?;
    if linked != metadata
    {
        return Err(error("linked module SDK/build contract differs; rebuild the module"));
    }
    let get = |kind| {
        resources.iter()
                 .find(|r| r.kind == kind && r.id == 0)
                 .ok_or_else(|| error("module lacks native envelope parts"))
    };
    let mut descriptor = mac_formats::native::Descriptor::decode(&get(*b"NABI")?.data)?;
    let image = mac_formats::elf::decode(elf, descriptor.load_base, descriptor.load_limit)?;
    descriptor.load_limit =
        descriptor.load_base
                  .checked_add(mac_formats::elf::relocation::span(&image, descriptor.load_base)?)
                  .ok_or_else(|| error("module reservation overflow"))?;
    let mut envelope = descriptor.encode()?.to_vec();
    envelope.extend_from_slice(&get(*b"RV64")?.data);
    envelope.extend_from_slice(&get(*b"RLOC")?.data);
    metadata[88..120].copy_from_slice(&Sha256::digest(&envelope));
    resources.retain(|r| !(r.id == 0 && [*b"NABI", *b"RV64", *b"RLOC"].contains(&r.kind)));
    let code_id = manifest.module_code_id();
    if resources.iter().any(|r| {
                           r.kind == kind.file_type()
                           || r.id == code_id
                              && [kind.code_type(), *b"DABI", *b"EABI"].contains(&r.kind)
                       })
    {
        return Err(error("module entry/contract resources are generated, not supplied"));
    }
    resources.push(entry_resource(kind, code_id, envelope)?);
    resources.push(Resource { kind:       kind.metadata_type(),
                              id:         code_id,
                              name:       None,
                              attributes: 0,
                              data:       metadata });
    match kind
    {
        PackageKind::Driver =>
        {
            mac_formats::module::Package::inspect(resources, kind.file_type())?;
        }
        PackageKind::Extension =>
        {
            let target = ExtensionTarget::from_resource_id(code_id).ok_or_else(|| {
                                                                       error("extension target \
                                                                              must select RV64 0 \
                                                                              (System) or RV64 1 \
                                                                              (Finder)")
                                                                   })?;
            mac_formats::module::Package::inspect_extension(resources, target)?;
        }
    }
    Ok(())
}

fn entry_resource(kind: PackageKind, code_id: i16, data: Vec<u8>) -> Result<Resource>
{
    let name = match kind
    {
        PackageKind::Driver => None,
        PackageKind::Extension =>
        {
            Some(ExtensionTarget::from_resource_id(code_id).ok_or_else(|| {
                                                               error("extension target must \
                                                                      select RV64 0 (System) or \
                                                                      RV64 1 (Finder)")
                                                           })?
                                                           .conventional_name()
                                                           .as_bytes()
                                                           .to_vec())
        }
    };
    Ok(Resource { kind: kind.code_type(),
                  id: code_id,
                  name,
                  attributes: 0,
                  data })
}

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn builders_emit_conventional_extension_names_and_keep_driver_entries_distinct()
    {
        for target in ExtensionTarget::ALL
        {
            let resource =
                entry_resource(PackageKind::Extension, target.resource_id(), vec![1]).unwrap();
            assert_eq!(resource.kind, *b"RV64");
            assert_eq!(resource.id, target.resource_id());
            assert_eq!(resource.name.as_deref(), Some(target.conventional_name().as_bytes()));
        }
        let driver = entry_resource(PackageKind::Driver, 0, vec![1]).unwrap();
        assert_eq!((driver.kind, driver.id, driver.name), (*b"DRVR", 0, None));
        let cause = entry_resource(PackageKind::Extension, 2, vec![]).unwrap_err().to_string();
        assert!(cause.contains("System") && cause.contains("Finder"), "{cause}");
    }
}
