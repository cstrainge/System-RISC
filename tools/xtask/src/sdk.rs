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
