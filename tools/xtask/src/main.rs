mod disk;
mod boot;
mod build;
mod docs;
mod authority;
mod ninja;
mod resource_map;
mod modules;
mod sdk;
mod elf;
mod emulator;
mod flash;
mod tools;
mod workspace;
mod crates;
mod cli;
mod patch;

use std::{env, fs,
          path::{Path, PathBuf},
          process::{Command, ExitCode}};

use mac_build::{Result, error, package};
use serde_json::Value;
pub(crate) use workspace::{DIST_ENV, LEASE_ENV, ROOT_ENV, Workspace};

fn run(command: &mut Command) -> Result<()>
{
    let status = command.status()?;
    if !status.success()
    {
        return Err(error(format!("command failed ({status}): {command:?}")));
    }
    Ok(())
}

/// Record what a stage produced.
///
/// The stamp's bytes digest the output tree, and it is only rewritten when that
/// digest changes. Ninja's `restat` then sees an unchanged stamp and leaves the
/// stages downstream of it alone, which is what makes the storage image depend
/// on the files it contains rather than on the build having run.
fn stamp(path: &Path, dist: &Path) -> Result<()>
{
    let mut digest = Vec::new();
    digest_tree(dist, dist, &mut digest)?;
    if fs::read(path).is_ok_and(|previous| previous == digest)
    {
        return Ok(());
    }
    fs::write(path, digest)?;
    Ok(())
}

fn digest_tree(base: &Path, directory: &Path, digest: &mut Vec<u8>) -> Result<()>
{
    let mut entries: Vec<PathBuf> =
        fs::read_dir(directory)?.map(|entry| entry.map(|entry| entry.path()))
                                .collect::<std::io::Result<_>>()?;
    entries.sort();
    for path in entries
    {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        // The graph's own directory holds this build's log and stamps, and the
        // lease belongs to the process, not to the artifacts.
        if name == "ninja" || name == ".build.lock"
        {
            continue;
        }
        if path.is_dir()
        {
            digest_tree(base, &path, digest)?;
            continue;
        }
        let metadata = fs::metadata(&path)?;
        let relative = path.strip_prefix(base).unwrap_or(&path).to_string_lossy();
        digest.extend_from_slice(relative.as_bytes());
        digest.push(0);
        digest.extend_from_slice(&metadata.len().to_le_bytes());
        let modified =
            metadata.modified()?.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        digest.extend_from_slice(&modified.as_nanos().to_le_bytes());
        digest.push(b'\n');
    }
    Ok(())
}

fn output(command: &mut Command) -> Result<String>
{
    let output = command.output()?;
    if !output.status.success()
    {
        return Err(error(format!("command failed: {command:?}\n{}",
                                 String::from_utf8_lossy(&output.stderr))));
    }
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

fn write_json(path: &Path, value: &Value) -> Result<()>
{
    fs::create_dir_all(path.parent().unwrap())?;
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(path, bytes)?;
    Ok(())
}


fn main() -> ExitCode
{
    // The private application SDK drives the compiler through this same
    // binary, so the proxy is answered before any argument is parsed.
    if let Some(status) = sdk::compiler_proxy()
    {
        return status;
    }
    match execute()
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) =>
        {
            eprintln!("xtask: {why}");
            ExitCode::FAILURE
        }
    }
}

/// Parse the command line, resolve the workspace, then run the command.
///
/// Everything the run depends on comes from the platform record the flags
/// select, so the two commands a person needs are `cargo xtask build` and
/// `cargo xtask run`.
fn execute() -> Result<()>
{
    let (global, invocation) = cli::parse(env::args().skip(1))?;

    // Listing the platforms and printing the help must work even when the
    // default platform cannot be loaded, which is exactly when they help most.
    match &invocation.command
    {
        cli::Command::Help =>
        {
            cli::help();
            return Ok(());
        }
        cli::Command::Platforms =>
        {
            for (name, summary) in mac_build::platform::catalogue()?
            {
                println!("{name:<16} {summary}");
            }
            return Ok(());
        }
        _ =>
        {}
    }

    let name = global.platform
                     .or_else(|| env::var(mac_build::platform::PLATFORM_ENV).ok())
                     .unwrap_or_else(|| mac_build::platform::DEFAULT_PLATFORM.into());
    // The packaging library resolves the selected record through a process-wide
    // singleton that reads this variable, while this binary resolves it from its
    // own command line. Publishing the selection here makes the two agree; a
    // mismatch silently packages against another machine's link geometry.
    // SAFETY: no thread has been spawned yet, so nothing else can be reading or
    // writing the environment concurrently.
    unsafe { env::set_var(mac_build::platform::PLATFORM_ENV, &name) };
    let workspace = Workspace::new(global.build,
                                   global.dist,
                                   global.target,
                                   invocation.spec.checking,
                                   mac_build::platform::load(&name)?)?;

    let lease = invocation.lease(&workspace)?;

    if matches!(&invocation.command, cli::Command::Check)
    {
        let checked = build::check(&workspace);
        // A failed check must not strand its writable disk copies. Release the
        // lease, clean up, then report the check's own error.
        drop(lease);
        let cleaned = build::prune(&workspace);
        checked?;
        return cleaned;
    }

    let result = dispatch(&workspace, invocation.command);
    drop(lease);
    result
}

fn dispatch(workspace: &Workspace, command: cli::Command) -> Result<()>
{
    match command
    {
        cli::Command::Help => cli::help(),
        cli::Command::Platforms =>
        {
            for (name, summary) in mac_build::platform::catalogue()?
            {
                println!("{name:<16} {summary}");
            }
        }
        cli::Command::Tools { install } => tools_command(workspace, install)?,
        cli::Command::Run { mode,
                            no_build,
                            extra } =>
        {
            if no_build
            {
                boot::run(workspace, mode, &[], &extra)?;
            }
            else
            {
                build::run_platform(workspace, mode, None, &extra)?;
            }
        }
        cli::Command::SdkPrepare =>
        {
            let sdk = sdk::prepare(workspace)?;
            println!("Private application SDK: {}", workspace.describe(&sdk.root));
        }
        cli::Command::SdkBuild =>
        {
            let sdk = sdk::prepare(workspace)?;
            sdk::build_std(workspace, &sdk)?;
        }
        cli::Command::SdkApp { manifest,
                               features } => build::sdk_app(workspace, &manifest, &features)?,
        cli::Command::Build => build::build(workspace)?,
        cli::Command::Stage(name) => build::stage(workspace, &name)?,
        cli::Command::ModulesCheck =>
        {
            disk::validate_modules(workspace)?;
            println!("Installed module identities are unique within each loading namespace.");
        }
        cli::Command::Clean => workspace.clean()?,
        cli::Command::Doc => docs::generate(workspace)?,
        cli::Command::Check => build::check(workspace)?,
        cli::Command::Rom => build::rom(workspace)?,
        cli::Command::BootCheck =>
        {
            build::build(workspace)?;
            boot::check(workspace)?;
        }
        cli::Command::Boot { disk } =>
        {
            build::run_platform(workspace, emulator::Mode::Interactive, disk.as_deref(), &[])?
        }
        cli::Command::Flash { target, yes } =>
        {
            flash::run(workspace, &target, yes)?
        }
        cli::Command::Inspect(path) =>
        {
            println!("{}", serde_json::to_string_pretty(&package::inspect(&fs::read(path)?)?)?)
        }
        cli::Command::Rebuild { manifest,
                                directory } =>
        {
            println!("{}",
                     serde_json::to_string_pretty(&package::rebuild(Path::new(&manifest),
                                                                    Path::new(&directory))?)?);
        }
        cli::Command::Package { manifest,
                                executable,
                                directory } =>
        {
            let report = package::build(Path::new(&manifest),
                                        Path::new(&executable),
                                        Path::new(&directory))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        cli::Command::ResourceMap(path) =>
        {
            println!("{}", resource_map::inspect(Path::new(&path))?);
        }
    }
    Ok(())
}

/// Report the host tools this platform's build and emulator need, or obtain a
/// missing one.
///
/// Installing is a deliberate act: only this command does it, and every other
/// command that needs a tool fails with the same instructions instead.
fn tools_command(workspace: &Workspace, install: bool) -> Result<()>
{
    let mut programs: Vec<String> = workspace.platform
                                             .emulation()
                                             .map(|emulation| vec![emulation.binary.clone()])
                                             .unwrap_or_default();
    programs.push("ninja".into());
    if install
    {
        tools::install(&programs)?;
    }
    tools::report(&programs);
    Ok(())
}
