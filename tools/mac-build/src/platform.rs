//! Current native platform selection. The Rust target selects the LP64 calling
//! convention; the complete LLVM profile selects instructions, including V/B.
use std::{collections::BTreeSet, env, process::Command, sync::LazyLock};

use serde::Deserialize;

use crate::{Result, error};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Platform
{
    pub rust_target:  String,
    pub llvm_profile: String,
    pub qemu_cpu:     String
}
static PLATFORM: LazyLock<Platform> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../platforms/riscv64.json")).expect("checked-in \
                                                                                  RISC-V platform \
                                                                                  configuration")
});
pub fn current() -> &'static Platform
{
    &PLATFORM
}
pub fn flags() -> String
{
    format!("-C\u{1f}target-feature=+{}", current().llvm_profile)
}
/// Guest builds launched outside the workspace still receive the same profile.
/// Existing flags remain visible; validation below rejects a narrowed or
/// broadened instruction set rather than silently ignoring caller flags.
pub fn configure(command: &mut Command)
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
    flags.push_str(&self::flags());
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
