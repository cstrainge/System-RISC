//! Current native platform selection. The Rust target selects the LP64 calling
//! convention; the complete LLVM profile selects instructions, including V/B.
use std::{collections::BTreeSet,
          env, fs,
          path::{Path, PathBuf},
          process::Command,
          sync::LazyLock};

use serde::Deserialize;
use serde_json::Value;

use crate::{Result, error};

pub mod emulation;
pub mod hardware;
pub mod memory;
mod devices;
pub mod virtual_memory;
pub mod scheduling;

/// The build manifest for one platform. Every section that describes a
/// particular architecture's physical machine is optional: a platform record
/// that only names an emulator or a board is valid, and a consumer that needs a
/// missing section fails with a message naming it rather than receiving a
/// default that was never declared.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Platform
{
    pub name:         String,
    pub rust_target:  String,
    /// The instruction set profile as a `target-feature` value. Empty means the
    /// architecture has no such profile, in which case no flag is emitted.
    #[serde(default)]
    pub llvm_profile: String,
    /// How to run this platform under an emulator. Optional and independent of
    /// the build; absent means this platform declares no emulation.
    #[serde(default)]
    pub emulation:    Option<emulation::Definition>,
    /// How to build and run this platform on a real board. Optional and
    /// independent of both the build and the emulation description.
    #[serde(default)]
    pub hardware:     Option<hardware::Definition>,
    fatal_shutdown:   Option<FatalShutdown>,
    #[serde(default)]
    mmio_devices:     Vec<devices::Definition>,
    scheduling:       Option<scheduling::Definition>,
    memory:           Option<memory::Definition>,
    virtual_memory:   Option<virtual_memory::Definition>
}
pub const DEFAULT_PLATFORM: &str = "QemuRISC-V";
pub const PLATFORM_ENV: &str = "SYSTEM7_PLATFORM";

/// The only directory a platform record is read from.
fn directory() -> PathBuf
{
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../platforms")
}

/// Platform names available on this checkout, for error messages.
fn known() -> String
{
    let Ok(entries) = fs::read_dir(directory())
    else
    {
        return "<none>".into();
    };
    let mut names: Vec<String> = entries.filter_map(|entry| {
                                            let path = entry.ok()?.path();
                                            if path.extension()? != "json"
                                            {
                                                return None;
                                            }
                                            Some(path.file_stem()?.to_str()?.to_string())
                                        })
                                        .collect();
    names.sort();
    if names.is_empty() { "<none>".into() } else { names.join(", ") }
}

/// `include` composes platform records. A base record is merged into the record
/// that includes it, depth first, before that record is read:
///
/// * scalars and plain arrays replace the included value;
/// * objects merge key by key, recursively;
/// * `{"$append": [...]}` appends to the included array instead of replacing it, and is equivalent
///   to the plain array when the key is absent.
///
/// The path is relative to the directory of the file that names it, includes
/// may nest to any depth, and a repeated or self-referential include is
/// reported as a cycle rather than recursing.
const INCLUDE: &str = "include";
const APPEND: &str = "$append";

fn document(path: &Path, stack: &mut Vec<PathBuf>) -> Result<Value>
{
    let resolved =
        path.canonicalize()
            .map_err(|cause| {
                error(format!("cannot read platform file {}: {cause}", path.display()))
            })?;
    if let Some(position) = stack.iter().position(|seen| *seen == resolved)
    {
        let mut chain: Vec<String> =
            stack[position..].iter().map(|file| file.display().to_string()).collect();
        chain.push(resolved.display().to_string());
        return Err(error(format!("platform include cycle: {}", chain.join(" -> "))));
    }
    let bytes = fs::read(&resolved)?;
    let mut value: Value = serde_json::from_slice(&bytes).map_err(|cause| {
                                                             error(format!("invalid platform \
                                                                            file {}: {cause}",
                                                                           resolved.display()))
                                                         })?;
    let Value::Object(map) = &mut value
    else
    {
        return Err(error(format!("platform file {} must be a JSON object", resolved.display())));
    };
    let Some(include) = map.remove(INCLUDE)
    else
    {
        return Ok(value);
    };
    let include = include.as_str().ok_or_else(|| {
                                       error(format!("platform include in {} must be a string \
                                                      path",
                                                     resolved.display()))
                                   })?;
    let target = resolved.parent().unwrap_or(Path::new(".")).join(include);
    stack.push(resolved.clone());
    let base = document(&target, stack).map_err(|cause| {
                                           error(format!("platform {} includes '{include}': \
                                                          {cause}",
                                                         resolved.display()))
                                       })?;
    stack.pop();
    merge(base, value)
}

fn merge(base: Value, overlay: Value) -> Result<Value>
{
    match (base, overlay)
    {
        (Value::Object(mut base), Value::Object(overlay)) =>
        {
            for (key, value) in overlay
            {
                let merged = match base.remove(&key)
                {
                    None =>
                    {
                        match value
                        {
                            Value::Object(mut object)
                                if object.len() == 1 && object.contains_key(APPEND) =>
                            {
                                object.remove(APPEND).unwrap()
                            }
                            other => other
                        }
                    }
                    Some(current) => merge(current, value)?
                };
                base.insert(key, merged);
            }
            Ok(Value::Object(base))
        }
        // A scalar or a plain array replaces what it overlays.
        (_, overlay) => Ok(overlay)
    }
}

/// The merged JSON record for a platform, with every include resolved.
///
/// This is the interpreter's view of the manifest: callers that need to read a
/// section the typed [`Platform`] does not model (or that need the record's
/// own spelling of a value) use this instead of re-reading the file, so
/// include and merge semantics stay in one place.
pub fn record(name: &str) -> Result<Value>
{
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(error("platform name must contain only ASCII letters, digits, - or _"));
    }
    let path = directory().join(format!("{name}.json"));
    if !path.is_file()
    {
        return Err(error(format!("cannot select platform '{name}': no such platform record; \
                                  known platforms: {}",
                                 known())));
    }
    document(&path, &mut Vec::new())
}

pub fn load(name: &str) -> Result<Platform>
{
    let path = directory().join(format!("{name}.json"));
    let document = record(name)?;
    let platform: Platform = serde_json::from_value(document).map_err(|cause| {
                                                                 error(format!("invalid platform \
                                                                                '{name}' at {}: \
                                                                                {cause}",
                                                                               path.display()))
                                                             })?;
    if platform.name != name
    {
        return Err(error(format!("platform '{name}' declares a different name '{}'",
                                 platform.name)));
    }
    // Validate every section the record does declare, and nothing it does not:
    // a platform without an emulator or without a physical layout still loads.
    if platform.memory.is_some()
    {
        platform.memory()?;
    }
    if platform.virtual_memory.is_some()
    {
        platform.virtual_memory()?;
    }
    if platform.scheduling.is_some()
    {
        platform.scheduling()?;
    }
    if !platform.mmio_devices.is_empty() || platform.fatal_shutdown.is_some()
    {
        platform.device_artifacts(true)?;
    }
    if let Some(emulation) = &platform.emulation
    {
        emulation.check(&platform.name)?;
    }
    if let Some(hardware) = &platform.hardware
    {
        hardware.check(&platform.name)?;
    }
    Ok(platform)
}
static PLATFORM: LazyLock<Platform> = LazyLock::new(|| {
    let name = env::var(PLATFORM_ENV).unwrap_or_else(|_| DEFAULT_PLATFORM.into());
    load(&name).expect("native platform selection failed")
});
pub fn current() -> &'static Platform
{
    &PLATFORM
}
/// Guest builds launched outside the workspace still receive the same profile.
/// Existing flags remain visible; validation below rejects a narrowed or
/// broadened instruction set rather than silently ignoring caller flags.
pub fn configure(command: &mut Command)
{
    configure_for(current(), command);
}
pub fn configure_for(platform: &Platform, command: &mut Command)
{
    let mut flags = env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_else(|_| {
                                                           env::var("RUSTFLAGS").unwrap_or_default()
                                                                                .split_whitespace()
                                                                                .collect::<Vec<_>>()
                                                                                .join("\u{1f}")
                                                       });
    if !flags.is_empty()
    {
        flags.push('\u{1f}');
    }
    if !platform.llvm_profile.is_empty()
    {
        flags.push_str(&format!("-C\u{1f}target-feature=+{}", platform.llvm_profile));
    }
    flags.push_str("\u{1f}-C\u{1f}relocation-model=pic");
    command.env(PLATFORM_ENV, &platform.name);
    command.env("CARGO_ENCODED_RUSTFLAGS", flags);
}
/// The compiler flag covers every Cargo-built target dependency, not just the
/// final executable. Refuse conflicting overrides instead of last-flag-wins.
pub fn validate_pic_flags() -> Result<()>
{
    let flags = env::var("CARGO_ENCODED_RUSTFLAGS").map_err(|_| {
                                                       error("native builds require encoded \
                                                              relocation-model=pic flags across \
                                                              all target dependencies; use cargo \
                                                              xtask image")
                                                   })?;
    let flags: Vec<_> = flags.split('\u{1f}').collect();
    let mut found = false;
    for (index, flag) in flags.iter().enumerate()
    {
        let option = if matches!(*flag, "-C" | "--codegen")
        {
            flags.get(index + 1).copied()
        }
        else
        {
            flag.strip_prefix("-C").or_else(|| flag.strip_prefix("--codegen="))
        };
        if let Some(value) = option.and_then(|option| option.strip_prefix("relocation-model="))
        {
            if value != "pic"
            {
                return Err(error(format!("native code requires relocation-model=pic for every \
                                          target dependency; remove conflicting {value:?} \
                                          override and rebuild with cargo xtask image")));
            }
            found = true;
        }
    }
    if !found
    {
        return Err(error("native code lacks relocation-model=pic across target dependencies; \
                          rebuild using cargo xtask image"));
    }
    Ok(())
}
fn features(text: &str) -> BTreeSet<&str>
{
    text.lines()
        .filter_map(|line| line.strip_prefix("target_feature=\"").and_then(|s| s.strip_suffix('"')))
        .collect()
}
/// Validate the compiler's entire profile expansion, not a hand-maintained
/// subset of extension names which could silently drift behind the profile.
pub fn validate_guest(target: &str, actual: &str) -> Result<()>
{
    let platform = current();
    if target != platform.rust_target
    {
        return Err(error(format!("native guest target must be {} with full {}; rebuild using \
                                  cargo xtask image",
                                 platform.rust_target, platform.llvm_profile)));
    }
    validate_profile(target, actual)
}

/// Application-role target supplied by the pinned private Rust SDK. Low-level
/// image roles continue to require the platform's explicit no_std target.
pub fn validate_std_application(target: &str, actual: &str) -> Result<()>
{
    if target != "riscv64-unknown-systemrisc"
    {
        return Err(error("std application requires the pinned System RISC SDK target"));
    }
    validate_profile(target, actual)
}

fn validate_profile(target: &str, actual: &str) -> Result<()>
{
    let platform = current();
    let compiler = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let expected = Command::new(compiler).args(["--print", "cfg", "--target", target, "-C"])
                                         .arg(format!("target-feature=+{}", platform.llvm_profile))
                                         .output()?;
    if !expected.status.success()
    {
        return Err(error(format!("compiler cannot expand the required {} profile: {}",
                                 platform.llvm_profile,
                                 String::from_utf8_lossy(&expected.stderr))));
    }
    let text = String::from_utf8(expected.stdout)?;
    let expected = features(&text);
    if ![platform.llvm_profile.as_str(),"v","d","f","zba","zbb","zbs"].iter().all(|name|expected.contains(name)) {return Err(error(format!("compiler does not implement the full {} profile (V/F/D/B required); install a compatible Rust/LLVM toolchain",platform.llvm_profile)));}
    let actual: BTreeSet<_> = actual.split(',').filter(|name| !name.is_empty()).collect();
    let missing: Vec<_> = expected.difference(&actual).copied().collect();
    let extra: Vec<_> = actual.difference(&expected).copied().collect();
    if !missing.is_empty() || !extra.is_empty()
    {
        return Err(error(format!("native instruction profile differs from {}: missing \
                                  {missing:?}; unexpected {extra:?}; remove ISA overrides and \
                                  rebuild with cargo xtask image",
                                 platform.llvm_profile)));
    }
    Ok(())
}
#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn full_compiler_profile_is_required_and_refusals_explain_recovery()
    {
        let p = current();
        let output = Command::new("rustc").args(["--print", "cfg", "--target", &p.rust_target,
                                                 "-C"])
                                          .arg(format!("target-feature=+{}", p.llvm_profile))
                                          .output()
                                          .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        let set = features(&text);
        let actual = set.iter().copied().collect::<Vec<_>>().join(",");
        validate_guest(&p.rust_target, &actual).unwrap();
        let narrowed = set.iter().copied().filter(|&s| s != "v").collect::<Vec<_>>().join(",");
        let error = validate_guest(&p.rust_target, &narrowed).unwrap_err().to_string();
        assert!(error.contains("missing [\"v\"]")
                && error.contains("rebuild with cargo xtask image"));
        let error =
            validate_guest(&p.rust_target, &format!("{actual},zvl256b")).unwrap_err().to_string();
        assert!(error.contains("unexpected [\"zvl256b\"]"));
        assert!(validate_guest("riscv64gc-unknown-none-elf", &actual).unwrap_err()
                                                                     .to_string()
                                                                     .contains("native guest \
                                                                                target must be"));
    }
}


/// Board terminal failure/restart signals emitted from one checked record.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FatalShutdown
{
    device:        String,
    failure_value: u32,
    restart_value: u32
}
/// Firmware only halts on fatal failure; resident System also requests restart.
pub fn firmware_artifacts() -> Result<(String, String)>
{
    guest_artifacts_for(false)
}

pub fn guest_artifacts() -> Result<(String, String)>
{
    guest_artifacts_for(true)
}

fn guest_artifacts_for(include_restart: bool) -> Result<(String, String)>
{
    let (rust, mut linker) = current().device_artifacts(include_restart)?;
    linker.push_str(&current().memory()?.linker_symbols());
    linker.push_str(&current().virtual_memory()?.linker_symbols());
    Ok((rust, linker))
}
impl Platform
{
    pub fn scheduling(&self) -> Result<scheduling::Checked>
    {
        self.scheduling.as_ref().ok_or_else(|| missing(self, "scheduling"))?.check()
    }

    fn device_artifacts(&self, include_restart: bool) -> Result<(String, String)>
    {
        let devices = devices::check(&self.mmio_devices, &self.memory()?)?;
        let Some(fatal_shutdown) = &self.fatal_shutdown
        else
        {
            if !devices.is_empty()
            {
                return Err(error(format!("platform '{}' declares MMIO devices but no \
                                          fatal_shutdown device",
                                         self.name)));
            }
            return Ok((devices::constants(&devices), String::new()));
        };
        let (mut rust, linker) = artifacts_for(fatal_shutdown, &devices, include_restart)?;
        rust.push_str(&devices::constants(&devices));
        Ok((rust, linker))
    }

    pub fn virtual_memory(&self) -> Result<virtual_memory::Checked>
    {
        self.virtual_memory
            .as_ref()
            .ok_or_else(|| missing(self, "virtual_memory"))?
            .check(&self.rust_target, &self.memory()?)
    }

    pub fn memory(&self) -> Result<memory::Checked>
    {
        self.memory.as_ref().ok_or_else(|| missing(self, "memory"))?.check()
    }

    /// The emulation description, if this platform declares one.
    pub fn emulation(&self) -> Option<&emulation::Definition>
    {
        self.emulation.as_ref()
    }

    /// The emulation description, or an actionable refusal. A platform that
    /// only describes hardware cannot be emulated, and no emulator is invented
    /// for it.
    pub fn require_emulation(&self) -> Result<&emulation::Definition>
    {
        self.emulation.as_ref().ok_or_else(|| {
                                   let hint = match &self.hardware
                                   {
                                       Some(hardware) =>
                                       {
                                           let artifact =
                                               hardware.artifact()
                                                       .map(|artifact| {
                                                           format!("; write '{artifact}' to it \
                                                                    with {}",
                                                                   hardware.tooling().join(", "))
                                                       })
                                                       .unwrap_or_default();
                                           format!(": it targets the board '{}' and cannot be \
                                                    emulated{artifact}",
                                                   hardware.board())
                                       }
                                       None => ": it declares neither emulation nor hardware".into()
                                   };
                                   error(format!("platform '{}' declares no emulation{hint}",
                                                 self.name))
                               })
    }

    /// Build-time origins are not runtime destinations. Every independently
    /// loadable definition/driver/extension shares the same checked link range.
    pub fn link_range(&self, role: mac_formats::native::Role) -> Result<std::ops::Range<u64>>
    {
        match role
        {
            mac_formats::native::Role::System => Ok(self.virtual_memory()?.system_image_range()),
            mac_formats::native::Role::CodeResource => Ok(self.memory()?.code_link_range()),
            mac_formats::native::Role::Application =>
            {
                Err(error("application link range still comes from its manifest until private \
                           mappings are active"))
            }
        }
    }

    /// Link/execute policy differs from physical System storage. The checked
    /// virtual record derives the System extent from its physical reservation.
    pub fn validate_image(&self, role: mac_formats::native::Role, base: u64, limit: u64)
                          -> Result<()>
    {
        if role == mac_formats::native::Role::System
        {
            let expected = self.virtual_memory()?.system_image_range();
            if base != expected.start || limit != expected.end
            {
                return Err(error(format!("native System execution range {base:#x}..{limit:#x} \
                                          must match selected platform {:#x}..{:#x}; use the \
                                          platform image symbols and rebuild ROM/System \
                                          together",
                                         expected.start, expected.end)));
            }
            Ok(())
        }
        else if role == mac_formats::native::Role::CodeResource
        {
            let expected = self.link_range(role)?;
            if base != expected.start || limit != expected.end
            {
                return Err(error("code-resource link geometry differs from selected platform \
                                  policy; rebuild all modules and definitions"));
            }
            Ok(())
        }
        else
        {
            self.memory()?.validate_image_storage(role, base, limit)
        }
    }
}

fn missing(platform: &Platform, section: &str) -> Box<dyn std::error::Error>
{
    error(format!("platform '{}' declares no {section} section", platform.name))
}

pub fn memory_artifacts() -> Result<(String, String)>
{
    let checked = current().memory()?;
    Ok((checked.system_constants(), checked.boot_constants()))
}
fn artifacts_for(device: &FatalShutdown, devices: &[devices::Checked], include_restart: bool)
                 -> Result<(String, String)>
{
    let hardware = devices.iter()
                          .find(|hardware| hardware.id == device.device)
                          .ok_or_else(|| {
                              error("platform fatal-shutdown device is absent from the MMIO \
                                     inventory")
                          })?;
    let address = hardware.address;
    if hardware.bytes < 4 || device.failure_value == 0
    {
        return Err(error("platform fatal-shutdown MMIO requires a nonzero aligned u32 register \
                          and failure value"));
    }
    if device.restart_value == 0 || device.restart_value == device.failure_value
    {
        return Err(error("platform fatal_shutdown.restart_value must be nonzero and distinct \
                          from failure_value; select the board's documented reset command"));
    }
    let mut rust = format!("// Generated from the validated platform definition.\npub const \
                            FATAL_FINISHER_ADDRESS: u64 = {address};\npub const \
                            FATAL_FINISHER_FAILURE: u32 = {};\n",
                           device.failure_value);
    if include_restart
    {
        rust.push_str(&format!("pub const FATAL_FINISHER_RESTART: u32 = {};\n",
                               device.restart_value));
    }
    let linker = format!("\nPROVIDE(__platform_fatal_finisher = \
                          0x{address:x});\nPROVIDE(__platform_fatal_failure = \
                          0x{:x});\nPROVIDE(__platform_fatal_restart = 0x{:x});\n",
                         device.failure_value, device.restart_value);
    Ok((rust, linker))
}

#[cfg(test)]
mod artifact_tests
{
    use super::*;
    #[test]
    fn named_platform_generates_one_consistent_device_record_and_rejects_bad_selection()
    {
        let platform = load("QemuRISC-V").unwrap();
        let (rust, linker) = platform.device_artifacts(true).unwrap();
        let address =
            devices::check(&platform.mmio_devices, &platform.memory().unwrap()).unwrap()[0].address;
        assert!(rust.contains(&format!("FATAL_FINISHER_ADDRESS: u64 = {address}")));
        assert!(linker.contains(&format!("__platform_fatal_finisher = 0x{address:x}")));
        assert!(rust.contains(&format!("FATAL_FINISHER_FAILURE: u32 = {}",
                                       platform.fatal_shutdown.as_ref().unwrap().failure_value)));
        assert!(linker.contains(&format!("__platform_fatal_failure = 0x{:x}",
                                         platform.fatal_shutdown.as_ref().unwrap().failure_value)));
        assert_eq!(platform.fatal_shutdown.as_ref().unwrap().restart_value, 0x7777);
        assert!(rust.contains("FATAL_FINISHER_RESTART: u32 = 30583;"));
        assert!(linker.contains("__platform_fatal_restart = 0x7777"));
        for name in ["../QemuRISC-V", "QemuRISC-V/other", ""]
        {
            assert!(load(name).err().unwrap().to_string().contains("platform name must contain"));
        }
        let error = load("NoSuchBoard").err().unwrap().to_string();
        assert!(error.contains("cannot select platform 'NoSuchBoard'")
                && error.contains("known platforms: ArmVirt, QemuRISC-V, RiscVBase"));
        for text in ["0x0", "0x3", "0xffffffffffffffff", "0xgarbage", "1234", "0x80000000"]
        {
            let mut bad = load("QemuRISC-V").unwrap();
            bad.mmio_devices[0].address = text.into();
            assert!(bad.device_artifacts(true)
                       .err()
                       .unwrap()
                       .to_string()
                       .contains("platform MMIO"));
        }
        let mut bad = load("QemuRISC-V").unwrap();
        bad.fatal_shutdown.as_mut().unwrap().failure_value = 0;
        assert!(bad.device_artifacts(true).is_err());
        bad.fatal_shutdown.as_mut().unwrap().failure_value = 1;
        bad.fatal_shutdown.as_mut().unwrap().device = "absent".into();
        assert!(bad.device_artifacts(true).err().unwrap().to_string().contains("absent"));
        let mut bad = load("QemuRISC-V").unwrap();
        bad.mmio_devices.push(bad.mmio_devices[0].clone());
        assert!(bad.device_artifacts(true).err().unwrap().to_string().contains("non-overlapping"));
    }
    #[test]
    fn emergency_restart_is_required_checked_and_generated_from_the_selected_record()
    {
        for restart in [0, 78643]
        {
            let mut platform = load("QemuRISC-V").unwrap();
            platform.fatal_shutdown.as_mut().unwrap().restart_value = restart;
            let error = platform.device_artifacts(true).unwrap_err().to_string();
            assert!(error.contains("fatal_shutdown.restart_value"), "{error}");
            assert!(error.contains("nonzero and distinct from failure_value"), "{error}");
            assert!(error.contains("board's documented reset command"), "{error}");
        }
        let mut platform = load("QemuRISC-V").unwrap();
        platform.fatal_shutdown.as_mut().unwrap().restart_value = 0x1234;
        let (rust, linker) = platform.device_artifacts(true).unwrap();
        assert!(rust.contains("FATAL_FINISHER_RESTART: u32 = 4660;"));
        assert!(linker.contains("__platform_fatal_restart = 0x1234"));
        let error = serde_json::from_str::<FatalShutdown>(r#"{"device":"sifive-test","failure_value":78643}"#)
            .err().unwrap().to_string();
        assert!(error.contains("missing field `restart_value`"), "{error}");
    }
}
