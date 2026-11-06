//! One checkout, one canonical spelling, every generated path derived from it.
//!
//! `C:\Users\nullp\workdir` is a junction onto `D:\`, so the same directory has
//! two spellings and `canonicalize` answers in a third. Ninja compares
//! dependency paths as text and cargo compares fingerprints the same way, so a
//! build that mixed spellings would see two files where there is one. The root
//! is therefore resolved once, at run time, and everything else is derived from
//! it. The Windows verbatim prefix is kept internally and stripped before a
//! path is handed to an external tool.

use std::{env, fs,
          path::{Path, PathBuf},
          process::Command};

use mac_build::{Result, error};

/// Refuse to build when the bootstrap toolchain pin and the platform's compiler
/// record disagree.
///
/// rustup reads `rust-toolchain.toml` before this program runs, so that file
/// cannot be derived. It is therefore checked instead: `sdk.json` remains the
/// authority, and a file that has drifted from it stops the build with the
/// command that fixes it, rather than building under a toolchain nobody chose.
fn verify_toolchain_pin(root: &Path, toolchain: &str) -> Result<()>
{
    let path = root.join("rust-toolchain.toml");
    let text = fs::read_to_string(&path).map_err(|why| {
                                            error(format!("cannot read {}: {why}; every build \
                                                           needs the bootstrap toolchain pin",
                                                          path.display()))
                                        })?;
    let document: toml::Value =
        toml::from_str(&text).map_err(|why| {
                                 error(format!("{} is not valid TOML: {why}", path.display()))
                             })?;
    let channel =
        document.get("toolchain")
                .and_then(|toolchain| toolchain.get("channel"))
                .and_then(toml::Value::as_str)
                .ok_or_else(|| error(format!("{} has no [toolchain] channel", path.display())))?;
    if channel != toolchain
    {
        return Err(error(format!("{} pins {channel}, but platforms/rust/sdk.json records \
                                  rustup_toolchain = {toolchain}; set both to the same \
                                  toolchain",
                                 path.display())));
    }
    Ok(())
}

/// The environment variable naming this platform's output directory.
pub const DIST_ENV: &str = "SR_DIST";
/// The environment variable that overrides the build directory.
/// Carries the resolved root to child processes, which do not share this
/// process's working directory.
pub const ROOT_ENV: &str = "SR_ROOT";
/// Marks a child that is already covered by its parent's build lease.
pub const LEASE_ENV: &str = "SR_BUILD_LEASE";

pub struct Workspace
{
    /// The canonical root of the checkout being built.
    pub root:            PathBuf,
    pub build:           PathBuf,
    /// This platform's output directory. `SR_DIST` names this directly,
    /// because every consumer wants the platform's directory rather than the
    /// root it sits in.
    pub dist:            PathBuf,
    /// Cargo's target directory.
    pub target:          PathBuf,
    /// Whether the caller chose the target directory, in which case cargo is
    /// told about it; otherwise cargo's own default inside the checkout stands.
    pub target_selected: bool,
    /// Run and boot reports.
    pub results:         PathBuf,
    /// The rustup channel the platform's toolchain record pins.
    pub toolchain:       String,
    pub checking:        bool,
    pub platform:        mac_build::platform::Platform
}

impl Workspace
{
    pub fn new(build: Option<PathBuf>, dist: Option<PathBuf>, target: Option<PathBuf>,
               checking: bool, platform: mac_build::platform::Platform)
               -> Result<Self>
    {
        let root = canonical_root()?;
        let build = match build
        {
            Some(path) => absolute(&root, path),
            None =>
            {
                let normal = mac_build::paths::build_dir(&root);
                // A checking build flips package features, so it gets its own
                // build directory and, below, its own target directory.
                if checking { normal.join("check") } else { normal }
            }
        };
        fs::create_dir_all(&build)?;
        // Canonicalise the build directory as well as the root, so two
        // spellings of the same place cannot become two directories. The
        // isolation branch added this, and it is kept: the old guard beside it
        // in `main.rs` - "build output must be outside the OS source
        // repository" - is deliberately *not* kept, because B13 reversed that
        // rule and the build now lives inside the checkout.
        let build = mac_build::paths::canonical(&build)?;
        // One meaning for the output directory, stated once here and
        // implemented once here: `dist` is the platform's own directory.
        // `--dist` names the root to create it inside, because a flag typed on
        // a command line reads as "put the output here"; `SR_DIST` names the
        // platform's directory itself, because every child process and script
        // that inherits it wants the directory it writes into. Precedence is
        // the flag, then the variable, then `dist/` inside the checkout.
        let dist = match dist
        {
            Some(path) if !path.as_os_str().is_empty() =>
            {
                absolute(&root, path).join(&platform.name)
            }
            _ =>
            {
                match env::var_os(DIST_ENV).map(PathBuf::from)
                {
                    Some(path) if !path.as_os_str().is_empty() => absolute(&root, path),
                    _ => root.join("dist").join(&platform.name)
                }
            }
        };
        fs::create_dir_all(&dist)?;
        let selected = target.or_else(|| env::var_os("CARGO_TARGET_DIR").map(PathBuf::from))
                             .filter(|path| !path.as_os_str().is_empty());
        let target_selected = selected.is_some();
        let target = match selected
        {
            Some(path) => absolute(&root, path),
            // Sharing one target between the two modes would make cargo
            // rebuild the largest crates on every alternation, because the
            // features differ. Separate fingerprints keep a warm build warm.
            None if checking => build.join("target"),
            None => root.join("target")
        };
        // Reports describe what was observed, not what was built, and the
        // capture scripts own this path. `--dist` therefore relocates build
        // output only; a report stays beside the checkout it is about.
        let results = root.join("results");
        let toolchain = mac_build::platform::rust_toolchain(&root, &platform)?;
        verify_toolchain_pin(&root, &toolchain)?;
        Ok(Self { root,
                  build,
                  dist,
                  target,
                  target_selected,
                  results,
                  checking,
                  toolchain,
                  platform })
    }

    pub fn root(&self) -> &Path
    {
        &self.root
    }

    pub fn results(&self) -> &Path
    {
        &self.results
    }

    pub fn dist(&self) -> &Path
    {
        &self.dist
    }

    pub fn target(&self) -> &Path
    {
        &self.target
    }

    /// The storage image this platform's machine boots from, named by the
    /// platform record rather than by this program.
    pub fn disk_image(&self) -> Result<PathBuf>
    {
        let storage = self.platform.storage.as_ref().ok_or_else(|| {
                                                         error(format!("platform '{}' declares \
                                                                        no storage images",
                                                                       self.platform.name))
                                                     })?;
        let name = self.boot_image_name()?;
        let image = storage.image(name).ok_or_else(|| {
                                            error(format!("platform '{}' boots image '{name}' \
                                                           but does not declare it in \
                                                           storage.images",
                                                          self.platform.name))
                                        })?;
        Ok(self.dist().join(image.file(name)))
    }

    pub(crate) fn boot_image_name(&self) -> Result<&str>
    {
        self.platform
            .emulation()
            .and_then(|emulation| emulation.boot_image())
            .ok_or_else(|| {
                error(format!("platform '{}' does not mark any emulation.disks.attach entry \
                               `\"boot\": true`, so the machine's boot medium is undeclared",
                              self.platform.name))
            })
    }

    /// A child process that inherits this build's platform, directories and
    /// root, so it never has to guess at any of them.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command
    {
        let mut command = Command::new(program);
        command.env(mac_build::paths::BUILD_DIR_ENV, &self.build);
        command.env(mac_build::platform::PLATFORM_ENV, &self.platform.name);
        command.env(DIST_ENV, &self.dist);
        command.env(ROOT_ENV, &self.root);
        command.current_dir(&self.root);
        command
    }

    pub fn describe(&self, path: &Path) -> String
    {
        path.strip_prefix(&self.root).unwrap_or(path).display().to_string()
    }

    /// Remove this platform's output and the cargo target directories.
    ///
    /// Everything is inside the checkout by default and an override may point
    /// anywhere, so each removal is refused unless the resolved path really is
    /// inside this checkout. Nothing here deletes a checkout or a parent.
    pub fn clean(&self) -> Result<()>
    {
        let mut removed = Vec::new();
        for path in [self.dist.clone(), self.target.clone(), self.checking_target()]
        {
            if !path.exists()
            {
                continue;
            }
            if !inside(&self.root, &path)
            {
                return Err(error(format!("refusing to remove '{}', which is outside the \
                                          checkout '{}'",
                                         path.display(),
                                         self.root.display())));
            }
            fs::remove_dir_all(&path)?;
            removed.push(self.describe(&path));
        }
        if removed.is_empty()
        {
            println!("Nothing to clean for platform '{}'.", self.platform.name);
        }
        else
        {
            println!("Removed {}", removed.join(", "));
        }
        Ok(())
    }

    /// The target directory a checking build uses, which is not the ordinary
    /// one because the two modes enable different features.
    fn checking_target(&self) -> PathBuf
    {
        mac_build::paths::build_dir(&self.root).join("check/target")
    }

    pub fn cargo(&self) -> Command
    {
        let mut command = self.command(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
        // `RUSTUP_TOOLCHAIN` rather than a `+channel` argument: `CARGO` may
        // already name a toolchain-specific binary, which rejects `+channel`,
        // while the shim honours the variable.
        command.env("RUSTUP_TOOLCHAIN", &self.toolchain);
        // Never let a cargo this build spawns install into the shared toolchain.
        // A nested build is not the place that decides what the toolchain
        // contains: two running at once delete each other's files, and the
        // process that would notice is long gone by the time a later build fails
        // on a missing library. Installing is `prerequisites`' job, once, at top
        // level.
        command.env("RUSTUP_AUTO_INSTALL", "0");
        // An unselected target leaves cargo's own default in place.
        if self.target_selected
        {
            command.env("CARGO_TARGET_DIR", &self.target);
        }
        command
    }

    /// The pinned compiler itself, so a reported version is the one the build
    /// actually used rather than whatever `rustc` happens to be first on PATH.
    pub fn rustc(&self) -> Command
    {
        let mut command = self.command(env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()));
        command.env("RUSTUP_TOOLCHAIN", &self.toolchain);
        command.arg("--version");
        command
    }
}

/// The checkout this build belongs to, resolved at run time in one canonical
/// spelling.
///
/// A parent process passes the answer down in `SR_ROOT`, because children do
/// not share its working directory. Otherwise walk upwards from the working
/// directory and then from the executable, looking for the markers of a
/// checkout. `CARGO_MANIFEST_DIR` is deliberately not used: it is baked in when
/// the binary is compiled, so a binary carried to another worktree would report
/// the worktree it was built in.
fn canonical_root() -> Result<PathBuf>
{
    let selected = match env::var_os(ROOT_ENV).map(PathBuf::from)
    {
        Some(root) => root,
        None =>
        {
            find_root().ok_or_else(|| {
                           error(format!("cannot find the checkout: no directory above {:?} \
                                          holds both Cargo.toml and platforms/",
                                         env::current_dir().unwrap_or_default()))
                       })?
        }
    };
    Ok(plain(selected.canonicalize().unwrap_or(selected)))
}

fn find_root() -> Option<PathBuf>
{
    let mut starts = Vec::new();
    if let Ok(directory) = env::current_dir()
    {
        starts.push(directory);
    }
    if let Ok(exe) = env::current_exe()
    {
        starts.extend(exe.parent().map(Path::to_path_buf));
    }
    starts.into_iter().find_map(|start| {
                          start.ancestors()
                               .find(|directory| {
                                   directory.join("Cargo.toml").is_file()
                                   && directory.join("platforms").is_dir()
                               })
                               .map(Path::to_path_buf)
                      })
}

/// Strip the Windows verbatim prefix (`\\?\`) so a path handed to ninja or
/// cargo is the one a human would type.
pub fn plain(path: PathBuf) -> PathBuf
{
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\")
    {
        Some(rest) => PathBuf::from(rest),
        None => path
    }
}

/// Resolve a possibly relative path against the checkout root.
pub fn absolute(root: &Path, path: PathBuf) -> PathBuf
{
    if path.is_absolute() { plain(path) } else { root.join(path) }
}

/// Whether a path lies inside the checkout, comparing canonical forms so a
/// junction or a differently-cased spelling cannot escape the check. Both
/// sides are canonicalised: comparing a canonical path against a raw one is
/// how a check silently stops firing on Windows.
pub fn inside(root: &Path, path: &Path) -> bool
{
    let root = root.canonicalize().map(plain).unwrap_or_else(|_| root.to_path_buf());
    let path = path.canonicalize().map(plain).unwrap_or_else(|_| path.to_path_buf());
    let (root, path) = if cfg!(windows)
    {
        (root.to_string_lossy().to_lowercase(), path.to_string_lossy().to_lowercase())
    }
    else
    {
        (root.to_string_lossy().into_owned(), path.to_string_lossy().into_owned())
    };
    path == root || path.starts_with(&format!("{root}{}", std::path::MAIN_SEPARATOR))
}

/// Keep two writers out of the same output directory. The OS releases the
/// lock when the process exits, including on a crash, so nothing has to clean
/// up after a killed build.
pub struct BuildLease
{
    _lock: fs::File
}

impl BuildLease
{
    pub fn acquire(directory: &Path) -> Result<Self>
    {
        let lock = fs::OpenOptions::new().read(true)
                                         .write(true)
                                         .create(true)
                                         .truncate(false)
                                         .open(directory.join(".build.lock"))?;
        lock.try_lock()
            .map_err(|e| {
                error(format!("build directory {} is already in use: {e}", directory.display()))
            })?;
        Ok(Self { _lock: lock })
    }
}
