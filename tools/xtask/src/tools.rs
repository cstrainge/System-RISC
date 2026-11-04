//! Host tool provisioning: one place that answers "where is this program on
//! this host, and how does this host obtain it".
//!
//! Platform records name programs without a host extension
//! (`qemu-system-riscv64`). This module owns the host's spelling of that name,
//! the search order, and — on the hosts that support it — obtaining the tool.
//!
//! The search order is the same on every host, and every step is visible in
//! `cargo xtask tools`:
//!
//! 1. `SR_TOOL_<PROGRAM>`, so a developer can point at a build they made;
//! 2. `PATH`;
//! 3. this user's tool cache, if a previous `cargo xtask tools --install` filled it;
//! 4. this host's own install locations — on Windows the `Program Files` QEMU directory, the
//!    per-user directory winget and manual installs use, and the ninja that Visual Studio ships.
//!    This is why a host with no copy on `PATH` can still resolve ninja, and skipping it was a
//!    documented gap;
//! 5. install — but only when the caller asked for installation.
//!
//! `build`, `run`, `flash` and `clean` never install anything. A missing tool is
//! reported with the same text `cargo xtask tools` prints, because obtaining a
//! tool is a deliberate act rather than a build side effect.
//!
//! The per-user cache is immutable once populated and keyed by version, so two
//! worktrees reading it concurrently read identical bytes. That is what makes
//! it safe to share without a lock: it is not shared mutable state.
//!
//! Windows obtains tools itself — `winget` first, because it verifies package
//! hashes and keeps its copies updated, then a pinned, checksum-verified
//! download. Linux and macOS are informed, never installed: the user's own
//! package manager is the authority there, and naming the wrong one is worse
//! than naming none, because it looks authoritative. Which command a host wants
//! is data in `PACKAGES` below; adding a host is a column, not a `match` in a
//! message formatter.
use std::{env,
          ffi::OsString,
          fs,
          path::{Path, PathBuf},
          process::Command};

use mac_build::{Result, error};

/// The program's name on this host, with the host's extension.
pub fn host_name(program: &str) -> OsString
{
    if cfg!(windows) { OsString::from(format!("{program}.exe")) } else { OsString::from(program) }
}

/// The environment variable that overrides this program's location.
fn override_name(program: &str) -> String
{
    let mut name = String::from("SR_TOOL_");
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

/// Which package manager family this host belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Host
{
    Windows,
    /// Debian, Ubuntu and their derivatives: `apt`.
    Debian,
    /// Fedora and RHEL: `dnf`.
    Fedora,
    /// Arch and Manjaro: `pacman`.
    Arch,
    /// openSUSE: `zypper`.
    Suse,
    MacOs,
    /// A Unix this build does not recognise. It is told to use its own package
    /// manager rather than being handed a command for a different one.
    Other
}

/// Which host this build is running on.
pub fn host() -> Host
{
    if cfg!(windows)
    {
        return Host::Windows;
    }
    if cfg!(target_os = "macos")
    {
        return Host::MacOs;
    }
    let release = fs::read_to_string("/etc/os-release").unwrap_or_default();
    let field = |key: &str| -> String {
        release.lines()
               .find_map(|line| line.strip_prefix(&format!("{key}=")))
               .map(|value| value.trim_matches('"').to_ascii_lowercase())
               .unwrap_or_default()
    };
    let mut family = format!("{} {}", field("ID"), field("ID_LIKE"));
    family = family.to_ascii_lowercase();
    let has = |word: &str| family.split_whitespace().any(|part| part == word);
    if has("debian") || has("ubuntu")
    {
        Host::Debian
    }
    else if has("fedora") || has("rhel") || has("centos")
    {
        Host::Fedora
    }
    else if has("arch") || has("manjaro")
    {
        Host::Arch
    }
    else if has("opensuse") || has("suse") || has("sles")
    {
        Host::Suse
    }
    else
    {
        Host::Other
    }
}

/// The checksum a pinned download is verified against.
pub enum Digest
{
    Sha256(&'static str),
    Sha512(&'static str)
}

/// How a pinned download becomes a usable binary.
pub enum Unpack
{
    /// A zip archive, unpacked with the host's own `tar`, which reads zip.
    Zip,
    /// A silent NSIS installer, given a destination directory.
    SilentInstaller
}

/// A pinned, checksum-verified fallback for a host with no `winget`.
pub struct Pinned
{
    pub version: &'static str,
    pub url:     &'static str,
    pub digest:  Digest,
    pub unpack:  Unpack
}

/// One program, and how each host obtains it.
///
/// Every host column is data. A host that is not a column here is told to use
/// its own package manager; it is never handed another distribution's command.
pub struct Package
{
    pub program: &'static str,
    /// The cache key and the package-manager unit this program comes from.
    pub package: &'static str,
    pub winget:  Option<&'static str>,
    pub pinned:  Option<&'static Pinned>,
    pub apt:     Option<&'static str>,
    pub dnf:     Option<&'static str>,
    pub pacman:  Option<&'static str>,
    pub zypper:  Option<&'static str>,
    pub brew:    Option<&'static str>
}

const QEMU_PINNED: Pinned = Pinned { version: "20260811",
                                     url:     "https://qemu.weilnetz.de/w64/qemu-w64-\
                                               setup-20260811.exe",
                                     digest:  Digest::Sha512("5bcf9eed634e8575a37b74f445af41a2fe41\
                                                              06da512d0c30c368301d4c105037fdfab40a5\
                                                              287367a28a957624cddebbc8c07e16c88ab66\
                                                              34f554cdf3d16bf543"),
                                     unpack:  Unpack::SilentInstaller };

const NINJA_PINNED: Pinned =
    Pinned { version: "1.13.1",
             url:
                 "https://github.com/ninja-build/ninja/releases/download/v1.13.1/ninja-win.zip",
             digest:
                 Digest::Sha256("26a40fa8595694dec2fad4911e62d29e10525d2133c9a4230b66397774ae25bf"),
             unpack:  Unpack::Zip };

/// The one table. A program missing here is still resolvable from `PATH` and
/// from `SR_TOOL_*`; it simply has no host-specific install advice.
const PACKAGES: &[Package] = &[Package { program: "qemu-system-riscv64",
                                         package: "qemu",
                                         winget:  Some("SoftwareFreedomConservancy.QEMU"),
                                         pinned:  Some(&QEMU_PINNED),
                                         apt:     Some("qemu-system-misc"),
                                         dnf:     Some("qemu-system-riscv"),
                                         pacman:  Some("qemu-system-riscv"),
                                         zypper:  Some("qemu-riscv"),
                                         brew:    Some("qemu") },
                               Package { program: "qemu-system-aarch64",
                                         package: "qemu",
                                         winget:  Some("SoftwareFreedomConservancy.QEMU"),
                                         pinned:  Some(&QEMU_PINNED),
                                         apt:     Some("qemu-system-arm"),
                                         dnf:     Some("qemu-system-aarch64"),
                                         pacman:  Some("qemu-system-aarch64"),
                                         zypper:  Some("qemu-arm"),
                                         brew:    Some("qemu") },
                               Package { program: "qemu-system-x86_64",
                                         package: "qemu",
                                         winget:  Some("SoftwareFreedomConservancy.QEMU"),
                                         pinned:  Some(&QEMU_PINNED),
                                         apt:     Some("qemu-system-x86"),
                                         dnf:     Some("qemu-system-x86"),
                                         pacman:  Some("qemu-system-x86"),
                                         zypper:  Some("qemu-x86"),
                                         brew:    Some("qemu") },
                               Package { program: "ninja",
                                         package: "ninja",
                                         winget:  Some("Ninja-build.Ninja"),
                                         pinned:  Some(&NINJA_PINNED),
                                         apt:     Some("ninja-build"),
                                         dnf:     Some("ninja-build"),
                                         pacman:  Some("ninja"),
                                         zypper:  Some("ninja"),
                                         brew:    Some("ninja") }];

fn package(program: &str) -> Option<&'static Package>
{
    PACKAGES.iter().find(|package| package.program == program)
}

/// The per-user tool cache. Nothing here is shared mutable state: a version
/// directory is written once and never rewritten.
fn cache_root() -> Option<PathBuf>
{
    if cfg!(windows)
    {
        return env::var_os("LOCALAPPDATA").map(|root| PathBuf::from(root).join("sr-tools"));
    }
    env::var_os("XDG_CACHE_HOME").map(PathBuf::from)
                                 .or_else(|| {
                                     env::var_os("HOME").map(|home| {
                                                            PathBuf::from(home).join(".cache")
                                                        })
                                 })
                                 .map(|root| root.join("sr-tools"))
}

/// Where this program's cached copy lives, if a pinned download produced one.
fn cached(program: &str) -> Option<PathBuf>
{
    let package = package(program)?;
    let pinned = package.pinned?;
    let root = cache_root()?;
    let path = root.join(package.package).join(pinned.version).join(host_name(program));
    path.is_file().then_some(path)
}

/// Where this host's installer puts the program, when it is not on `PATH`.
///
/// Each program is probed in its own locations: a missing `ninja` must not
/// report a QEMU directory among the places it looked.
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
        if program.starts_with("qemu-")
        {
            for root in &roots
            {
                places.push(root.join("qemu").join(host_name(program)));
            }
            // A machine-wide install is not the only one: winget and manual
            // archives land under the user's own profile, and a tool that is
            // invisible there looks missing while it is installed.
            for variable in ["LOCALAPPDATA", "USERPROFILE"]
            {
                if let Some(root) = env::var_os(variable)
                {
                    let root = PathBuf::from(root);
                    places.push(root.join("Programs").join("qemu").join(host_name(program)));
                    places.push(root.join("qemu").join(host_name(program)));
                }
            }
        }
        if program == "ninja"
        {
            // Visual Studio ships ninja for its CMake support under a
            // version-specific directory, so probe rather than guess.
            for root in &roots
            {
                let studio = root.join("Microsoft Visual Studio");
                for edition in subdirectories(&studio)
                {
                    for version in subdirectories(&edition)
                    {
                        places.push(version.join("Common7")
                                           .join("IDE")
                                           .join("CommonExtensions")
                                           .join("Microsoft")
                                           .join("CMake")
                                           .join("Ninja")
                                           .join(host_name(program)));
                    }
                }
            }
        }
    }
    else
    {
        places.push(Path::new("/usr/bin").join(host_name(program)));
        places.push(Path::new("/usr/local/bin").join(host_name(program)));
        if cfg!(target_os = "macos")
        {
            places.push(Path::new("/opt/homebrew/bin").join(host_name(program)));
        }
    }
    places
}

/// Why a program could not be resolved.
pub enum Missing
{
    /// The program is genuinely absent from this host.
    Absent
    {
        program: String, searched: Vec<String>
    },
    /// The request itself is malformed, so "install it" would be bad advice.
    Defect(String)
}

/// Resolve a program without ever installing one.
pub fn find(program: &str) -> std::result::Result<PathBuf, Missing>
{
    if program.trim().is_empty()
    {
        return Err(Missing::Defect("a platform record names an empty program".into()));
    }
    let name = host_name(program);
    let mut searched = Vec::new();
    // One override name per program: `SR_TOOL_<PROGRAM>` is derived from the
    // program the platform names, so every tool has exactly one spelling and a
    // record cannot be redirected by a variable no other tool honours.
    if let Some(override_path) = env::var_os(override_name(program))
    {
        let path = PathBuf::from(override_path);
        if path.is_file()
        {
            return Ok(path);
        }
        searched.push(format!("{}={}", override_name(program), path.display()));
    }
    if let Some(found) = on_path(&name)
    {
        return Ok(found);
    }
    searched.push("PATH".into());
    if let Some(found) = cached(program)
    {
        return Ok(found);
    }
    if let Some(root) = cache_root()
    {
        searched.push(root.display().to_string());
    }
    for place in installed(program)
    {
        if place.is_file()
        {
            return Ok(place);
        }
        searched.push(place.display().to_string());
    }
    Err(Missing::Absent { program: program.into(),
                          searched })
}

fn on_path(name: &OsString) -> Option<PathBuf>
{
    let path = env::var_os("PATH")?;
    env::split_paths(&path).map(|directory| directory.join(name))
                           .find(|candidate| candidate.is_file())
}

/// The child directories of a directory, empty when it does not exist.
fn subdirectories(directory: &Path) -> Vec<PathBuf>
{
    let Ok(entries) = fs::read_dir(directory)
    else
    {
        return Vec::new();
    };
    entries.filter_map(|entry| entry.ok())
           .map(|entry| entry.path())
           .filter(|path| path.is_dir())
           .collect()
}

/// Resolve a program, or report how to obtain it on this host.
pub fn resolve(program: &str) -> Result<PathBuf>
{
    match find(program)
    {
        Ok(path) => Ok(path),
        Err(Missing::Defect(why)) => Err(error(why)),
        Err(Missing::Absent { program,
                              searched }) =>
        {
            Err(error(format!("cannot find '{program}' on this host; looked in {}. {}",
                              searched.join(", "),
                              hint(&program))))
        }
    }
}

/// The host command that installs a missing program.
///
/// This is a table lookup, not a policy: the command comes from the program's
/// row for this host. A host with no row is told to use its own package
/// manager, because naming another distribution's command looks authoritative
/// and is wrong.
pub fn hint(program: &str) -> String
{
    let Some(package) = package(program)
    else
    {
        return format!("Install '{program}' and make sure it is on PATH, or set {} to its full \
                        path.",
                       override_name(program));
    };
    match command(package)
    {
        Some(command) =>
        {
            format!("Install it with `{command}`, or set {} to the full path of an existing copy.",
                    override_name(program))
        }
        None =>
        {
            format!("Install it with this host's package manager, or set {} to the full path of \
                     an existing copy.",
                    override_name(program))
        }
    }
}

/// The command that installs this program on this host, when this build knows
/// one.
fn command(package: &Package) -> Option<String>
{
    let host = host();
    let named = |manager: &str, package: &str, elevate: bool| -> String {
        if elevate
        {
            format!("sudo {manager} install {package}")
        }
        else
        {
            format!("{manager} install {package}")
        }
    };
    match host
    {
        Host::Windows =>
        {
            package.winget.map(|identifier| {
                              format!("winget install --silent --accept-package-agreements \
                                       --accept-source-agreements {identifier}")
                          })
        }
        Host::Debian => package.apt.map(|name| named("apt", name, true)),
        Host::Fedora => package.dnf.map(|name| named("dnf", name, true)),
        Host::Arch => package.pacman.map(|name| named("pacman -S", name, true)),
        Host::Suse => package.zypper.map(|name| named("zypper", name, true)),
        Host::MacOs => package.brew.map(|name| format!("brew install {name}")),
        Host::Other => None
    }
}

/// Whether this host is one where the build obtains tools itself.
pub fn can_install() -> bool
{
    matches!(host(), Host::Windows)
}

/// Obtain every program that is not already resolvable.
///
/// Only `cargo xtask tools --install` calls this. `build`, `run`, `flash` and
/// `clean` never do.
pub fn install(programs: &[String]) -> Result<()>
{
    if programs.is_empty()
    {
        println!("This platform declares no emulator, so it needs no host tools.");
        return Ok(());
    }
    if !can_install()
    {
        println!("This build does not install host tools on {}. Obtain them with your package \
                  manager:",
                 host_label());
        for program in programs
        {
            println!("  {}", hint(program));
        }
        return Ok(());
    }
    let mut failed = Vec::new();
    for program in programs
    {
        if let Ok(path) = find(program)
        {
            println!("{:<24} already available at {}", program, path.display());
            continue;
        }
        let Some(package) = package(program)
        else
        {
            failed.push(program.clone());
            continue;
        };
        match obtain(package)
        {
            Ok(()) =>
            {}
            Err(why) =>
            {
                println!("{:<24} {why}", program);
                failed.push(program.clone());
            }
        }
    }
    for program in &failed
    {
        println!("{}", hint(program));
    }
    if failed.is_empty()
    {
        Ok(())
    }
    else
    {
        Err(error(format!("{} host tool(s) unavailable", failed.len())))
    }
}

/// Obtain one package by the best path this host offers.
fn obtain(package: &Package) -> Result<()>
{
    // winget first: it verifies package hashes itself and keeps its copies
    // updated, so it is the path that needs the least trust in this program.
    if package.winget.is_some() && winget_present()
    {
        match run_winget(package)
        {
            Ok(()) if package.programs_available() => return Ok(()),
            Ok(()) =>
            {}
            Err(why) => println!("{:<24} winget did not provide it ({why})", package.program)
        }
    }
    match package.pinned
    {
        Some(pinned) =>
        {
            download(package, pinned)?;
            println!("{:<24} pinned {} into the per-user cache", package.program, pinned.version);
            Ok(())
        }
        None =>
        {
            Err(error(format!("no pinned download is recorded for '{}' on this host",
                              package.program)))
        }
    }
}

impl Package
{
    fn programs_available(&self) -> bool
    {
        find(self.program).is_ok()
    }
}

fn winget_present() -> bool
{
    on_path(&OsString::from("winget.exe")).is_some() || on_path(&OsString::from("winget")).is_some()
}

fn run_winget(package: &Package) -> Result<()>
{
    let identifier = package.winget.ok_or_else(|| error("no winget identifier"))?;
    let status = Command::new("winget").args(["install", "--silent",
                                              "--accept-package-agreements",
                                              "--accept-source-agreements", identifier])
                                       .status()?;
    if status.success() { Ok(()) } else { Err(error(format!("winget exited with {status}"))) }
}

/// Fetch a pinned download, verify it, and unpack it into the per-user cache.
///
/// The cache directory is created under a temporary name and renamed into
/// place, so a reader either sees no version directory or a complete one. That
/// is what lets two worktrees share the cache without a lock.
fn download(package: &Package, pinned: &Pinned) -> Result<()>
{
    let root = cache_root().ok_or_else(|| error("this host has no per-user cache directory"))?;
    let directory = root.join(package.package).join(pinned.version);
    if directory.join(host_name(package.program)).is_file()
    {
        return Ok(());
    }
    let staging = root.join(format!("{}-{}.partial", package.package, std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;
    let archive = staging.join("download");
    fetch(pinned.url, &archive)?;
    let actual = checksum(&archive, &pinned.digest)?;
    let expected = match pinned.digest
    {
        Digest::Sha256(text) => text.to_ascii_lowercase(),
        Digest::Sha512(text) => text.to_ascii_lowercase()
    };
    if actual != expected
    {
        let _ = fs::remove_dir_all(&staging);
        return Err(error(format!("{} failed verification: expected {expected}, got {actual}",
                                 pinned.url)));
    }
    let unpacked = staging.join("unpacked");
    fs::create_dir_all(&unpacked)?;
    match pinned.unpack
    {
        Unpack::Zip =>
        {
            // `tar` reads zip on Windows 10 and later, so unpacking needs no
            // extra dependency on the one host that reaches this path.
            let status =
                Command::new("tar").arg("-xf").arg(&archive).arg("-C").arg(&unpacked).status()?;
            if !status.success()
            {
                let _ = fs::remove_dir_all(&staging);
                return Err(error(format!("cannot unpack {}: tar exited with {status}",
                                         pinned.url)));
            }
        }
        Unpack::SilentInstaller =>
        {
            // An NSIS installer with `/S` and `/D=` writes its payload into the
            // directory it is given, which is how the cache is populated.
            let status = Command::new(&archive).arg("/S")
                                               .arg(format!("/D={}", unpacked.display()))
                                               .status()?;
            if !status.success()
            {
                let _ = fs::remove_dir_all(&staging);
                return Err(error(format!("cannot install {}: the installer exited with \
                                          {status}",
                                         pinned.url)));
            }
        }
    }
    let _ = fs::remove_file(&archive);
    if !unpacked.join(host_name(package.program)).is_file()
    {
        let _ = fs::remove_dir_all(&staging);
        return Err(error(format!("{} did not provide '{}'", pinned.url, package.program)));
    }
    fs::create_dir_all(directory.parent().unwrap_or(&root))?;
    // A version directory is written once. If another worktree won the race,
    // its copy is identical, so the existing one stands.
    if directory.exists()
    {
        let _ = fs::remove_dir_all(&staging);
        return Ok(());
    }
    fs::rename(&unpacked, &directory)?;
    let _ = fs::remove_dir_all(&staging);
    Ok(())
}

fn fetch(url: &str, into: &Path) -> Result<()>
{
    // PowerShell is present on every supported Windows and needs no extra
    // dependency; this path is only reached on Windows.
    let status = Command::new("powershell").args(["-NoProfile",
                                                  "-NonInteractive",
                                                  "-Command",
                                                  &format!("$ProgressPreference='\
                                                            SilentlyContinue'; \
                                                            Invoke-WebRequest -Uri '{}' \
                                                            -OutFile '{}' -UseBasicParsing",
                                                           url,
                                                           into.display())])
                                           .status()?;
    if status.success() { Ok(()) } else { Err(error(format!("cannot download {url}"))) }
}

fn checksum(path: &Path, digest: &Digest) -> Result<String>
{
    use sha2::{Digest as _, Sha256, Sha512};
    let bytes = fs::read(path)?;
    let hex = |bytes: &[u8]| -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() };
    Ok(match digest
    {
        Digest::Sha256(_) => hex(&Sha256::digest(&bytes)),
        Digest::Sha512(_) => hex(&Sha512::digest(&bytes))
    })
}

fn host_label() -> &'static str
{
    match host()
    {
        Host::Windows => "Windows",
        Host::Debian => "this Debian-family host",
        Host::Fedora => "this Fedora-family host",
        Host::Arch => "this Arch-family host",
        Host::Suse => "this openSUSE host",
        Host::MacOs => "macOS",
        Host::Other => "this host"
    }
}

/// One row of the `cargo xtask tools` report.
pub struct Status
{
    pub program: String,
    pub found:   Found
}

/// What resolving a program produced.
pub enum Found
{
    At(PathBuf),
    Missing,
    /// The record itself is malformed, so the report must not say "install it".
    Defect(String)
}

impl Status
{
    pub fn of(program: &str) -> Self
    {
        let found = match find(program)
        {
            Ok(path) => Found::At(path),
            Err(Missing::Absent { .. }) => Found::Missing,
            Err(Missing::Defect(why)) => Found::Defect(why)
        };
        Self { program: program.into(),
               found }
    }

    pub fn line(&self) -> String
    {
        match &self.found
        {
            Found::At(path) => format!("{:<24} {}", self.program, path.display()),
            Found::Missing => format!("{:<24} missing; {}", self.program, hint(&self.program)),
            Found::Defect(why) => format!("{:<24} invalid record: {why}", self.program)
        }
    }
}

/// Report the tools this platform's emulation needs, and whether each is
/// usable.
pub fn report(programs: &[String])
{
    if programs.is_empty()
    {
        println!("This platform declares no emulator, so it needs no host tools.");
        return;
    }
    let order = "Host tools are taken from SR_TOOL_*, then PATH, then the per-user cache, then \
                 this host's install locations.";
    match host()
    {
        Host::Windows =>
        {
            println!("{order} `cargo xtask tools --install` obtains a missing one with winget or \
                      a pinned download.")
        }
        _ =>
        {
            println!("{order} This build does not install host tools on {}; use your package \
                      manager.",
                     host_label())
        }
    }
    for program in programs
    {
        println!("{}", Status::of(program).line());
    }
}
