//! Current native platform selection. The Rust target selects the LP64 calling
//! convention; the complete LLVM profile selects instructions, including V/B.
use std::{collections::BTreeSet, env, process::Command, sync::LazyLock};

use serde::Deserialize;

use crate::{Result, error};

pub mod memory;
mod devices;
pub mod virtual_memory;
pub mod scheduling;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Platform
{
    pub name:           String,
    pub rust_target:    String,
    pub llvm_profile:   String,
    pub qemu_cpu:       String,
    pub qemu_ram:       String,
    pub qemu_harts:     core::num::NonZeroUsize,
    pub fatal_shutdown: FatalShutdown,
    pub mmio_devices:   Vec<devices::Definition>,
    scheduling:         scheduling::Definition,
    memory:             memory::Definition,
    virtual_memory:     virtual_memory::Definition
}
pub const DEFAULT_PLATFORM: &str = "QemuRISC-V";
pub const PLATFORM_ENV: &str = "SYSTEM7_PLATFORM";
pub fn load(name: &str) -> Result<Platform>
{
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(error("platform name must contain only ASCII letters, digits, - or _"));
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../platforms")
                                                               .join(format!("{name}.json"));
    let bytes = std::fs::read(&path).map_err(|cause| {
                                        error(format!("cannot select platform '{name}': {cause}; \
                                                       known platform: {DEFAULT_PLATFORM}"))
                                    })?;
    let platform: Platform =
        serde_json::from_slice(&bytes).map_err(|cause| {
                                          error(format!("invalid platform '{name}' at {}: {cause}",
                                                        path.display()))
                                      })?;
    if platform.name != name
    {
        return Err(error(format!("platform '{name}' declares a different name '{}'",
                                 platform.name)));
    }
    platform.device_artifacts()?;
    platform.virtual_memory()?;
    platform.scheduling()?;
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
    flags.push_str(&format!("-C\u{1f}target-feature=+{}\u{1f}-C\u{1f}relocation-model=pic",
                            platform.llvm_profile));
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
pub fn guest_artifacts() -> Result<(String, String)>
{
    let (rust, mut linker) = current().device_artifacts()?;
    linker.push_str(&current().memory()?.linker_symbols());
    linker.push_str(&current().virtual_memory()?.linker_symbols());
    Ok((rust, linker))
}
impl Platform
{
    pub fn scheduling(&self) -> Result<scheduling::Checked>
    {
        self.scheduling.check()
    }

    fn device_artifacts(&self) -> Result<(String, String)>
    {
        let devices = devices::check(&self.mmio_devices, &self.memory()?)?;
        let (mut rust, linker) = artifacts_for(&self.fatal_shutdown, &devices)?;
        rust.push_str(&devices::constants(&devices));
        Ok((rust, linker))
    }

    pub fn virtual_memory(&self) -> Result<virtual_memory::Checked>
    {
        self.virtual_memory.check(&self.rust_target, &self.memory()?)
    }

    pub fn memory(&self) -> Result<memory::Checked>
    {
        self.memory.check()
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
pub fn memory_artifacts() -> Result<(String, String)>
{
    let checked = current().memory()?;
    Ok((checked.system_constants(), checked.boot_constants()))
}
fn artifacts_for(device: &FatalShutdown, devices: &[devices::Checked]) -> Result<(String, String)>
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
    let rust = format!("// Generated from the validated platform definition.\npub const \
                        FATAL_FINISHER_ADDRESS: u64 = {address};\npub const \
                        FATAL_FINISHER_FAILURE: u32 = {};\npub const FATAL_FINISHER_RESTART: u32 \
                        = {};\n",
                       device.failure_value, device.restart_value);
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
        let (rust, linker) = platform.device_artifacts().unwrap();
        let address =
            devices::check(&platform.mmio_devices, &platform.memory().unwrap()).unwrap()[0].address;
        assert!(rust.contains(&format!("FATAL_FINISHER_ADDRESS: u64 = {address}")));
        assert!(linker.contains(&format!("__platform_fatal_finisher = 0x{address:x}")));
        assert!(rust.contains(&format!("FATAL_FINISHER_FAILURE: u32 = {}",
                                       platform.fatal_shutdown.failure_value)));
        assert!(linker.contains(&format!("__platform_fatal_failure = 0x{:x}",
                                         platform.fatal_shutdown.failure_value)));
        assert_eq!(platform.fatal_shutdown.restart_value, 0x7777);
        assert!(rust.contains("FATAL_FINISHER_RESTART: u32 = 30583;"));
        assert!(linker.contains("__platform_fatal_restart = 0x7777"));
        for name in ["../QemuRISC-V", "QemuRISC-V/other", ""]
        {
            assert!(load(name).err().unwrap().to_string().contains("platform name must contain"));
        }
        let error = load("NoSuchBoard").err().unwrap().to_string();
        assert!(error.contains("cannot select platform 'NoSuchBoard'")
                && error.contains("known platform: QemuRISC-V"));
        for text in ["0x0", "0x3", "0xffffffffffffffff", "0xgarbage", "1234", "0x80000000"]
        {
            let mut bad = load("QemuRISC-V").unwrap();
            bad.mmio_devices[0].address = text.into();
            assert!(bad.device_artifacts().err().unwrap().to_string().contains("platform MMIO"));
        }
        let mut bad = load("QemuRISC-V").unwrap();
        bad.fatal_shutdown.failure_value = 0;
        assert!(bad.device_artifacts().is_err());
        bad.fatal_shutdown.failure_value = 1;
        bad.fatal_shutdown.device = "absent".into();
        assert!(bad.device_artifacts().err().unwrap().to_string().contains("absent"));
        let mut bad = load("QemuRISC-V").unwrap();
        bad.mmio_devices.push(bad.mmio_devices[0].clone());
        assert!(bad.device_artifacts().err().unwrap().to_string().contains("non-overlapping"));
    }
    #[test]
    fn emergency_restart_is_required_checked_and_generated_from_the_selected_record()
    {
        for restart in [0, 78643]
        {
            let mut platform = load("QemuRISC-V").unwrap();
            platform.fatal_shutdown.restart_value = restart;
            let error = platform.device_artifacts().unwrap_err().to_string();
            assert!(error.contains("fatal_shutdown.restart_value"), "{error}");
            assert!(error.contains("nonzero and distinct from failure_value"), "{error}");
            assert!(error.contains("board's documented reset command"), "{error}");
        }
        let mut platform = load("QemuRISC-V").unwrap();
        platform.fatal_shutdown.restart_value = 0x1234;
        let (rust, linker) = platform.device_artifacts().unwrap();
        assert!(rust.contains("FATAL_FINISHER_RESTART: u32 = 4660;"));
        assert!(linker.contains("__platform_fatal_restart = 0x1234"));
        let error = serde_json::from_str::<FatalShutdown>(r#"{"device":"sifive-test","failure_value":78643}"#)
            .err().unwrap().to_string();
        assert!(error.contains("missing field `restart_value`"), "{error}");
    }
}
