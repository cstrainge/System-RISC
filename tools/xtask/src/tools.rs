//! Host tool resolution: one place that answers "where is this program on this
//! host", so `build`, `run` and `flash` fail the same way when something is
//! missing.
//!
//! Platform records name programs without a host extension (`qemu-system-riscv64`).
//! This module owns the host's spelling of that name and the search order:
//!
//! 1. an explicit environment override, so a developer can point at a build they made themselves;
//! 2. this build's own tool cache, which `cargo xtask tools` fills;
//! 3. `PATH`;
//! 4. the locations each host's installer uses.
//!
//! A miss is an error naming the exact command that installs the tool on this
//! host. Nothing here assumes a POSIX shell, and nothing writes to a fixed
//! absolute path.
use std::{env,
          ffi::OsString,
          path::{Path, PathBuf}};

use mac_build::{Result, error};

/// The program's name on this host, with the host's extension.
pub fn host_name(program: &str) -> OsString
{
    if cfg!(windows) { OsString::from(format!("{program}.exe")) } else { OsString::from(program) }
}

/// The environment variable that overrides this program's location.
fn override_name(program: &str) -> String
{
    let mut name = String::from("SYSTEM7_TOOL_");
    for character in program.chars()
    {
        if character.is_ascii_alphanumeric()
        {
            name.push(character.to_ascii_uppercase());
        }
        else
        {
            name.push('_');
        }
    }
    name
}

/// A per-user cache for tools this build installs itself.
pub fn cache_dir() -> PathBuf
{
    let base = if cfg!(windows)
    {
        env::var_os("LOCALAPPDATA").map(PathBuf::from)
                                   .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
    }
    else
    {
        env::var_os("XDG_CACHE_HOME").map(PathBuf::from).or_else(|| {
                                                            env::var_os("HOME").map(|home| {
                                                                PathBuf::from(home).join(".cache")
                                                            })
                                                        })
    };
    base.unwrap_or_else(env::temp_dir).join("system7").join("tools")
}

/// Where this host's package manager or installer puts the program.
fn installed(program: &str) -> Vec<PathBuf>
{
    let mut places = Vec::new();
    if cfg!(windows)
    {
        let mut roots = Vec::new();
        for variable in ["ProgramFiles", "ProgramW6432"]
        {
            if let Some(root) = env::var_os(variable)
            {
                roots.push(PathBuf::from(root));
            }
        }
        for root in roots
        {
            places.push(root.join("qemu").join(host_name(program)));
        }
    }
    else
    {
        places.push(Path::new("/usr/bin").join(host_name(program)));
        places.push(Path::new("/usr/local/bin").join(host_name(program)));
    }
    places
}

/// Resolve a program, or report how to install it.
pub fn resolve(program: &str) -> Result<PathBuf>
{
    if program.trim().is_empty()
    {
        return Err(error("a platform record names an empty program"));
    }
    let name = host_name(program);
    let mut searched = Vec::new();
    if let Some(override_path) = env::var_os(override_name(program)).or_else(|| {
                                     if program.starts_with("qemu-")
                                     {
                                         env::var_os("SYSTEM7_EMULATOR")
                                     }
                                     else
                                     {
                                         None
                                     }
                                 })
    {
        let path = PathBuf::from(override_path);
        if path.is_file()
        {
            return Ok(path);
        }
        searched.push(format!("{}={}", override_name(program), path.display()));
    }
    let cached = cache_dir().join(&name);
    if cached.is_file()
    {
        return Ok(cached);
    }
    searched.push(cached.display().to_string());
    if let Some(found) = on_path(&name)
    {
        return Ok(found);
    }
    searched.push("PATH".into());
    for place in installed(program)
    {
        if place.is_file()
        {
            return Ok(place);
        }
        searched.push(place.display().to_string());
    }
    Err(error(format!("cannot find '{program}' on this host; looked in {}. {}",
                      searched.join(", "),
                      hint(program))))
}

fn on_path(name: &OsString) -> Option<PathBuf>
{
    let path = env::var_os("PATH")?;
    env::split_paths(&path).map(|directory| directory.join(name))
                           .find(|candidate| candidate.is_file())
}

/// The host command that installs a missing program.
pub fn hint(program: &str) -> String
{
    if cfg!(windows)
    {
        if program.starts_with("qemu-")
        {
            return format!("Install it with `winget install SoftwareFreedomConservancy.QEMU`, \
                            or set {} to the full path of an existing copy.",
                           override_name(program));
        }
        if program == "ninja"
        {
            return format!("Install it with `winget install Ninja-build.Ninja`, or set {} to \
                            the full path of an existing copy.",
                           override_name(program));
        }
        return format!("Install '{program}' and make sure it is on PATH, or set {} to its full \
                        path.",
                       override_name(program));
    }
    let package = match program
    {
        "qemu-system-riscv64" => Some("qemu-system-misc"),
        "qemu-system-aarch64" => Some("qemu-system-arm"),
        "qemu-system-x86_64" => Some("qemu-system-x86"),
        "ninja" => Some("ninja-build"),
        _ => None
    };
    match package
    {
        Some(package) =>
        {
            format!("Install it with your package manager, e.g. `sudo apt install {package}`, or \
                     set {} to an existing binary.",
                    override_name(program))
        }
        None =>
        {
            format!("Install '{program}' with your package manager, or set {} to an existing \
                     binary.",
                    override_name(program))
        }
    }
}

/// One row of the `cargo xtask tools` report.
pub struct Status
{
    pub program: String,
    pub path:    Option<PathBuf>
}

impl Status
{
    pub fn found(program: &str) -> Self
    {
        Self { program: program.into(),
               path:    resolve(program).ok() }
    }

    pub fn line(&self) -> String
    {
        match &self.path
        {
            Some(path) => format!("{:<24} {}", self.program, path.display()),
            None =>
            {
                format!("{:<24} missing; {}", self.program, hint(&self.program))
            }
        }
    }
}

/// The tools the declared platforms need, for the `tools` report.
/// Report the tools this platform's emulation needs, and whether each is usable.
pub fn report(programs: &[String])
{
    if programs.is_empty()
    {
        println!("This platform declares no emulator, so it needs no host tools.");
        return;
    }
    println!("Tool cache: {}", cache_dir().display());
    for program in programs
    {
        println!("{}", Status::found(program).line());
    }
}
