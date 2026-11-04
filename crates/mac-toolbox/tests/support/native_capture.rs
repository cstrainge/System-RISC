//! Explicit private installation selection for native acceptance campaigns.
use std::{path::PathBuf, process::Command};

pub fn source() -> PathBuf
{
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

pub fn build() -> PathBuf
{
    assert!(std::env::var_os(mac_build::paths::BUILD_DIR_ENV).is_some_and(|value| {
                                                                 !value.is_empty()
                                                             }),
            "set SR_BUILD_DIR to a separate private installation, built with cargo xtask \
             build --build-dir <private-directory>, before running native acceptance");
    mac_build::paths::build_dir(&source())
}

/// The selected platform's output directory, which is where the build puts the
/// images and the bootloader.
pub fn dist() -> PathBuf
{
    if let Some(selected) = std::env::var_os("SR_DIST").filter(|value| !value.is_empty())
    {
        let path = PathBuf::from(selected);
        return if path.is_absolute() { path } else { source().join(path) };
    }
    let platform =
        std::env::var("SR_PLATFORM").unwrap_or_else(|_| "qemu_riscv".to_string());
    source().join("dist").join(platform)
}

pub fn command(script: &str) -> Command
{
    let source = source();
    let mut command =
        Command::new(std::env::var_os("SYSTEM7_PYTHON").unwrap_or_else(|| "python3".into()));
    command.arg(source.join("scripts").join(script))
           .env(mac_build::paths::BUILD_DIR_ENV, build())
           .current_dir(source);
    command
}
