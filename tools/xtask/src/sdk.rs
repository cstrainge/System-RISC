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

pub(super) fn run(workspace: &Workspace, action: &str) -> Result<()>
{
    crate::run(workspace.command("python3")
                        .arg(workspace.root.join("scripts/system7_sdk.py"))
                        .arg(action)
                        .arg("--build-dir")
                        .arg(&workspace.build)
                        .arg("--compiler-proxy")
                        .arg(env::current_exe()?)
                        .arg("--llvm-profile")
                        .arg(&workspace.platform.llvm_profile))
}
