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


/// Definition executables are siblings of Cargo's `build` directory, under
/// the selected target/profile. The package/hash nesting is Cargo's concern;
/// neither a normal installation nor CARGO_TARGET_DIR from a caller selects it.
pub(crate) fn artifact_profile(out: &Path) -> crate::Result<&Path>
{
    if out.file_name() == Some(std::ffi::OsStr::new("out"))
    {
        for directory in out.ancestors().skip(1)
        {
            if directory.file_name() == Some(std::ffi::OsStr::new("build"))
               && let Some(profile) = directory.parent()
               && profile.file_name().is_some()
            {
                return Ok(profile);
            }
        }
    }
    Err(crate::error(format!("cannot resolve native definition artifacts from Cargo OUT_DIR \
                              '{}': expected an out directory beneath a profile/build directory",
                             out.display())))
}

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn definition_artifacts_follow_the_selected_profile_despite_package_nesting()
    {
        for (output, profile) in
            [("/private/check/target/riscv64imac-unknown-none-elf/release/build/mac-system-abc/\
               out",
              "/private/check/target/riscv64imac-unknown-none-elf/release"),
             ("/private/check/target/riscv64imac-unknown-none-elf/release/build/mac-system/abc/\
               out",
              "/private/check/target/riscv64imac-unknown-none-elf/release"),
             ("/workspace/build/project/private-target/riscv64imac-unknown-none-elf/bench/build/\
               mac-system/abc/out",
              "/workspace/build/project/private-target/riscv64imac-unknown-none-elf/bench"),
             ("target/aarch64-unknown-none/custom-profile/build/definition/abc/out",
              "target/aarch64-unknown-none/custom-profile")]
        {
            let profile = Path::new(profile);
            let actual = artifact_profile(Path::new(output)).unwrap();
            assert_eq!(actual, profile);
        }
    }

    #[test]
    fn invalid_cargo_output_reports_the_input_instead_of_guessing_an_installation()
    {
        for output in ["/private/check/out", "/build/package/out",
                       "/private/target/release/build/package/not-out"]
        {
            let error = artifact_profile(Path::new(output)).unwrap_err().to_string();
            assert!(error.contains(output), "missing offending OUT_DIR: {error}");
            assert!(error.contains("profile/build"), "missing required layout: {error}");
        }
    }
}
