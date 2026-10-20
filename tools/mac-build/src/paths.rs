//! Host build-output selection shared by packaging and acceptance readers.
use std::{env,
          path::{Path, PathBuf}};

pub const BUILD_DIR_ENV: &str = "SYSTEM7_BUILD_DIR";

/// Relative environment overrides are relative to the outer workspace, so
/// changing a child process's working directory never changes its input media.
pub fn build_dir(workspace: &Path) -> PathBuf
{
    workspace.join(env::var_os(BUILD_DIR_ENV).map(PathBuf::from)
                                             .unwrap_or_else(|| "build/system7".into()))
}

pub fn dist_dir(workspace: &Path) -> PathBuf
{
    build_dir(workspace).join("dist")
}
