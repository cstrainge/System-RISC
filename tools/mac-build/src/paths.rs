//! Host build-output selection shared by packaging and acceptance readers.
use std::{env,
          path::{Path, PathBuf}};

pub const BUILD_DIR_ENV: &str = "SYSTEM7_BUILD_DIR";
/// Explicit final target/profile directory when Cargo intermediates live elsewhere.
pub const ARTIFACT_DIR_ENV: &str = "SYSTEM7_CARGO_ARTIFACT_DIR";

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


/// Cargo normally keeps final binaries beside the profile's `build` directory.
/// A caller separating its intermediate build-dir must name the actual final
/// profile explicitly: OUT_DIR then describes the cache, not final artifacts.
pub(crate) fn artifact_profile(out: &Path) -> crate::Result<PathBuf>
{
    artifact_profile_with(out,
                          env::var_os(ARTIFACT_DIR_ENV).as_deref(),
                          env::var_os("CARGO_BUILD_BUILD_DIR").as_deref())
}
fn artifact_profile_with(out: &Path, selected: Option<&std::ffi::OsStr>,
                         cache: Option<&std::ffi::OsStr>)
                         -> crate::Result<PathBuf>
{
    if let Some(selected) = selected
    {
        let path = Path::new(selected);
        if !path.is_absolute()
        {
            return Err(crate::error(format!("{ARTIFACT_DIR_ENV} must name an absolute final \
                                             Cargo target/profile directory; got '{}'",
                                            path.display())));
        }
        if path.exists() && !path.is_dir()
        {
            return Err(crate::error(format!("{ARTIFACT_DIR_ENV} must name a directory, not \
                                             '{}'",
                                            path.display())));
        }
        return Ok(path.to_path_buf());
    }
    if cache.is_some()
    {
        return Err(crate::error(format!("with CARGO_BUILD_BUILD_DIR set, OUT_DIR cannot \
                                         establish the final artifact directory; set \
                                         {ARTIFACT_DIR_ENV} to the absolute final Cargo \
                                         target/profile directory")));
    }
    if out.file_name() == Some(std::ffi::OsStr::new("out"))
    {
        for directory in out.ancestors().skip(1)
        {
            if directory.file_name() == Some(std::ffi::OsStr::new("build"))
               && let Some(profile) = directory.parent()
               && profile.file_name().is_some()
            {
                return Ok(profile.to_path_buf());
            }
        }
    }
    Err(crate::error(format!("cannot resolve native definition artifacts from Cargo OUT_DIR \
                              '{}': expected an out directory beneath a profile/build \
                              directory; set {ARTIFACT_DIR_ENV} when Cargo's build-dir is \
                              separate from its final target directory",
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
            let actual = artifact_profile_with(Path::new(output), None, None).unwrap();
            assert_eq!(actual, profile);
        }
    }

    #[test]
    fn invalid_cargo_output_reports_the_input_instead_of_guessing_an_installation()
    {
        for output in ["/private/check/out", "/build/package/out",
                       "/private/target/release/build/package/not-out"]
        {
            let error =
                artifact_profile_with(Path::new(output), None, None).unwrap_err().to_string();
            assert!(error.contains(output), "missing offending OUT_DIR: {error}");
            assert!(error.contains("profile/build"), "missing required layout: {error}");
        }
    }
}

#[cfg(test)]
mod explicit_artifact_tests
{
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn explicit_final_profile_does_not_follow_the_shared_intermediate_cache()
    {
        let final_profile = std::env::temp_dir().join("system-risc-fixture-final/target/release");
        let intermediate =
            std::env::temp_dir().join("system-risc-shared-cache/release/build/probe/hash/out");
        assert_eq!(artifact_profile_with(&intermediate,
                                         Some(final_profile.as_os_str()),
                                         Some(OsStr::new("shared-cache"))).unwrap(),
                   final_profile);
        assert_eq!(artifact_profile_with(&intermediate, None, None).unwrap(),
                   std::env::temp_dir().join("system-risc-shared-cache/release"));
    }

    #[test]
    fn explicit_relative_or_empty_profiles_are_refused_without_guessing()
    {
        for selected in [OsStr::new(""), OsStr::new("relative/release")]
        {
            let error = artifact_profile_with(Path::new("cache/release/build/probe/hash/out"),
                                              Some(selected),
                                              None).unwrap_err()
                                                   .to_string();
            assert!(error.contains(ARTIFACT_DIR_ENV), "{error}");
            assert!(error.contains("absolute final Cargo target/profile directory"), "{error}");
        }
    }

    #[test]
    fn an_existing_file_cannot_be_the_final_profile_directory()
    {
        let error = artifact_profile_with(Path::new("cache/release/build/probe/hash/out"),
                                          Some(std::env::current_exe().unwrap().as_os_str()),
                                          None).unwrap_err()
                                               .to_string();
        assert!(error.contains(ARTIFACT_DIR_ENV), "{error}");
        assert!(error.contains("must name a directory"), "{error}");
    }
    #[test]
    fn separated_intermediate_cache_requires_an_explicit_final_artifact_location()
    {
        let error = artifact_profile_with(Path::new("shared-cache/release/build/probe/hash/out"),
                                          None,
                                          Some(OsStr::new("shared-cache"))).unwrap_err()
                                                                           .to_string();
        assert!(error.contains("CARGO_BUILD_BUILD_DIR"), "{error}");
        assert!(error.contains(ARTIFACT_DIR_ENV), "{error}");
        assert!(error.contains("absolute final Cargo target/profile directory"), "{error}");
    }
}
