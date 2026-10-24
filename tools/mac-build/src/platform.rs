//! Current native platform selection. The Rust target selects the LP64 calling
//! convention; the complete LLVM profile selects instructions, including V/B.
use std::{collections::BTreeSet, env, process::Command, sync::LazyLock};

use serde::Deserialize;

use crate::{Result, error};

pub mod memory;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Platform
{
    pub name:           String,
    pub rust_target:    String,
    pub llvm_profile:   String,
    pub qemu_cpu:       String,
    pub fatal_shutdown: FatalShutdown,
    memory:             memory::Definition
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
    artifacts_for(&platform.fatal_shutdown)?;
    platform.memory.check()?;
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
    flags.push_str(&format!("-C\u{1f}target-feature=+{}", platform.llvm_profile));
    command.env(PLATFORM_ENV, &platform.name);
    command.env("CARGO_ENCODED_RUSTFLAGS", flags);
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


/// Board failure signal emitted alongside the checked memory artifacts.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FatalShutdown
{
    mmio_address:  String,
    failure_value: u32
}
pub fn guest_artifacts() -> Result<(String, String)>
{
    let (rust, mut linker) = artifacts_for(&current().fatal_shutdown)?;
    linker.push_str(&current().memory()?.linker_symbols());
    Ok((rust, linker))
}
impl Platform
{
    pub fn memory(&self) -> Result<memory::Checked>
    {
        self.memory.check()
    }
}
pub fn memory_artifacts() -> Result<(String, String)>
{
    let checked = current().memory()?;
    Ok((checked.system_constants(), checked.boot_constants()))
}
fn artifacts_for(device: &FatalShutdown) -> Result<(String, String)>
{
    let address =
        device.mmio_address
              .strip_prefix("0x")
              .ok_or_else(|| {
                  error("platform fatal-shutdown MMIO address must use 0x hexadecimal")
              })?;
    let address = u64::from_str_radix(address, 16).map_err(|cause| {
                                                      error(format!("platform fatal-shutdown \
                                                                     MMIO address '{}': {cause}",
                                                                    device.mmio_address))
                                                  })?;
    if address == 0
       || address % 4 != 0
       || address.checked_add(4).is_none()
       || device.failure_value == 0
    {
        return Err(error("platform fatal-shutdown MMIO requires a nonzero aligned u32 register \
                          and failure value"));
    }
    let rust = format!("// Generated from the validated platform definition.\npub const \
                        FATAL_FINISHER_ADDRESS: u64 = {address};\npub const \
                        FATAL_FINISHER_FAILURE: u32 = {};\n",
                       device.failure_value);
    let linker = format!("\nPROVIDE(__platform_fatal_finisher = \
                          0x{address:x});\nPROVIDE(__platform_fatal_failure = 0x{:x});\n",
                         device.failure_value);
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
        let (rust, linker) = artifacts_for(&platform.fatal_shutdown).unwrap();
        let address = u64::from_str_radix(platform.fatal_shutdown
                                                  .mmio_address
                                                  .trim_start_matches("0x"),
                                          16).unwrap();
        assert!(rust.contains(&format!("FATAL_FINISHER_ADDRESS: u64 = {address}")));
        assert!(linker.contains(&format!("__platform_fatal_finisher = 0x{address:x}")));
        assert!(rust.contains(&format!("FATAL_FINISHER_FAILURE: u32 = {}",
                                       platform.fatal_shutdown.failure_value)));
        assert!(linker.contains(&format!("__platform_fatal_failure = 0x{:x}",
                                         platform.fatal_shutdown.failure_value)));
        for name in ["../QemuRISC-V", "QemuRISC-V/other", ""]
        {
            assert!(load(name).err().unwrap().to_string().contains("platform name must contain"));
        }
        let error = load("NoSuchBoard").err().unwrap().to_string();
        assert!(error.contains("cannot select platform 'NoSuchBoard'")
                && error.contains("known platform: QemuRISC-V"));
        for text in ["0x0", "0x3", "0xffffffffffffffff", "0xgarbage", "1234"]
        {
            let bad = FatalShutdown { mmio_address:  text.into(),
                                      failure_value: 1 };
            assert!(artifacts_for(&bad).err()
                                       .unwrap()
                                       .to_string()
                                       .contains("platform fatal-shutdown"));
        }
        assert!(artifacts_for(&FatalShutdown { mmio_address:  "0x100000".into(),
                                               failure_value: 0 }).is_err());
    }
}
