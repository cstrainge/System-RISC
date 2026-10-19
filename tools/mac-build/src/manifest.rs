use std::{collections::BTreeSet,
          fs,
          path::{Path, PathBuf}};

use mac_formats::{native::{Descriptor, Role},
                  resource::Resource,
                  text};
use serde::{Deserialize, Serialize};

use crate::{Result, error};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest
{
    pub name:          String,
    /// Cargo executable name, when the Macintosh filename contains spaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_name:   Option<String>,
    pub file_type:     String,
    pub creator:       String,
    pub role:          String,
    pub abi_version:   u32,
    pub load_base:     String,
    pub load_limit:    String,
    #[serde(skip)]
    pub cargo_version: String,
    pub description:   String,
    #[serde(default)]
    pub resources:     Vec<Definition>
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Definition
{
    #[serde(rename = "type")]
    pub kind:       String,
    pub id:         i16,
    pub symbol:     String,
    #[serde(default)]
    pub name:       Option<String>,
    #[serde(default)]
    pub attributes: u8,
    pub payload:    Payload
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
pub enum Payload
{
    Pstring
    {
        text: String
    },
    Strings
    {
        strings: Vec<String>
    },
    Menu
    {
        title: String, items: Vec<MenuItem>
    },
    Hex
    {
        hex: String
    },
    File
    {
        path: PathBuf
    },
    NativeCode
    {
        path: PathBuf, load_base: String, load_limit: String
    }
}

pub fn fourcc(text: &str) -> Result<[u8; 4]>
{
    text::encode(text)?.try_into().map_err(|_| {
                                      error("a resource type or creator must contain exactly four \
                                             Mac Roman bytes")
                                  })
}

fn address(value: &str) -> Result<u64>
{
    Ok(u64::from_str_radix(value.strip_prefix("0x")
                                .ok_or_else(|| error("addresses must start with 0x"))?,
                           16)?)
}

/// Cargo resolves both explicit versions and `version.workspace = true`.
/// A build script already has the exact resolved version in its environment;
/// packaging outside Cargo uses metadata for the same resolution rules.
fn package_version(resource_path: &Path) -> Result<String>
{
    let package = resource_path.parent().ok_or_else(|| error("missing package directory"))?;
    let cargo_manifest = package.join("Cargo.toml").canonicalize()?;
    if std::env::var_os("OUT_DIR").is_some()
       && std::env::var_os("CARGO_MANIFEST_DIR").is_some_and(|dir| {
                                                    Path::new(&dir).join("Cargo.toml")
                                                                   .canonicalize()
                                                                   .ok()
                                                                   .as_ref()
                                                    == Some(&cargo_manifest)
                                                })
    {
        return Ok(std::env::var("CARGO_PKG_VERSION")?);
    }
    let output = std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| {
                                                                         "cargo".into()
                                                                     })).args(["metadata",
                                                                               "--offline",
                                                                               "--locked",
                                                                               "--no-deps",
                                                                               "--format-version",
                                                                               "1",
                                                                               "--manifest-path"])
                                                                        .arg(&cargo_manifest)
                                                                        .output()?;
    if !output.status.success()
    {
        return Err(error(String::from_utf8_lossy(&output.stderr)));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    metadata["packages"].as_array()
                        .and_then(|packages| {
                            packages.iter().find(|p| {
                                               p["manifest_path"].as_str()
                                                                 .is_some_and(|p| {
                                                                     Path::new(p) == cargo_manifest
                                                                 })
                                           })
                        })
                        .and_then(|p| p["version"].as_str())
                        .map(String::from)
                        .ok_or_else(|| {
                            error("Cargo metadata did not return the application version")
                        })
}

/// Classic NumVersion has decimal BCD digits and a bounded prerelease ordinal.
/// Reject unrepresentable numeric versions rather than emitting misleading bytes.
fn version_header(version: &str) -> Result<[u8; 6]>
{
    let core = version.split('+').next().unwrap_or(version);
    let (numbers, pre) = core.split_once('-').unwrap_or((core, ""));
    let numbers =
        numbers.split('.').map(str::parse::<u8>).collect::<std::result::Result<Vec<_>, _>>()?;
    if numbers.len() != 3 || numbers[0] > 99 || numbers[1] > 9 || numbers[2] > 9
    {
        return Err(error("vers requires a two-digit BCD major and single-digit minor/patch"));
    }
    let (stage, ordinal) = if pre.is_empty()
    {
        (0x80, 0)
    }
    else
    {
        let (label, ordinal) = pre.split_once('.').unwrap_or((pre, "0"));
        let stage = match label
        {
            "dev" => 0x20,
            "alpha" => 0x40,
            "beta" => 0x60,
            "rc" => 0x80,
            _ =>
            {
                return Err(error("vers prerelease must be dev, alpha, beta or rc with an \
                                  optional u8 ordinal"));
            }
        };
        let ordinal = ordinal.parse::<u8>()?;
        if label == "rc" && ordinal == 0
        {
            return Err(error("vers release candidates require a nonzero ordinal"));
        }
        (stage, ordinal)
    };
    text::pstring(version)?;
    Ok([((numbers[0] / 10) << 4) | (numbers[0] % 10),
        numbers[1] << 4 | numbers[2],
        stage,
        ordinal,
        0,
        0])
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum MenuItem
{
    Text(String),
    Definition(MenuItemDefinition)
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MenuItemDefinition
{
    pub text:    String,
    #[serde(default)]
    pub key:     Option<String>,
    #[serde(default)]
    pub mark:    u8,
    #[serde(default)]
    pub style:   u8,
    #[serde(default = "menu_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub checked: bool,
    #[serde(default)]
    pub command: Option<String>
}
fn menu_enabled() -> bool
{
    true
}
impl MenuItem
{
    fn text(&self) -> &str
    {
        match self
        {
            Self::Text(text) => text,
            Self::Definition(item) => &item.text
        }
    }

    fn enabled(&self) -> bool
    {
        self.text() != "-"
        && match self
        {
            Self::Text(_) => true,
            Self::Definition(item) => item.enabled
        }
    }

    fn command(&self) -> Result<u32>
    {
        match self
        {
            Self::Definition(item) =>
            {
                item.command
                    .as_deref()
                    .map(fourcc)
                    .transpose()
                    .map(|v| v.map_or(0, u32::from_be_bytes))
            }
            _ => Ok(0)
        }
    }

    fn metadata(&self) -> Result<[u8; 4]>
    {
        match self
        {
            Self::Text(_) => Ok([0; 4]),
            Self::Definition(item) =>
            {
                let key = match &item.key
                {
                    None => 0,
                    Some(key) =>
                    {
                        let bytes = text::encode(key)?;
                        if bytes.len() != 1
                        {
                            return Err(error("menu key must encode one Mac Roman character"));
                        }
                        bytes[0]
                    }
                };
                Ok([0, key, if item.checked { 0x12 } else { item.mark }, item.style])
            }
        }
    }
}

impl Manifest
{
    pub fn read(path: &Path) -> Result<Self>
    {
        let mut manifest: Self = serde_json::from_slice(&fs::read(path)?)?;
        manifest.cargo_version = package_version(path)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()>
    {
        if self.binary_name.as_deref().is_some_and(|name| {
                                          name.is_empty()
                                          || !name.bytes().all(|b| {
                                                              b.is_ascii_alphanumeric()
                                                              || b == b'_'
                                                              || b == b'-'
                                                          })
                                      })
        {
            return Err(error("invalid Cargo binary name"));
        }
        let name = text::encode(&self.name)?;
        if name.is_empty()
           || name.len() > 31
           || name.contains(&b':')
           || name.contains(&0)
           || self.name.contains('/')
           || self.name.contains('\\')
        {
            return Err(error("invalid HFS application filename"));
        }
        fourcc(&self.file_type)?;
        fourcc(&self.creator)?;
        self.descriptor()?.encode()?;
        version_header(&self.cargo_version)?;
        let mut keys = BTreeSet::from([(*b"RV64", 0), (*b"NABI", 0), (*b"vers", 1)]);
        let mut symbols = BTreeSet::from(["NATIVE_CODE", "NATIVE_ABI", "VERSION"]);
        for definition in &self.resources
        {
            let kind = fourcc(&definition.kind)?;
            if !keys.insert((kind, definition.id))
            {
                return Err(error("duplicate or reserved resource type and ID"));
            }
            let symbol = &definition.symbol;
            if symbol.is_empty()
               || !symbol.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
               || symbol.as_bytes()[0].is_ascii_digit()
               || !symbols.insert(symbol)
            {
                return Err(error("resource symbols must be unique uppercase Rust identifiers"));
            }
            if let Some(name) = &definition.name
            {
                text::pstring(name)?;
            }
            if definition.attributes & 1 != 0
            {
                return Err(error("compressed resource payloads are not supported"));
            }
        }
        Ok(())
    }

    pub fn binary_name(&self) -> &str
    {
        self.binary_name.as_deref().unwrap_or(&self.name)
    }

    pub fn descriptor(&self) -> Result<Descriptor>
    {
        let role = match self.role.as_str()
        {
            "system" => Role::System,
            "application" => Role::Application,
            "code_resource" => Role::CodeResource,
            _ => return Err(error("role must be system, application or code_resource"))
        };
        let d = Descriptor { role,
                             abi_version: self.abi_version,
                             load_base: address(&self.load_base)?,
                             load_limit: address(&self.load_limit)? };
        if d.load_base & 4095 != 0 || d.load_limit & 4095 != 0
        {
            return Err(error("load reservations must be page aligned"));
        }
        d.encode()?;
        Ok(d)
    }

    pub fn rust_ids(&self) -> String
    {
        let mut output = String::from("// Generated from resources.json; do not edit.\n");
        for (symbol, kind, id) in [
            ("NATIVE_CODE", "RV64", 0),
            ("NATIVE_ABI", "NABI", 0),
            ("VERSION", "vers", 1),
        ]
        .into_iter()
        .chain(
            self.resources
                .iter()
                .map(|r| (r.symbol.as_str(), r.kind.as_str(), r.id)),
        ) {
            let kind = fourcc(kind).expect("validated manifest");
            output.push_str(&format!("#[allow(dead_code)]\npub const {symbol}: mac_abi::ResourceKey = mac_abi::ResourceKey {{ kind: {kind:?}, id: {id} }};\n"));
        }
        output
    }

    pub fn source_files(&self, path: &Path) -> Vec<PathBuf>
    {
        self.resources
            .iter()
            .filter_map(|r| {
                match &r.payload
                {
                    Payload::File { path: source }
                    | Payload::NativeCode { path: source,
                                          .. } => Some(path.parent().unwrap().join(source)),
                    _ => None
                }
            })
            .collect()
    }

    pub fn compile_resources(&self, path: &Path, elf: &[u8]) -> Result<Vec<Resource>>
    {
        self.validate()?;
        let mut resources = vec![Resource { kind:       *b"RV64",
                                            id:         0,
                                            name:       Some(b"Native executable".to_vec()),
                                            attributes: 0,
                                            data:       elf.to_vec() },
                                 Resource { kind:       *b"NABI",
                                            id:         0,
                                            name:       Some(b"Native ABI".to_vec()),
                                            attributes: 0,
                                            data:       self.descriptor()?.encode()?.to_vec() },];
        let mut version = version_header(&self.cargo_version)?.to_vec();
        version.extend(text::pstring(&self.cargo_version)?);
        version.extend(text::pstring(&self.description)?);
        resources.push(Resource { kind:       *b"vers",
                                  id:         1,
                                  name:       None,
                                  attributes: 0,
                                  data:       version });
        for definition in &self.resources
        {
            let data = match &definition.payload
            {
                Payload::Pstring { text: value } =>
                {
                    if definition.kind != "STR "
                    {
                        return Err(error("pstring payload requires resource type STR (with \
                                          trailing space)"));
                    }
                    text::pstring(value)?
                }
                Payload::Strings { strings } =>
                {
                    if definition.kind != "STR#"
                    {
                        return Err(error("strings payload requires STR#"));
                    }
                    let mut out = u16::try_from(strings.len())?.to_be_bytes().to_vec();
                    for value in strings
                    {
                        out.extend(text::pstring(value)?);
                    }
                    out
                }
                Payload::Menu { title,
                                items } =>
                {
                    if definition.kind != "MENU"
                       || items.len() > 31
                       || items.iter().any(|item| item.text().is_empty())
                    {
                        return Err(error("invalid MENU definition"));
                    }
                    let mut out = definition.id.to_be_bytes().to_vec();
                    out.extend_from_slice(&[0; 6]); // width, height, procedure ID
                    let enable_bits = items.iter().enumerate().fold(1u32, |bits, (index, item)| {
                                                                  bits
                                                                  | if item.enabled()
                                                                  {
                                                                      1u32 << (index + 1)
                                                                  }
                                                                  else
                                                                  {
                                                                      0
                                                                  }
                                                              });
                    out.extend_from_slice(&enable_bits.to_be_bytes());
                    out.extend(text::pstring(title)?);
                    let mut commands = Vec::new();
                    for item in items
                    {
                        out.extend(text::pstring(item.text())?);
                        out.extend_from_slice(&item.metadata()?);
                        commands.extend_from_slice(&item.command()?.to_be_bytes());
                    }
                    if commands.iter().any(|byte| *byte != 0)
                    {
                        if self.resources.iter().any(|r| r.kind == "MCMD" && r.id == definition.id)
                        {
                            return Err(error("menu command fields conflict with explicit MCMD \
                                              resource"));
                        }
                        resources.push(Resource { kind:       *b"MCMD",
                                                  id:         definition.id,
                                                  name:       None,
                                                  attributes: 0,
                                                  data:       commands });
                    }
                    out.push(0);
                    out
                }
                Payload::Hex { hex } =>
                {
                    let hex: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
                    if !hex.is_ascii() || !hex.len().is_multiple_of(2)
                    {
                        return Err(error("hex payload must contain pairs of ASCII hex digits"));
                    }
                    (0..hex.len()).step_by(2)
                                  .map(|i| Ok(u8::from_str_radix(&hex[i..i + 2], 16)?))
                                  .collect::<Result<Vec<u8>>>()?
                }
                Payload::File { path: source } => fs::read(path.parent().unwrap().join(source))?,
                Payload::NativeCode { path: source,
                                      load_base,
                                      load_limit } =>
                {
                    let source = path.parent().unwrap().join(source);
                    let stripped = source.with_extension("runtime.elf");
                    crate::package::strip_runtime(&source, &stripped)?;
                    let code = fs::read(stripped)?;
                    let descriptor = Descriptor { role:        Role::CodeResource,
                                                  abi_version: self.abi_version,
                                                  load_base:   address(load_base)?,
                                                  load_limit:  address(load_limit)? };
                    mac_formats::elf::decode(&code, descriptor.load_base, descriptor.load_limit)?;
                    let mut payload = descriptor.encode()?.to_vec();
                    payload.extend(code);
                    payload
                }
            };
            resources.push(Resource { kind: fourcc(&definition.kind)?,
                                      id: definition.id,
                                      name: definition.name
                                                      .as_deref()
                                                      .map(text::encode)
                                                      .transpose()?,
                                      attributes: definition.attributes,
                                      data });
        }
        Ok(resources)
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    fn manifest() -> Manifest
    {
        Manifest::read(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../finder/resources.json")).unwrap()
    }

    #[test]
    fn compiles_real_menu_strings_and_version()
    {
        let m = manifest();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../finder/resources.json");
        let resources = m.compile_resources(&path, b"ELF").unwrap();
        let menu = &resources.iter().find(|r| r.kind == *b"MENU" && r.id == 1252).unwrap().data;
        assert_eq!(&menu[..2], &1252i16.to_be_bytes());
        assert_eq!(&menu[12..17], b"\x04File");
        assert_eq!(&menu[17..32], b"\x0aNew Folder\0N\0\0");
        let commands = &resources.iter().find(|r| r.kind == *b"MCMD" && r.id == 1252).unwrap().data;
        assert_eq!(&commands[..8], b"new sope");
        let vers = &resources.iter().find(|r| r.kind == *b"vers").unwrap().data;
        assert_eq!(&vers[..7], &[0, 0x10, 0x80, 0, 0, 0, 5]);
        assert!(m.rust_ids().contains("FILE_MENU"));
        let mut m = manifest();
        m.cargo_version = "12.3.4".into();
        let resources = m.compile_resources(&path, b"ELF").unwrap();
        let vers = &resources.iter().find(|r| r.kind == *b"vers").unwrap().data;
        assert_eq!(&vers[..2], &[0x12, 0x34]);
    }

    #[test]
    fn cargo_versions_encode_stages_and_reject_unrepresentable_versions()
    {
        assert_eq!(version_header("12.3.4").unwrap(), [0x12, 0x34, 0x80, 0, 0, 0]);
        for (version, stage, ordinal) in [("1.2.3-dev.7", 0x20, 7),
                                          ("1.2.3-alpha.2", 0x40, 2),
                                          ("1.2.3-beta.255", 0x60, 255),
                                          ("1.2.3-rc.1", 0x80, 1)]
        {
            assert_eq!(&version_header(version).unwrap()[2..4], &[stage, ordinal]);
        }
        for version in
            ["100.0.0", "1.10.0", "1.2.10", "1.2.3-beta.256", "1.2.3-preview.1", "1.2.3-rc.0"]
        {
            assert!(version_header(version).is_err(), "{version}");
        }
        let mut manifest = manifest();
        manifest.cargo_version = "12.3.4-beta.2+build.2026".into();
        let resources = manifest.compile_resources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../finder/resources.json"), b"ELF").unwrap();
        let vers = &resources.iter().find(|r| r.kind == *b"vers").unwrap().data;
        assert_eq!(&vers[..6], &[0x12, 0x34, 0x60, 2, 0, 0]);
        assert_eq!(&vers[7..7 + vers[6] as usize], b"12.3.4-beta.2+build.2026");
    }
    #[test]
    fn cargo_resolves_inherited_and_explicit_package_versions()
    {
        let dir = std::env::temp_dir().join(format!("system-risc-vers-{}", std::process::id()));
        let package = dir.join("app");
        fs::create_dir_all(package.join("src")).unwrap();
        fs::write(
                  dir.join("Cargo.toml"),
                  r#"[workspace]
members=["app"]
resolver="3"
[workspace.package]
version="12.3.4-beta.2"
"#
        ).unwrap();
        fs::write(package.join("src/lib.rs"), "").unwrap();
        fs::write(package.join("Cargo.toml"),"[package]\nname=\"vers-fixture\"\nversion.workspace=true\nedition=\"2024\"\n").unwrap();
        assert_eq!(package_version(&package.join("resources.json")).unwrap(), "12.3.4-beta.2");
        fs::write(package.join("Cargo.toml"),"[package]\nname=\"vers-fixture\"\nversion=\"2.1.0\"\nedition=\"2024\"\n").unwrap();
        assert_eq!(package_version(&package.join("resources.json")).unwrap(), "2.1.0");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_reserved_keys_bad_symbols_and_wrong_abi()
    {
        let mut m = manifest();
        m.resources[0].symbol = "not-valid".into();
        assert!(m.validate().is_err());
        let mut m = manifest();
        m.resources[0].kind = "RV64".into();
        m.resources[0].id = 0;
        assert!(m.validate().is_err());
        let mut m = manifest();
        m.abi_version = mac_formats::native::ABI_VERSION + 1;
        assert!(m.validate().is_err());
        let mut m = manifest();
        m.resources[0].kind = "MENU".into();
        assert!(m.compile_resources(Path::new("x"), b"").is_err());
    }
}
