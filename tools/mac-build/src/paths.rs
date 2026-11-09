//! Host build-output selection shared by packaging and acceptance readers.
use std::{env,
          path::{Component, Path, PathBuf}};

pub const BUILD_DIR_ENV: &str = "SR_BUILD_DIR";
/// Explicit final target/profile directory when Cargo intermediates live elsewhere.
pub const ARTIFACT_DIR_ENV: &str = "SR_CARGO_ARTIFACT_DIR";

/// Directories a walk of the checkout must never descend into. Output lives
/// inside the tree, so a walk that ignores this list can package its own
/// output on the next build.
pub const OUTPUT_DIRECTORIES: [&str; 4] = ["build", "dist", "results", "target"];

/// Whether a directory name is a build output directory.
pub fn is_output_directory(name: &str) -> bool
{
    OUTPUT_DIRECTORIES.contains(&name)
}

/// Relative environment overrides are relative to the outer workspace, so
/// changing a child process's working directory never changes its input media.
pub fn build_dir(workspace: &Path) -> PathBuf
{
    workspace.join(env::var_os(BUILD_DIR_ENV).map(PathBuf::from).unwrap_or_else(|| "build".into()))
}

/// Whether a record names a path inside the checkout rather than anywhere on
/// the host. A record field is read on whichever host is building, so the test
/// is lexical and component-based, not a substring test: `version..ld` is a
/// legitimate file name, and a Windows drive-relative `C:entry.S` is not a
/// repository path even though it has no leading separator.
pub fn is_repo_relative(text: &str) -> bool
{
    if text.trim().is_empty()
    {
        return false;
    }
    if let Some((first, _)) = text.split_at_checked(1)
    {
        if first == "/" || first == "\\"
        {
            return false;
        }
    }
    // A drive letter is a prefix on one host and an ordinary character on
    // another, so it is recognised from the text rather than from `Path`.
    if text.as_bytes().get(1) == Some(&b':')
       && text.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
    {
        return false;
    }
    Path::new(text).components().all(|component| matches!(component, Component::Normal(_)))
}

/// `fs::canonicalize` returns a verbatim (`\\?\`) path on Windows. The MSVC
/// linker mangles a verbatim input that contains a space, so a Cargo target
/// directory below a spaced path cannot link host proc macros; a verbatim path
/// also never compares equal to a plain source root.
pub fn canonical(path: &Path) -> crate::Result<PathBuf>
{
    Ok(plain(path.canonicalize()?))
}

#[cfg(windows)]
fn plain(path: PathBuf) -> PathBuf
{
    let Some(text) = path.to_str()
    else
    {
        return path;
    };
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\")
    {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\")
    {
        Some(rest) => PathBuf::from(rest),
        None => path
    }
}

#[cfg(not(windows))]
fn plain(path: PathBuf) -> PathBuf
{
    path
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


