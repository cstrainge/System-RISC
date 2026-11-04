//! The command line, described once.
//!
//! Every command is one row: its name, the text `help` prints, whether it
//! writes this platform's output and therefore needs the build lease, whether
//! it is a checking build with its own target directory, and the shape of its
//! arguments. `help` and the dispatcher read the same table, so a command
//! cannot be documented and not implemented, or implemented and not
//! documented, and a new command cannot quietly skip the lease.

use std::{env,
          path::{Path, PathBuf}};

use mac_build::{Result, error};

use crate::{emulator, workspace::Workspace};

/// A flag that applies to every command.
pub struct Flag
{
    pub name:     &'static str,
    pub argument: &'static str,
    pub summary:  &'static str
}

pub const FLAGS: &[Flag] =
    &[Flag { name:     "--platform",
             argument: "NAME",
             summary:  "Select a platform record (also SR_PLATFORM); spellings are matched with \
                        case and punctuation ignored" },
      Flag { name:     "--dist",
             argument: "DIRECTORY",
             summary:  "Output root for this build; the platform's own directory is created \
                        inside it. Defaults to dist/ in this checkout. SR_DIST names that \
                        platform directory itself, which is why it takes no platform suffix" },
      Flag { name:     "--target-dir",
             argument: "DIRECTORY",
             summary:  "Cargo target directory (also CARGO_TARGET_DIR); defaults to target/ in \
                        this checkout" },
      Flag { name:     "--build-dir",
             argument: "DIRECTORY",
             summary:  "Generated build files (also SR_BUILD_DIR); defaults to build/ in this \
                        checkout" }];

/// What a command asks for, after parsing.
pub enum Command
{
    Help,
    Platforms,
    Tools
    {
        install: bool
    },
    Run
    {
        mode:     emulator::Mode,
        no_build: bool,
        extra:    Vec<String>
    },
    SdkPrepare,
    SdkBuild,
    SdkApp
    {
        manifest: String,
        features: Vec<String>
    },
    Build,
    Stage(String),
    ModulesCheck,
    Clean,
    Doc,
    Check,
    Rom,
    BootCheck,
    Boot
    {
        disk: Option<String>
    },
    Inspect(String),
    Rebuild
    {
        manifest:  String,
        directory: String
    },
    Package
    {
        manifest:   String,
        executable: String,
        directory:  String
    },
    ResourceMap(String)
}

/// How a command's arguments are read. Keeping this in the row means the table
/// is the whole description of a command.
#[derive(Clone, Copy)]
pub enum Shape
{
    /// No arguments at all.
    Nothing(fn() -> Command),
    /// Exactly one free argument, turned into a command by the constructor.
    One(fn(String) -> Command),
    Run,
    Boot,
    Stage,
    SdkApp,
    Tools,
    Rebuild,
    Package
}

/// One command, as the table declares it.
pub struct Spec
{
    pub name:     &'static str,
    pub aliases:  &'static [&'static str],
    pub usage:    &'static str,
    pub summary:  &'static str,
    /// Writes or deletes this platform's output, so it takes the build lease.
    pub writes:   bool,
    /// Enables the tracing features, which gives it its own target directory.
    pub checking: bool,
    pub shape:    Shape
}

/// A parsed command together with the row that describes it.
pub struct Invocation
{
    pub spec:    &'static Spec,
    pub command: Command
}

pub const COMMANDS: &[Spec] =
    &[Spec { name:     "platforms",
             aliases:  &[],
             usage:    "cargo xtask platforms",
             summary:  "List the platform records in this checkout",
             writes:   false,
             checking: false,
             shape:    Shape::Nothing(|| Command::Platforms) },
      Spec { name:     "tools",
             aliases:  &[],
             usage:    "cargo xtask tools [--install]",
             summary:  "Report the host tools this platform needs, or install them",
             writes:   false,
             checking: false,
             shape:    Shape::Tools },
      Spec { name:     "build",
             aliases:  &["image"],
             usage:    "cargo xtask build",
             summary:  "Build this platform's System, definitions, applications, modules and \
                        storage images",
             writes:   true,
             checking: false,
             shape:    Shape::Nothing(|| Command::Build) },
      Spec { name:     "run",
             aliases:  &[],
             usage:    "cargo xtask run [--headless] [--no-build] [-- ARGS]",
             summary:  "Build, then boot this platform in its emulator",
             writes:   true,
             checking: false,
             shape:    Shape::Run },
      Spec { name:     "boot",
             aliases:  &[],
             usage:    "cargo xtask boot [DISK]",
             summary:  "Boot the built platform, or the given disk, in the emulator",
             writes:   true,
             checking: false,
             shape:    Shape::Boot },
      Spec { name:     "boot-check",
             aliases:  &[],
             usage:    "cargo xtask boot-check",
             summary:  "Boot the image this platform's build produced and verify the rejection \
                        cases",
             writes:   true,
             // Not a checking build: the point is to verify the artefacts
             // `build` produces, so it uses the same target directory and the
             // same features. A differently-configured rebuild would prove
             // something about a build nobody ships.
             checking: false,
             shape:    Shape::Nothing(|| Command::BootCheck) },
      Spec { name:     "check",
             aliases:  &["test"],
             usage:    "cargo xtask check",
             summary:  "Host, disk, ROM boot and graphical desktop checks, with the tracing \
                        features enabled",
             writes:   true,
             checking: true,
             shape:    Shape::Nothing(|| Command::Check) },
      Spec { name:     "clean",
             aliases:  &[],
             usage:    "cargo xtask clean",
             summary:  "Remove this platform's output and the cargo target directories",
             writes:   true,
             checking: false,
             shape:    Shape::Nothing(|| Command::Clean) },
      Spec { name:     "rom",
             aliases:  &[],
             usage:    "cargo xtask rom",
             summary:  "Build the bootloader package and the read-only boot ROM",
             writes:   true,
             checking: false,
             shape:    Shape::Nothing(|| Command::Rom) },
      Spec { name:     "stage",
             aliases:  &[],
             usage:    "cargo xtask stage NAME",
             summary:  "Run one stage of the build graph; used by ninja",
             writes:   true,
             checking: false,
             shape:    Shape::Stage },
      Spec { name:     "sdk prepare",
             aliases:  &[],
             usage:    "cargo xtask sdk prepare",
             summary:  "Materialize the pinned private Rust application SDK",
             writes:   true,
             checking: false,
             shape:    Shape::Nothing(|| Command::SdkPrepare) },
      Spec { name:     "sdk build",
             aliases:  &[],
             usage:    "cargo xtask sdk build",
             summary:  "Compile the staged System RISC standard library",
             writes:   true,
             checking: false,
             shape:    Shape::Nothing(|| Command::SdkBuild) },
      Spec { name:     "sdk app",
             aliases:  &[],
             usage:    "cargo xtask sdk app MANIFEST [--features LIST]",
             summary:  "Build one std application with the private SDK",
             writes:   true,
             checking: false,
             shape:    Shape::SdkApp },
      Spec { name:     "doc",
             aliases:  &[],
             usage:    "cargo xtask doc",
             summary:  "Generate Rust library documentation in docs/api",
             writes:   false,
             checking: false,
             shape:    Shape::Nothing(|| Command::Doc) },
      Spec { name:     "modules-check",
             aliases:  &[],
             usage:    "cargo xtask modules-check",
             summary:  "Validate packaged module identities without rebuilding",
             writes:   false,
             checking: false,
             shape:    Shape::Nothing(|| Command::ModulesCheck) },
      Spec { name:     "resource-map",
             aliases:  &[],
             usage:    "cargo xtask resource-map FILE",
             summary:  "Inspect checked resource metadata as JSON",
             writes:   false,
             checking: false,
             shape:    Shape::One(Command::ResourceMap) },
      Spec { name:     "inspect",
             aliases:  &[],
             usage:    "cargo xtask inspect FILE",
             summary:  "Validate a packaged native module",
             writes:   false,
             checking: false,
             shape:    Shape::One(Command::Inspect) },
      Spec { name:     "rebuild",
             aliases:  &[],
             usage:    "cargo xtask rebuild MANIFEST DIRECTORY",
             summary:  "Rebuild a ResEdit resource archive",
             writes:   false,
             checking: false,
             shape:    Shape::Rebuild },
      Spec { name:     "package",
             aliases:  &[],
             usage:    "cargo xtask package MANIFEST EXECUTABLE DIRECTORY",
             summary:  "Package one native module",
             writes:   false,
             checking: false,
             shape:    Shape::Package },
      Spec { name:     "help",
             aliases:  &["--help"],
             usage:    "cargo xtask help",
             summary:  "Print this list",
             writes:   false,
             checking: false,
             shape:    Shape::Nothing(|| Command::Help) }];

/// The flags every command accepts, on either side of the command name.
pub struct Global
{
    pub platform: Option<String>,
    pub build:    Option<PathBuf>,
    pub dist:     Option<PathBuf>,
    pub target:   Option<PathBuf>
}

/// Split the arguments into global flags and the command, then parse the
/// command with the row that owns it.
///
/// Global flags are recognised on either side of the command word, so
/// `cargo xtask --platform qemu_riscv build` and
/// `cargo xtask build --platform qemu_riscv` are the same invocation. The rest
/// of the stream keeps its order and reaches the command untouched, and
/// everything after `--` is never scanned at all — so
/// `cargo xtask run -- --platform x` gives the emulator a `--platform` flag
/// rather than selecting a platform record.
pub fn parse(arguments: impl Iterator<Item = String>) -> Result<(Global, Invocation)>
{
    let mut global = Global { platform: None,
                              build:    None,
                              dist:     None,
                              target:   None };
    let mut rest: Vec<String> = Vec::new();
    let mut input = arguments;
    let mut options = true;
    while let Some(argument) = input.next()
    {
        if !options
        {
            rest.push(argument);
            continue;
        }
        match argument.as_str()
        {
            "--" =>
            {
                options = false;
                rest.push(argument);
            }
            "--platform" =>
            {
                let read = value(&mut input, "--platform")?;
                once(&mut global.platform, read, "--platform")?;
            }
            "--build-dir" =>
            {
                let read = value(&mut input, "--build-dir")?;
                once(&mut global.build, PathBuf::from(read), "--build-dir")?;
            }
            "--dist" =>
            {
                let read = value(&mut input, "--dist")?;
                once(&mut global.dist, PathBuf::from(read), "--dist")?;
            }
            "--target-dir" =>
            {
                if env::var_os("CARGO_TARGET_DIR").is_some()
                {
                    return Err(error("--target-dir and CARGO_TARGET_DIR are both set; use one"));
                }
                let read = value(&mut input, "--target-dir")?;
                once(&mut global.target, PathBuf::from(read), "--target-dir")?;
            }
            _ => rest.push(argument)
        }
    }
    // `--` itself is not an argument to the command; only what follows it is.
    if let Some(position) = rest.iter().position(|argument| argument == "--")
    {
        rest.remove(position);
    }
    let Some(first) = rest.first()
    else
    {
        return Ok((global,
                   Invocation { spec:    row("help")?,
                                command: Command::Help }));
    };
    // `sdk` names its action in the second word, so its rows are matched
    // against the first two words and its arguments start at the third.
    let (spec, arguments) = if first == "sdk"
    {
        let action =
            rest.get(1).ok_or_else(|| error("sdk needs an action; use cargo xtask help"))?;
        (row(&format!("sdk {action}"))?, &rest[2..])
    }
    else
    {
        (row(first)?, &rest[1..])
    };
    let command = command(spec, arguments)?;
    Ok((global,
        Invocation { spec,
                     command }))
}

/// The row for a command name or alias.
fn row(name: &str) -> Result<&'static Spec>
{
    COMMANDS.iter()
            .find(|spec| spec.name == name || spec.aliases.iter().any(|alias| *alias == name))
            .ok_or_else(|| {
                error(format!("unknown command '{name}'; use cargo xtask help for the list"))
            })
}

/// Print the table, so documentation and dispatch cannot drift apart.
pub fn help()
{
    println!("Usage: cargo xtask COMMAND [options]\n");
    let width =
        COMMANDS.iter()
                .map(|spec| spec.usage.chars().count())
                .chain(FLAGS.iter().map(|flag| {
                                       flag.name.chars().count() + 1 + flag.argument.chars().count()
                                   }))
                .max()
                .unwrap_or_default()
                .max(20)
        + 2;
    for spec in COMMANDS
    {
        println!("  {:<width$} {}", spec.usage, spec.summary);
        if !spec.aliases.is_empty()
        {
            println!("  {:<width$} also: {}", "", spec.aliases.join(", "));
        }
    }
    println!("\nOptions:");
    for flag in FLAGS
    {
        println!("  {:<width$} {}", format!("{} {}", flag.name, flag.argument), flag.summary);
    }
    println!("\nThe build is driven by the platform record in platforms/. By default this \
              platform's output goes to dist/<platform>/ inside this checkout; --dist relocates \
              the directory it is created inside, and SR_DIST names the platform's directory \
              itself for child processes. SR_BUILD_DIR, SR_ROOT and SR_PLATFORM name the rest.");
}

/// Read one value for a flag.
fn value(input: &mut impl Iterator<Item = String>, flag: &str) -> Result<String>
{
    input.next().ok_or_else(|| error(format!("{flag} needs a value")))
}

/// Store a flag's value, refusing a second one rather than letting the last
/// spelling silently win.
fn once<T>(slot: &mut Option<T>, read: T, flag: &str) -> Result<()>
{
    if slot.replace(read).is_some()
    {
        return Err(error(format!("{flag} may only be supplied once")));
    }
    Ok(())
}

fn command(spec: &Spec, arguments: &[String]) -> Result<Command>
{
    match spec.shape
    {
        Shape::Nothing(make) =>
        {
            match arguments
            {
                [] => Ok(make()),
                _ => Err(error(format!("'{}' takes no arguments", spec.name)))
            }
        }
        Shape::One(make) =>
        {
            match arguments
            {
                [argument] if !argument.starts_with('-') => Ok(make(argument.clone())),
                _ => Err(error(format!("'{}' takes exactly one argument", spec.name)))
            }
        }
        Shape::Run => run(arguments),
        Shape::Boot => boot(arguments),
        Shape::Stage =>
        {
            match arguments
            {
                [stage] => Ok(Command::Stage(stage.clone())),
                _ => Err(error("stage needs exactly one stage name"))
            }
        }
        Shape::SdkApp => sdk_app(arguments),
        Shape::Tools =>
        {
            match arguments
            {
                [] => Ok(Command::Tools { install: false }),
                [flag] if flag == "--install" => Ok(Command::Tools { install: true }),
                [other] => Err(error(format!("unknown tools option '{other}'"))),
                _ => Err(error("'tools' takes at most one option"))
            }
        }
        Shape::Rebuild =>
        {
            match arguments
            {
                [manifest, directory] =>
                {
                    Ok(Command::Rebuild { manifest:  manifest.clone(),
                                          directory: directory.clone() })
                }
                _ => Err(error("rebuild needs a manifest and an output directory"))
            }
        }
        Shape::Package =>
        {
            match arguments
            {
                [manifest, executable, directory] =>
                {
                    Ok(Command::Package { manifest:   manifest.clone(),
                                          executable: executable.clone(),
                                          directory:  directory.clone() })
                }
                _ => Err(error("package needs a manifest, an executable and an output directory"))
            }
        }
    }
}

fn run(arguments: &[String]) -> Result<Command>
{
    let mut mode = emulator::Mode::Interactive;
    let mut no_build = false;
    let mut extra = Vec::new();
    let mut passthrough = false;
    for argument in arguments
    {
        if passthrough
        {
            extra.push(argument.clone());
            continue;
        }
        match argument.as_str()
        {
            "--headless" => mode = emulator::Mode::Headless,
            "--no-build" => no_build = true,
            "--" => passthrough = true,
            other => return Err(error(format!("unknown run option '{other}'")))
        }
    }
    Ok(Command::Run { mode,
                      no_build,
                      extra })
}

fn boot(arguments: &[String]) -> Result<Command>
{
    match arguments
    {
        [] => Ok(Command::Boot { disk: None }),
        [disk] if !disk.starts_with('-') => Ok(Command::Boot { disk: Some(disk.clone()) }),
        _ => Err(error("boot takes at most one disk image"))
    }
}

fn sdk_app(arguments: &[String]) -> Result<Command>
{
    let mut manifest = None;
    let mut features: Option<Vec<String>> = None;
    let mut input = arguments.iter();
    while let Some(argument) = input.next()
    {
        match argument.as_str()
        {
            "--features" =>
            {
                let list =
                    input.next().ok_or_else(|| error("--features needs a comma-separated list"))?;
                once(&mut features,
                     list.split(',').filter(|f| !f.is_empty()).map(String::from).collect(),
                     "--features")?;
            }
            other if other.starts_with('-') =>
            {
                return Err(error(format!("unknown sdk app option '{other}'")));
            }
            other => manifest = Some(other.to_string())
        }
    }
    let manifest = manifest.ok_or_else(|| error("sdk app needs a manifest path"))?;
    Ok(Command::SdkApp { manifest,
                         features: features.unwrap_or_default() })
}

impl Invocation
{
    /// Take the build lease when the command writes this platform's output.
    /// Policy is a property of what the command does, not of its name, so it
    /// is read from the same row that documents it.
    pub fn lease(&self, workspace: &Workspace) -> Result<Option<crate::workspace::BuildLease>>
    {
        if !self.spec.writes
        {
            return Ok(None);
        }
        // A stage child runs inside a build its parent already holds the lease
        // for, so it must not deadlock against it. The handoff is scoped to
        // exactly that: a `stage` command, and an inherited value naming the
        // very directory the parent locked. Any other setting of the variable
        // — a stray export in a shell, a stale value from another checkout —
        // is ignored and the lease is taken as usual.
        let handed_off = matches!(self.command, Command::Stage(_))
                         && env::var_os(crate::workspace::LEASE_ENV).is_some_and(|held| {
                                                                        Path::new(&held)
                                                                        == workspace.dist()
                                                                    });
        if handed_off
        {
            return Ok(None);
        }
        crate::workspace::BuildLease::acquire(workspace.dist()).map(Some)
    }
}
