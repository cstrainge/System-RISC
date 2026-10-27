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
            "set SYSTEM7_BUILD_DIR to a separate private installation, built with cargo xtask \
             image --build-dir <private-directory>, before running native acceptance");
    mac_build::paths::build_dir(source().parent().unwrap())
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
