//! Private rustc sysroot selection for Cargo's build-std source discovery.
//! Using this existing native host executable also works on Windows: RUSTC
//! cannot portably name a Python script or a command with appended arguments.
use std::{env,
          path::Path,
          process::{Command, ExitCode}};

use mac_build::{Result, error};

use crate::Workspace;

pub(super) fn compiler_proxy() -> Option<ExitCode>
{
    let compiler = env::var_os("SYSTEM7_SDK_COMPILER")?;
    let run = || -> Result<ExitCode> {
        let sysroot = env::var_os("SYSTEM7_SDK_SYSROOT").ok_or_else(|| {
                                                            error("SDK compiler requires \
                                                                   SYSTEM7_SDK_SYSROOT")
                                                        })?;
        if !Path::new(&sysroot).join("lib/rustlib").is_dir()
        {
            return Err(error("SDK sysroot is missing; run cargo xtask sdk prepare"));
        }
        let args: Vec<_> = env::args_os().skip(1).collect();
        if args.iter()
               .any(|arg| arg == "--sysroot" || arg.to_string_lossy().starts_with("--sysroot="))
        {
            return Err(error("SDK compiler refuses a second sysroot selection"));
        }
        let status = Command::new(compiler).args(args)
                                           .arg("--sysroot")
                                           .arg(sysroot)
                                           .args(["-Z", "unstable-options"])
                                           .env_remove("SYSTEM7_SDK_COMPILER")
                                           .status()?;
        Ok(ExitCode::from(status.code().and_then(|code| u8::try_from(code).ok()).unwrap_or(1)))
    };
    Some(match run()
         {
             Ok(status) => status,
             Err(cause) =>
             {
                 eprintln!("System RISC SDK compiler: {cause}");
                 ExitCode::FAILURE
             }
         })
}

fn command(workspace: &Workspace, action: &str) -> Result<Command>
{
    let mut command = workspace.command("python3");
    command.arg(workspace.root.join("scripts/system7_sdk.py"))
           .arg(action)
           .arg("--build-dir")
           .arg(&workspace.build)
           .arg("--compiler-proxy")
           .arg(env::current_exe()?)
           .arg("--llvm-profile")
           .arg(&workspace.platform.llvm_profile);
    Ok(command)
}

pub(super) fn run(workspace: &Workspace, action: &str, manifest: Option<&str>) -> Result<()>
{
    if let Some(manifest) = manifest
    {
        application(workspace, Path::new(manifest), &[], None)?;
    }
    else
    {
        crate::run(&mut command(workspace, action)?)?;
    }
    Ok(())
}

/// The toolchain record the platform's compiler is pinned to.
fn pin(workspace: &Workspace) -> Result<serde_json::Value>
{
    let path = workspace.root.join("platforms/rust/sdk.json");
    Ok(serde_json::from_slice(&std::fs::read(&path)?)?)
}

fn rustup(args: &[&str]) -> Result<std::process::Output>
{
    let program = env::var_os("RUSTUP").unwrap_or_else(|| "rustup".into());
    let mut command = Command::new(&program);
    command.args(args);
    command.output().map_err(|cause| {
                        error(format!("cannot run '{} {}': {cause}",
                                      program.to_string_lossy(),
                                      args.join(" ")))
                    })
}

fn installed(listing: &[u8], name: &str) -> bool
{
    String::from_utf8_lossy(listing).lines()
                                    .any(|line| line.split_whitespace().next() == Some(name))
}

/// Install everything the platform's toolchain record asks for, so a first
/// clone needs no manual `rustup`, `sdk prepare` or `sdk build`.
pub(super) fn prerequisites(workspace: &Workspace) -> Result<()>
{
    let pin = pin(workspace)?;
    let field = |name: &str| -> Result<String> {
        pin[name].as_str()
                 .map(str::to_string)
                 .ok_or_else(|| error(format!("platforms/rust/sdk.json lacks '{name}'")))
    };
    let toolchain = field("rustup_toolchain")?;
    let target = workspace.platform.rust_target.clone();
    let listing = rustup(&["toolchain", "list"])?;
    if !listing.status.success()
    {
        return Err(error(format!("cannot list the rustup toolchains:\n{}",
                                 String::from_utf8_lossy(&listing.stderr))));
    }
    if !installed(&listing.stdout, &toolchain)
    {
        println!("Installing the pinned toolchain {toolchain}");
        let install = rustup(&["toolchain", "install", &toolchain, "--profile", "minimal",
                               "--component", "rust-src"])?;
        if !install.status.success()
        {
            return Err(error(format!("cannot install the pinned toolchain {toolchain}; the \
                                      platform record requires it:\n{}",
                                     String::from_utf8_lossy(&install.stderr))));
        }
    }
    let targets = rustup(&["target", "list", "--installed", "--toolchain", &toolchain])?;
    if !targets.status.success()
    {
        return Err(error(format!("cannot list the targets of {toolchain}:\n{}",
                                 String::from_utf8_lossy(&targets.stderr))));
    }
    if !installed(&targets.stdout, &target)
    {
        println!("Installing the guest target {target} for {toolchain}");
        let add = rustup(&["target", "add", &target, "--toolchain", &toolchain])?;
        if !add.status.success()
        {
            return Err(error(format!("cannot install the guest target {target} for \
                                      {toolchain}, which platform '{}' requires:\n{}",
                                     workspace.platform.name,
                                     String::from_utf8_lossy(&add.stderr))));
        }
    }
    if private(workspace)?.is_none()
    {
        crate::run(&mut command(workspace, "build")?)?;
    }
    Ok(())
}

/// One application to compile and package.
pub(super) struct App
{
    /// Directory under `apps/`, which is the application's identity.
    pub directory: &'static str,
    /// Features of that application's package to enable.
    pub features:  &'static [&'static str]
}

impl App
{
    pub(super) fn new(directory: &'static str, features: &'static [&'static str]) -> Self
    {
        Self { directory,
               features }
    }
}

/// The `[package] name` an application directory declares.
///
/// The Cargo package name is not the directory name, so it is read from the
/// manifest rather than restated in this list.
fn package_name(workspace: &Workspace, directory: &str) -> Result<String>
{
    let manifest = workspace.root.join("apps").join(directory).join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)?;
    let mut in_package = false;
    for line in text.lines()
    {
        let line = line.trim();
        if line.starts_with('[')
        {
            in_package = line == "[package]";
            continue;
        }
        if in_package
        {
            if let Some(rest) = line.strip_prefix("name")
            {
                if let Some(value) = rest.trim_start().strip_prefix('=')
                {
                    return Ok(value.trim().trim_matches('"').to_string());
                }
            }
        }
    }
    Err(error(format!("{} declares no [package] name", manifest.display())))
}

/// The private SDK a previous `sdk prepare`/`sdk build` left behind.
struct Private
{
    root:    std::path::PathBuf,
    sysroot: std::path::PathBuf,
    target:  std::path::PathBuf,
    rustc:   std::path::PathBuf,
    cargo:   std::path::PathBuf
}

fn private(workspace: &Workspace) -> Result<Option<Private>>
{
    let current = workspace.build.join("sdk/current.json");
    let Ok(bytes) = std::fs::read(&current)
    else
    {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let field = |name: &str| -> Result<std::path::PathBuf> {
        value[name].as_str().map(std::path::PathBuf::from).ok_or_else(|| {
                                                              error(format!("SDK receipt {} \
                                                                             lacks '{name}'",
                                                                            current.display()))
                                                          })
    };
    let private = Private { root:    field("root")?,
                            sysroot: field("sysroot")?,
                            target:  field("target")?,
                            rustc:   field("rustc")?,
                            cargo:   field("cargo")? };
    if !private.root.join("build.json").is_file()
    {
        return Ok(None);
    }
    Ok(Some(private))
}

/// Compile every application in one Cargo invocation, then package each one.
///
/// The applications are workspace members, so one invocation with a shared
/// target directory compiles the Toolbox and the shared crates once instead of
/// once per application.
pub(super) fn applications(workspace: &Workspace, apps: &[App]) -> Result<Vec<serde_json::Value>>
{
    let private = match private(workspace)?
    {
        Some(private) => private,
        None =>
        {
            crate::run(&mut command(workspace, "build")?)?;
            private(workspace)?.ok_or_else(|| {
                                   error("the SDK did not produce a std build; inspect cargo \
                                          xtask sdk build")
                               })?
        }
    };
    let target_dir = workspace.target().join("applications");
    let mut packages = Vec::new();
    for app in apps
    {
        packages.push(package_name(workspace, app.directory)?);
    }
    let mut command = Command::new(&private.cargo);
    command.current_dir(&workspace.root)
           .args(["build", "--release", "--locked", "--target"])
           .arg(&private.target)
           .arg("--target-dir")
           .arg(&target_dir)
           .args(["--message-format=json-render-diagnostics", "-Zjson-target-spec"]);
    for package in &packages
    {
        command.args(["--package", package]);
    }
    for (package, feature) in
        packages.iter().zip(apps.iter()).flat_map(|(package, app)| {
                                            app.features
                                               .iter()
                                               .map(move |feature| (package, feature))
                                        })
    {
        command.args(["--features", &format!("{package}/{feature}")]);
    }
    for name in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER",
                 "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_BUILD_DIR", "SYSTEM7_CARGO_ARTIFACT_DIR"]
    {
        command.env_remove(name);
    }
    for (name, _) in env::vars_os()
    {
        if name.to_string_lossy().starts_with("CARGO_PROFILE_")
        {
            command.env_remove(name);
        }
    }
    command.env("RUSTC", env::current_exe()?)
           .env("SYSTEM7_SDK_COMPILER", &private.rustc)
           .env("SYSTEM7_SDK_SYSROOT", &private.sysroot)
           .env("RUST_TARGET_PATH", &private.root)
           .env("CARGO_ENCODED_RUSTFLAGS", "-C\u{1f}relocation-model=pic");
    let output = command.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success()
    {
        return Err(error(format!("application build failed:\n{}",
                                 String::from_utf8_lossy(&output.stderr))));
    }
    let executables = executables(&stdout);
    let mut reports = Vec::new();
    for (app, package) in apps.iter().zip(packages.iter())
    {
        let manifest = workspace.root.join("apps").join(app.directory).join("resources.json");
        let resources: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest)?)?;
        let binary = resources["binary_name"].as_str()
                                             .or_else(|| resources["name"].as_str())
                                             .ok_or_else(|| {
                                                 error(format!("{} names no binary",
                                                               manifest.display()))
                                             })?;
        let elf = executables.iter()
                             .find(|(name, _)| name == binary)
                             .map(|(_, path)| path.clone())
                             .ok_or_else(|| {
                                 error(format!("the build produced no executable named \
                                                '{binary}' for application '{package}'"))
                             })?;
        reports.push(package_app(workspace, &manifest, &elf, &target_dir)?);
    }
    Ok(reports)
}

/// Target name to executable path, from Cargo's JSON messages.
fn executables(stdout: &str) -> Vec<(String, std::path::PathBuf)>
{
    stdout.lines()
          .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
          .filter(|message| message["reason"] == "compiler-artifact")
          .filter_map(|message| {
              let name = message["target"]["name"].as_str()?.to_string();
              let path = message["executable"].as_str()?;
              Some((name, std::path::PathBuf::from(path)))
          })
          .collect()
}

/// Package a compiled application into the platform's output directory.
fn package_app(workspace: &Workspace, resources: &Path, elf: &Path, artifacts: &Path)
               -> Result<serde_json::Value>
{
    let package = mac_build::package::build_requiring_with_artifacts(
        resources,
        elf,
        workspace.dist(),
        &[mac_formats::requirements::Requirement { class: mac_entropy_interface::CLASS,
                                                   version: mac_entropy_interface::VERSION }],
        artifacts)?;
    let name = package["name"].as_str().ok_or_else(|| error("package lacks its name"))?;
    let receipt = serde_json::json!({"state": "packaged-not-executed",
                                     "native_execution_observed": false,
                                     "resource_manifest": resources.to_string_lossy(),
                                     "elf": elf.to_string_lossy(),
                                     "package": package});
    crate::write_json(&workspace.dist().join(format!("{name}.sdk.json")), &receipt)?;
    println!("Packaged std application {name}. Native execution has not been observed.");
    Ok(package)
}

/// The image builder and standalone SDK command use the same compiler,
/// platform requirements and packager. Low-level definitions remain separate.
pub(super) fn application(workspace: &Workspace, manifest: &Path, features: &[&str],
                          output: Option<&Path>)
                          -> Result<serde_json::Value>
{
    let artifacts =
        workspace.build.join("target").join(&workspace.platform.rust_target).join("release");
    let mut command = command(workspace, "app")?;
    command.arg("--manifest-path").arg(manifest).arg("--artifact-dir").arg(&artifacts);
    if !features.is_empty()
    {
        command.arg("--features").arg(features.join(","));
    }
    crate::run(&mut command)?;
    let receipt = workspace.build.join("sdk/application.json");
    let mut result: serde_json::Value = serde_json::from_slice(&std::fs::read(&receipt)?)?;
    let path = |name: &str| -> Result<&Path> {
        result[name].as_str().map(Path::new).ok_or_else(|| error("SDK app receipt lacks paths"))
    };
    let destination = output.unwrap_or(path("output")?).to_path_buf();
    let packaged = mac_build::package::build_requiring_with_artifacts(
        path("resource_manifest")?, path("elf")?, &destination,
        &[mac_formats::requirements::Requirement { class: mac_entropy_interface::CLASS,
                                                   version: mac_entropy_interface::VERSION }],
        &artifacts);
    let package = match packaged
    {
        Ok(package) => package,
        Err(cause) =>
        {
            result["package_error"] = cause.to_string().into();
            crate::write_json(&receipt, &result)?;
            return Err(cause);
        }
    };
    result["output"] = destination.to_string_lossy().as_ref().into();
    result["package"] = package.clone();
    result["state"] = "packaged-not-executed".into();
    crate::write_json(&receipt, &result)?;
    // Keep each application receipt even when the next app replaces the SDK's
    // current receipt. This binds an installed image to its actual std build.
    let name = package["name"].as_str().ok_or_else(|| error("SDK package lacks its name"))?;
    crate::write_json(&destination.join(format!("{name}.sdk.json")), &result)?;
    println!("Packaged std application {name}. Native execution has not been observed.");
    Ok(package)
}
