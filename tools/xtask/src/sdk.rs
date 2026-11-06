//! The private Rust SDK the platform record pins.
//!
//! A platform names its compiler in `toolchain`, which is a path under
//! `platforms/` and ships as `platforms/rust/sdk.json`. That record holds the
//! rustup toolchain, the exact `rustc`/`cargo` commits, the target
//! specification and the patches applied to `rust-src`. Everything here is
//! derived from it, so a platform that names a different pin gets a different
//! SDK without the build system knowing about it.
//!
//! One checkout still resolves one toolchain per run, because rustup picks it
//! from `rust-toolchain.toml` before this program starts. `cargo xtask` checks
//! that file against the record and refuses to build when they disagree, so the
//! constraint is enforced rather than assumed.
//!
//! Two steps produce the SDK, and both are idempotent:
//!
//! * *prepare* copies the pinned `rust-src`, applies the platform's patches, overlays the
//!   workspace's private `library` crates and writes the target specification into
//!   `build/sdk/<key>`. The key digests every input, so a changed pin, patch or private crate lands
//!   in a new directory and the old one stays valid for whoever is still using it.
//! * *build* compiles `std`, `core`, `alloc` and `panic_abort` for the target with `-Zbuild-std`
//!   and installs the resulting libraries into the private sysroot.
//!
//! Applications then build against that sysroot with this executable as the
//! `RUSTC` proxy, which is what makes the private sysroot reach the compiler
//! without the caller having to know about it.

use std::{collections::{BTreeMap, BTreeSet},
          env,
          ffi::OsString,
          fs,
          path::{Path, PathBuf},
          process::{Command, ExitCode, Stdio}};

use mac_build::{Result, error};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{Workspace, patch, run, write_json};

/// Bumped when the preparation logic changes in a way that invalidates a
/// previously prepared tree. A prepared tree is not upgraded in place; it is
/// abandoned under its old key.
const BUILDER: &str = "xtask-sdk-3";

/// The proxy mode this executable runs in when the SDK build invokes it as
/// `RUSTC`.
const PROXY_ENV: &str = "SR_SDK_COMPILER";
const SYSROOT_ENV: &str = "SR_SDK_SYSROOT";

/// Environment variables that would redirect a cargo invocation away from the
/// private sysroot. Cargo reads them from the caller, so they are cleared for
/// every SDK-driven build rather than trusted.
const SCRUBBED: &[&str] = &["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER",
                            "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_BUILD_DIR",
                            "CARGO_TARGET_DIR", "CARGO_PROFILE_DEV_DEBUG",
                            "CARGO_PROFILE_DEV_INCREMENTAL", "CARGO_PROFILE_DEV_OPT_LEVEL",
                            "CARGO_PROFILE_DEV_STRIP", "CARGO_PROFILE_RELEASE_CODEGEN_UNITS",
                            "CARGO_PROFILE_RELEASE_DEBUG", "CARGO_PROFILE_RELEASE_INCREMENTAL",
                            "CARGO_PROFILE_RELEASE_LTO", "CARGO_PROFILE_RELEASE_OPT_LEVEL",
                            "CARGO_PROFILE_RELEASE_PANIC", "CARGO_PROFILE_RELEASE_STRIP"];

/// The compiler record the platform pins, as read from
/// `platforms/rust/sdk.json`.
#[derive(Debug, serde::Deserialize)]
struct Pin
{
    rustup_toolchain: String,
    rustc_commit:     String,
    cargo_commit:     String,
    target:           String,
    patches:          Vec<String>,
    rust_src_sha256:  String
}

/// The resolved compiler behind a pin.
struct Compiler
{
    rustc:         PathBuf,
    cargo:         PathBuf,
    sysroot:       PathBuf,
    host:          String,
    rustc_commit:  String,
    cargo_version: String
}

/// A prepared and built private SDK.
pub(super) struct Private
{
    pub root:    PathBuf,
    pub sysroot: PathBuf,
    pub target:  PathBuf,
    pub rustc:   PathBuf
}

/// One application to build, resolved from the workspace crate graph.
pub(super) struct App
{
    /// The package name, as cargo knows it.
    pub package:   String,
    /// The package's `Cargo.toml`.
    pub manifest:  PathBuf,
    /// The package's `resources.json`, which names the binary and its file
    /// type.
    pub resources: PathBuf,
    /// The binary target to package.
    pub binary:    String,
    /// Per-package features, from the platform's installed set.
    pub features:  Vec<String>
}

/// Answer every compiler invocation from the private sysroot.
///
/// This executable is installed as `RUSTC` for SDK and application builds. The
/// private sysroot is what makes `-Zbuild-std` read the patched sources and
/// what lets an application crate resolve `std` for a target the host toolchain
/// never built one for, so every invocation is forwarded with that sysroot
/// selected rather than only the `--print`/`--version` probes.
pub(super) fn compiler_proxy() -> Option<ExitCode>
{
    let compiler = env::var_os(PROXY_ENV)?;
    let run = || -> Result<ExitCode> {
        let sysroot = env::var_os(SYSROOT_ENV).ok_or_else(|| {
                                                  error(format!("the SDK compiler requires \
                                                                 {SYSROOT_ENV} to name its \
                                                                 private sysroot"))
                                              })?;
        let arguments: Vec<OsString> = env::args_os().skip(1).collect();

        // Cargo's own `--sysroot` would mean the caller expected a different
        // source root than the platform prepared, so the two are not merged.
        if arguments.iter().any(|argument| {
                               argument == "--sysroot"
                               || argument.to_string_lossy().starts_with("--sysroot=")
                           })
        {
            return Err(error("the SDK compiler refuses a second sysroot selection"));
        }

        let status = Command::new(&compiler).args(&arguments)
                                            .arg("--sysroot")
                                            .arg(&sysroot)
                                            .env_remove(PROXY_ENV)
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

fn pin(root: &Path, platform: &mac_build::platform::Platform) -> Result<Pin>
{
    let path = mac_build::platform::compiler_path(root, platform)?;
    let text = fs::read_to_string(&path).map_err(|cause| {
                                            error(format!("cannot read {}: {cause}",
                                                          path.display()))
                                        })?;

    serde_json::from_str(&text).map_err(|cause| {
                                   error(format!("{} is not a valid compiler pin: {cause}",
                                                 path.display()))
                               })
}

fn rustup(root: &Path, arguments: &[&str]) -> Command
{
    let mut command = Command::new("rustup");
    command.args(arguments).current_dir(root);

    command
}

/// Whether `listing` names `toolchain`, allowing for rustup's habit of
/// appending a host triple to the channel it installed.
pub fn installed(listing: &str, toolchain: &str) -> bool
{
    listing.lines().map(str::trim).any(|line| {
                                      line == toolchain
                                      || line.starts_with(&format!("{toolchain}-"))
                                      || line.starts_with(&format!("{toolchain} ("))
                                  })
}

/// Install the pinned compiler and its target, then prepare and build the SDK
/// if it is not already current.
///
/// This is the only place the build mutates the developer's rustup
/// installation, so it takes the workspace's toolchain lock and only writes
/// when something is actually missing.
pub fn prerequisites(workspace: &Workspace) -> Result<()>
{
    let pin = pin(workspace.root(), &workspace.platform)?;

    let listing = run_output(rustup(workspace.root(), &["toolchain", "list"]))?;
    let missing_toolchain = !installed(&listing, &pin.rustup_toolchain);

    if missing_toolchain
    {
        let _lock = toolchain_lock(workspace.root())?;

        println!("installing the pinned toolchain {}", pin.rustup_toolchain);

        let mut command = rustup(workspace.root(),
                                 &["toolchain", "install", &pin.rustup_toolchain, "--profile",
                                   "minimal"]);
        command.args(["--component", "rust-src"]);

        let _ = run(&mut command)?;
    }
    else
    {
        // The target is a JSON specification this repository owns, so it is
        // built from source rather than added as a rustup target; what rustup
        // has to supply is that source.
        let components = run_output(rustup(workspace.root(),
                                           &["component", "list", "--installed", "--toolchain",
                                             &pin.rustup_toolchain]))?;
        if !components.lines().any(|line| line.trim() == "rust-src")
        {
            let _lock = toolchain_lock(workspace.root())?;

            println!("installing the rust-src component for {}", pin.rustup_toolchain);

            let _ = run(&mut rustup(workspace.root(),
                                    &["component", "add", "rust-src", "--toolchain",
                                      &pin.rustup_toolchain]))?;
        }
    }

    // The private SDK exists to build `std` applications for the platform's
    // declared standard-library target. A platform that declares none cannot
    // install one — `platform::validate_std_target` rejects such a record — so
    // preparing the SDK here would build another platform's standard library
    // and reject this platform's own compiler record.
    if workspace.platform.std_target.is_some()
    {
        let sdk = prepare(workspace)?;

        if !sdk.root.join("build.json").is_file()
        {
            build_std(workspace, &sdk)?;
        }
    }

    Ok(())
}

fn toolchain_lock(root: &Path) -> Result<fs::File>
{
    let path = mac_build::paths::build_dir(root).join("toolchain.lock");

    fs::create_dir_all(path.parent().expect("the build directory has a parent"))?;

    let file = fs::OpenOptions::new().create(true).write(true).truncate(false).open(&path)?;
    file.lock()?;

    Ok(file)
}

/// Prepare the private SDK and return it, reusing a prepared tree whose inputs
/// still match.
pub(super) fn prepare(workspace: &Workspace) -> Result<Private>
{
    let root = workspace.root();
    let pin = pin(root, &workspace.platform)?;
    let compiler = resolve(root, &pin)?;

    // Patch paths are relative to the record that names them, so a platform
    // pointing at a different pin directory moves its patches with it.
    let port =
        mac_build::platform::compiler_path(root, &workspace.platform)?.parent()
                                                                      .ok_or_else(|| {
                                                                          error("the compiler \
                                                                                 record has no \
                                                                                 directory")
                                                                      })?
                                                                      .to_path_buf();
    let upstream = compiler.sysroot.join("lib/rustlib/src/rust");

    if !upstream.join("library/std/Cargo.toml").is_file()
    {
        return Err(error(format!("{} lacks rust-src; install the `rust-src` component for {}",
                                 upstream.display(),
                                 pin.rustup_toolchain)));
    }

    let target_source = port.join(format!("{}.json", pin.target));

    if !target_source.is_file()
    {
        return Err(error(format!("the platform pins target {} but {} does not describe it",
                                 pin.target,
                                 target_source.display())));
    }

    let target_text = fs::read_to_string(&target_source)?;
    let target: Value = serde_json::from_str(&target_text).map_err(|cause| {
                            error(format!("{} is not valid JSON: {cause}", target_source.display()))
                        })?;

    let expected = format!("+{}", workspace.platform.llvm_profile);

    if target.get("features").and_then(Value::as_str) != Some(expected.as_str())
    {
        return Err(error(format!("{} must declare features \"{}\" to match the platform's \
                                  llvm profile; it declares {}",
                                 target_source.display(),
                                 workspace.platform.llvm_profile,
                                 target.get("features")
                                       .and_then(Value::as_str)
                                       .unwrap_or("nothing"))));
    }

    let source_digest = digest_tree(&upstream)?;

    if source_digest != pin.rust_src_sha256
    {
        return Err(error(format!("the pinned rust-src for {} does not match \
                                  platforms/rust/sdk.json; the pin records {} and the \
                                  installed source digests to {}. Update the pin deliberately \
                                  or install the pinned toolchain.",
                                 pin.rustup_toolchain, pin.rust_src_sha256, source_digest)));
    }

    let private = ["crates/mac-abi", "crates/mac-traps"];

    let mut inputs = Map::new();
    inputs.insert("schema".into(), json!(1));
    inputs.insert("builder".into(), json!(BUILDER));
    inputs.insert("compiler_commit".into(), json!(compiler.rustc_commit));
    inputs.insert("cargo_version".into(), json!(compiler.cargo_version));
    inputs.insert("host".into(), json!(compiler.host));
    inputs.insert("upstream_source_sha256".into(), json!(source_digest));
    inputs.insert("port_sha256".into(), json!(digest_tree(&port)?));
    inputs.insert("target_sha256".into(), json!(digest(&target_source)?));
    inputs.insert("patches".into(), json!(pin.patches));

    for directory in private
    {
        inputs.insert(format!("{directory}_sha256"), json!(digest_tree(&root.join(directory))?));
    }

    inputs.insert("manifest_sha256".into(), json!(digest(&root.join("Cargo.toml"))?));
    inputs.insert("lock_sha256".into(), json!(digest(&root.join("Cargo.lock"))?));

    let config = root.join(".cargo/config.toml");

    if config.is_file()
    {
        inputs.insert("cargo_config_sha256".into(), json!(digest(&config)?));
    }

    let key = hex(Sha256::digest(serde_json::to_vec(&Value::Object(inputs.clone()))?));

    let cache = mac_build::paths::build_dir(root).join("sdk");
    let destination = cache.join(&key);
    let receipt = destination.join("prepared.json");

    if !receipt.is_file()
    {
        stage(workspace, &pin, &compiler, &port, &upstream, &target_source, &cache, &destination,
              &key, &inputs)?;
    }
    else
    {
        let recorded: Value = serde_json::from_str(&fs::read_to_string(&receipt)?)?;

        if recorded.get("inputs") != Some(&Value::Object(inputs.clone()))
        {
            return Err(error(format!("{} was prepared from different inputs; remove it and \
                                      rebuild",
                                     destination.display())));
        }

        if recorded.get("patched_source_sha256").and_then(Value::as_str)
           != Some(digest_tree(&destination.join("sysroot/lib/rustlib/src/rust"))?.as_str())
        {
            return Err(error(format!("the prepared source tree under {} was modified after \
                                      preparation; remove it and rebuild",
                                     destination.display())));
        }

        if recorded.get("target_sha256").and_then(Value::as_str)
           != Some(digest(&destination.join(format!("{}.json", pin.target)))?.as_str())
        {
            return Err(error(format!("the prepared target specification under {} was \
                                      modified; remove it and rebuild",
                                     destination.display())));
        }
    }

    let sysroot = destination.join("sysroot");
    let target = destination.join(format!("{}.json", pin.target));

    write_json(&cache.join("current.json"),
               &json!({"key": key,
                       "root": plain(&destination),
                       "sysroot": plain(&sysroot),
                       "target": pin.target,
                       "target_spec": plain(&target),
                       "rustc": plain(&compiler.rustc),
                       "cargo": plain(&compiler.cargo)}))?;

    Ok(Private { root: destination,
                 sysroot,
                 target,
                 rustc: compiler.rustc })
}

#[allow(clippy::too_many_arguments)]
fn stage(workspace: &Workspace, pin: &Pin, compiler: &Compiler, port: &Path, upstream: &Path,
         target_source: &Path, cache: &Path, destination: &Path, key: &str,
         inputs: &Map<String, Value>)
         -> Result<()>
{
    let root = workspace.root();
    let staging = cache.join(format!("{key}.preparing"));

    if staging.exists()
    {
        fs::remove_dir_all(&staging)?;
    }

    fs::create_dir_all(&staging)?;

    let sysroot = staging.join("sysroot");
    let rustlib = sysroot.join("lib/rustlib");
    let copied = rustlib.join("src/rust");

    fs::create_dir_all(&rustlib)?;
    mirror(&compiler.sysroot.join("lib/rustlib"), &rustlib, &["src"])?;
    copy_tree(upstream, &copied)?;

    // A patch path in the pin is relative to the pin itself, so a record that
    // moves its patches moves the paths with it.
    let patches: Vec<PathBuf> = pin.patches.iter().map(|patch| port.join(patch)).collect();

    for patch in &patches
    {
        if !patch.is_file()
        {
            return Err(error(format!("the platform pins patch {} but it does not exist",
                                     patch.display())));
        }
    }

    let changed = patch::apply_all(&copied, &patches)?;

    println!("prepared the private SDK from {} patch(es) touching {} file(s)",
             patches.len(),
             changed.len());

    copy_tree(&port.join("library"), &copied.join("library"))?;
    fs::copy(target_source, staging.join(format!("{}.json", pin.target)))?;

    let package = workspace_package(root)?;

    // The two core-only transport packages resolve into the private workspace
    // under the directory names the patched `library/std/Cargo.toml` already
    // points at. Only their sources are copied: no application facade and no
    // OS implementation joins the private source root.
    for name in ["mac-abi", "mac-traps"]
    {
        let destination = copied.join("library").join(name);

        copy_tree(&root.join("crates").join(name).join("src"), &destination.join("src"))?;
        write_manifest(&destination.join("Cargo.toml"),
                       &name.replace("mac-", "systemrisc-std-"),
                       &package,
                       name == "mac-traps")?;
    }

    write_json(&staging.join("prepared.json"),
               &json!({"schema": 1,
                       "builder": BUILDER,
                       "inputs": inputs,
                       "patched_source_sha256": digest_tree(&copied)?,
                       "target_sha256": digest(&staging.join(format!("{}.json", pin.target)))?,
                       "state": "prepared-sources-only",
                       "std_runtime_qualified": false}))?;

    if destination.exists()
    {
        fs::remove_dir_all(destination)?;
    }

    fs::rename(&staging, destination)?;

    Ok(())
}

/// The version and edition the private transport manifests mirror.
///
/// They are declared once for the whole workspace, so reading them from
/// `[workspace.package]` keeps the private manifests in step with the crates
/// they shadow.
fn workspace_package(root: &Path) -> Result<(String, String)>
{
    let path = root.join("Cargo.toml");
    let text = fs::read_to_string(&path).map_err(|cause| {
                                            error(format!("cannot read {}: {cause}",
                                                          path.display()))
                                        })?;
    let document: toml::Value =
        toml::from_str(&text).map_err(|cause| {
                                 error(format!("{} is not valid TOML: {cause}", path.display()))
                             })?;

    let package =
        document.get("workspace")
                .and_then(|workspace| workspace.get("package"))
                .ok_or_else(|| {
                    error(format!("{} has no [workspace.package] table", path.display()))
                })?;

    let field = |name: &str| -> Result<String> {
        package.get(name)
               .and_then(toml::Value::as_str)
               .map(str::to_string)
               .ok_or_else(|| {
                   error(format!("[workspace.package] in {} has no string `{name}`", path.display()))
               })
    };

    Ok((field("version")?, field("edition")?))
}

/// Write one private transport manifest.
///
/// The two packages are compiled as part of the standard library's own
/// workspace, so they take the in-tree `core` rather than the host's, and the
/// trap entry reaches the ABI crate through the path the standard library
/// already names.
fn write_manifest(path: &Path, name: &str, package: &(String, String), with_abi: bool)
                  -> Result<()>
{
    fs::create_dir_all(path.parent().expect("a manifest path has a parent"))?;

    let mut manifest = format!("[package]\nname = \"{name}\"\nversion = \"{}\"\nedition = \
                                \"{}\"\npublish = false\n\n[dependencies]\ncore = {{ package = \
                                \"rustc-std-workspace-core\", path = \
                                \"../rustc-std-workspace-core\" }}\n",
                               package.0, package.1);

    if with_abi
    {
        manifest.push_str("mac-abi = { package = \"systemrisc-std-abi\", path = \"../mac-abi\" \
                           }\n");
    }

    fs::write(path, manifest)?;

    Ok(())
}

/// Build `std` and friends for the target and install them in the private
/// sysroot.
pub(super) fn build_std(workspace: &Workspace, sdk: &Private) -> Result<()>
{
    let root = workspace.root();
    let target = format!("{}", sdk.target.display());
    let name = sdk.target
                  .file_stem()
                  .ok_or_else(|| error("the platform's std target spec has no file name"))?
                  .to_string_lossy()
                  .to_string();
    let profile = release_profile(root)?;

    let mut command = Command::new("cargo");
    command.current_dir(root)
           .args(["build", "--release", "--locked", "--package", "mac-traps", "--target", &target])
           .arg("--target-dir")
           .arg(root.join("target"))
           .args(["-Zbuild-std=core,alloc,std,panic_abort",
                  "-Zbuild-std-features=compiler-builtins-mem", "-Zjson-target-spec",
                  "--message-format=json-render-diagnostics"])
           .stdout(Stdio::piped());

    for (key, value) in &profile
    {
        command.arg("--config").arg(format!("profile.release.{key}={value}"));
    }

    command.env("RUSTC", env::current_exe()?)
           .env("CARGO_ENCODED_RUSTFLAGS", "")
           .env(PROXY_ENV, &sdk.rustc)
           .env(SYSROOT_ENV, &sdk.sysroot)
           .env("RUSTUP_TOOLCHAIN", &workspace.toolchain);

    for variable in SCRUBBED
    {
        command.env_remove(variable);
    }

    let mut probe = Command::new(env::current_exe()?);
    probe.args(["--print", "sysroot"]).env(PROXY_ENV, &sdk.rustc).env(SYSROOT_ENV, &sdk.sysroot);

    let reported = String::from_utf8_lossy(&probe.output()?.stdout).trim().to_string();

    if Path::new(&reported) != sdk.sysroot
    {
        return Err(error(format!("the compiler proxy reports sysroot {reported} instead of {}",
                                 sdk.sysroot.display())));
    }

    let output =
        command.output().map_err(|cause| error(format!("cannot run the SDK build: {cause}")))?;
    let returncode = output.status.code().unwrap_or(-1);

    if !output.status.success()
    {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));

        return Err(error("the private SDK did not build; see the cargo output above"));
    }

    // Cargo reports the files this invocation actually produced, so the
    // sysroot is filled from that report rather than from a scan of a shared
    // target directory that may still hold an earlier build's output.
    let produced = artifacts(&String::from_utf8_lossy(&output.stdout))?;
    let libraries = install_libraries(sdk, &name, &produced)?;

    write_json(&sdk.root.join("build.json"),
               &json!({"schema": 1,
                       "command": ["cargo", "build", "--release", "--locked", "--package", "mac-traps", "--target", target],
                       "profile": profile.into_iter().collect::<BTreeMap<_, _>>(),
                       "returncode": returncode,
                       "compiler_source_root": plain(&sdk.root),
                       "std_runtime_qualified": false,
                       "libraries": libraries}))?;

    Ok(())
}

fn release_profile(root: &Path) -> Result<Vec<(String, String)>>
{
    let path = root.join("Cargo.toml");
    let text = fs::read_to_string(&path).map_err(|cause| {
                                            error(format!("cannot read {}: {cause}",
                                                          path.display()))
                                        })?;
    let document: toml::Value =
        toml::from_str(&text).map_err(|cause| {
                                 error(format!("{} is not valid TOML: {cause}", path.display()))
                             })?;

    let release = document.get("profile")
                          .and_then(|profile| profile.get("release"))
                          .ok_or_else(|| {
                              error(format!("{} has no [profile.release]; the private SDK needs \
                                             one",
                                            path.display()))
                          })?;

    let table = release.as_table()
                       .ok_or_else(|| {
                           error(format!("[profile.release] in {} is not a table", path.display()))
                       })?;

    let mut values = Vec::new();

    for key in ["panic", "opt-level", "lto", "codegen-units", "debug", "strip"]
    {
        let Some(value) = table.get(key)
        else
        {
            continue;
        };

        values.push((key.to_string(),
                     match value
                     {
                         toml::Value::String(text) => format!("\"{text}\""),
                         other => other.to_string()
                     }));
    }

    Ok(values)
}

/// The library files one cargo invocation reported producing.
///
/// `--message-format=json` names every artifact with its filenames, so the
/// sysroot is filled from what this build produced rather than from whatever
/// happens to sit in a target directory that other builds also use.
fn artifacts(stdout: &str) -> Result<Vec<PathBuf>>
{
    let mut produced = Vec::new();
    for line in stdout.lines()
    {
        let Ok(message) = serde_json::from_str::<Value>(line)
        else
        {
            continue;
        };
        if message["reason"] != "compiler-artifact"
        {
            continue;
        }
        let Some(filenames) = message["filenames"].as_array()
        else
        {
            continue;
        };
        for filename in filenames
        {
            let Some(path) = filename.as_str()
            else
            {
                continue;
            };
            let path = PathBuf::from(path);
            let Some(name) = path.file_name().map(|name| name.to_string_lossy().into_owned())
            else
            {
                continue;
            };
            if name.starts_with("lib") && (name.ends_with(".rlib") || name.ends_with(".rmeta"))
            {
                produced.push(path);
            }
        }
    }
    produced.sort();
    produced.dedup();
    Ok(produced)
}

fn install_libraries(sdk: &Private, target: &str, produced: &[PathBuf]) -> Result<Vec<Value>>
{
    let destination = sdk.sysroot.join("lib/rustlib").join(target).join("lib");

    fs::create_dir_all(&destination).map_err(|cause| {
                                        error(format!("cannot create {}: {cause}",
                                                      destination.display()))
                                    })?;

    let mut found: BTreeMap<String, (PathBuf, String)> = BTreeMap::new();
    for path in produced
    {
        let Some(name) = path.file_name().map(|name| name.to_string_lossy().into_owned())
        else
        {
            continue;
        };
        // The transport packages drive the build; their ordinary Cargo crates
        // are not part of the standard-library sysroot.
        if name.starts_with("libmac_traps") || name.starts_with("libmac_abi")
        {
            continue;
        }
        let sha256 = digest(path)?;
        match found.get(&name)
        {
            Some((other, seen)) if *seen != sha256 =>
            {
                return Err(error(format!("the SDK build reported two different {name} \
                                          libraries: {} and {}",
                                         other.display(),
                                         path.display())));
            }
            Some(_) =>
            {}
            None =>
            {
                found.insert(name, (path.clone(), sha256));
            }
        }
    }

    if found.is_empty()
    {
        return Err(error("the SDK build reported no standard-library libraries"));
    }

    let mut installed = Vec::new();
    for (name, (path, sha256)) in found
    {
        fs::copy(&path, destination.join(&name)).map_err(|cause| {
                                                    error(format!("cannot install {name} in {}: \
                                                                   {cause}",
                                                                  destination.display()))
                                                })?;
        installed.push(json!({"name": name, "sha256": sha256}));
    }

    for suffix in ["rlib", "rmeta"]
    {
        let count = installed.iter()
                             .filter(|library| {
                                 library["name"].as_str().is_some_and(|name| {
                                                             name.starts_with("libstd-")
                                                     && name.ends_with(&format!(".{suffix}"))
                                                         })
                             })
                             .count();
        if count != 1
        {
            return Err(error(format!("the SDK build produced {count} libstd.{suffix} files; \
                                      exactly one is required")));
        }
    }

    Ok(installed)
}

/// Build every application in one cargo invocation and package each.
///
/// Applications share the toolbox and the standard library, so they share a
/// target directory and a compiler process. Packaging is per application
/// because each one has its own `resources.json`.
pub(super) fn applications(workspace: &Workspace, apps: &[App]) -> Result<Vec<Value>>
{
    if apps.is_empty()
    {
        return Ok(Vec::new());
    }

    let sdk = prepare(workspace)?;

    if !sdk.root.join("build.json").is_file()
    {
        build_std(workspace, &sdk)?;
    }

    let target = sdk.target.display().to_string();
    // Application artifacts are only valid against the SDK build that produced
    // them: a changed SDK means a different `core`, and reusing the old
    // directory then fails with "found possibly newer version of crate core".
    // The cache key names the SDK, so a marker beside the artifacts records
    // which one they belong to, and a change clears them. The key is not part
    // of the path: object paths here already run close to Windows' limit, and
    // the extra 64 characters made the host linker fail to open files that
    // exist.
    let sdk_key = sdk.root
                     .file_name()
                     .map(|name| name.to_string_lossy().into_owned())
                     .ok_or_else(|| error("the private SDK has no cache key"))?;
    let target_dir = workspace.target().join("applications");
    let marker = target_dir.join("sdk");
    if fs::read_to_string(&marker).ok().as_deref() != Some(sdk_key.as_str())
    {
        let _ = fs::remove_dir_all(&target_dir);
        fs::create_dir_all(&target_dir)?;
        fs::write(&marker, &sdk_key)?;
    }
    let artifacts = target_dir.join(&target).join("release");
    let mut command = Command::new("cargo");

    command.current_dir(workspace.root())
           .args(["build", "--release", "--locked", "--target", &target, "-Zjson-target-spec",
                  "-Zunstable-options", "--message-format=json-render-diagnostics"])
           .arg("--target-dir")
           .arg(&target_dir)
           .env("RUSTC", env::current_exe()?)
           .env(PROXY_ENV, &sdk.rustc)
           .env(SYSROOT_ENV, &sdk.sysroot)
           .env("RUSTUP_TOOLCHAIN", &workspace.toolchain)
           .env("RUST_TARGET_PATH", sdk.target.parent().expect("the target spec has a parent"))
           .env("CARGO_ENCODED_RUSTFLAGS", "-C\u{1f}relocation-model=pic")
           .env("CARGO_BUILD_RUSTFLAGS", "")
           .env_remove("SR_CARGO_ARTIFACT_DIR")
           .stdout(Stdio::piped());

    for app in apps
    {
        command.args(["--package", &app.package]);
    }

    for feature in features(apps)
    {
        command.args(["--features", &feature]);
    }

    // The scrub list exists so an ambient setting cannot change code
    // generation. The two variables this build sets itself are the exception:
    // they are the application profile, not an inherited one.
    for variable in SCRUBBED
    {
        if !matches!(*variable, "CARGO_TARGET_DIR" | "CARGO_ENCODED_RUSTFLAGS")
        {
            command.env_remove(variable);
        }
    }

    let output =
        command.output()
               .map_err(|cause| error(format!("cannot run cargo for the applications: {cause}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    let executables = executables(&stdout)?;

    if !output.status.success()
    {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));

        return Err(error("the applications did not build; see the cargo output above"));
    }

    let mut packaged = Vec::new();

    for app in apps
    {
        let path = executables.get(&(app.manifest.clone(), app.binary.clone()))
                              .ok_or_else(|| {
                                  error(format!("cargo produced no `{}` executable for {}",
                                                app.binary,
                                                app.manifest.display()))
                              })?;

        packaged.push(package_app(workspace, &app.resources, &app.binary, path, &artifacts)?);
    }

    Ok(packaged)
}

fn features(apps: &[App]) -> BTreeSet<String>
{
    let mut features = BTreeSet::new();

    for app in apps
    {
        for feature in &app.features
        {
            features.insert(format!("{}/{}", app.package, feature));
        }
    }

    features
}

/// The executable paths cargo reported, keyed by the manifest that asked for
/// them and the binary target name.
///
/// A binary name alone is not unique across the workspace, and a shared
/// invocation builds many packages at once.
fn executables(stdout: &str) -> Result<BTreeMap<(PathBuf, String), PathBuf>>
{
    let mut executables = BTreeMap::new();

    for line in stdout.lines()
    {
        let Ok(message) = serde_json::from_str::<Value>(line)
        else
        {
            continue;
        };

        if message.get("reason").and_then(Value::as_str) != Some("compiler-artifact")
        {
            continue;
        }

        let Some(executable) = message.get("executable").and_then(Value::as_str)
        else
        {
            continue;
        };

        let Some(manifest) = message.get("manifest_path").and_then(Value::as_str)
        else
        {
            return Err(error("a cargo artifact message has no manifest path"));
        };

        let Some(name) =
            message.get("target").and_then(|target| target.get("name")).and_then(Value::as_str)
        else
        {
            return Err(error("a cargo artifact message has no target name"));
        };

        executables.insert((PathBuf::from(manifest), name.to_string()), PathBuf::from(executable));
    }

    Ok(executables)
}

/// Package one compiled application into the platform's output directory.
///
/// The receipt is written beside the package so an installed image can be tied
/// back to the exact SDK build that produced it.
fn package_app(workspace: &Workspace, resources: &Path, binary: &str, executable: &Path,
               artifacts: &Path)
               -> Result<Value>
{
    let package = mac_build::package::build_requiring_with_artifacts(
        resources,
        executable,
        workspace.dist(),
        &[mac_formats::requirements::Requirement { class: mac_entropy_interface::CLASS,
                                                   version: mac_entropy_interface::VERSION }],
        artifacts)?;
    let name = package["name"].as_str().ok_or_else(|| error("the package lacks its name"))?;
    let receipt = json!({"state": "packaged-not-executed",
                         "native_execution_observed": false,
                         "binary": binary,
                         "resource_manifest": resources.to_string_lossy(),
                         "elf": executable.to_string_lossy(),
                         "package": package});

    crate::write_json(&workspace.dist().join(format!("{name}.sdk.json")), &receipt)?;
    println!("Packaged std application {name}. Native execution has not been observed.");

    Ok(package)
}

fn resolve(root: &Path, pin: &Pin) -> Result<Compiler>
{
    let which = |tool: &str| -> Result<PathBuf> {
        let output =
            run_output(rustup(root, &["which", "--toolchain", &pin.rustup_toolchain, tool]))?;

        Ok(PathBuf::from(output.trim()))
    };

    let rustc = which("rustc")?;
    let cargo = which("cargo")?;

    let version = run_output({
        let mut command = Command::new(&rustc);
        command.arg("-vV");
        command
    })?;

    let commit = field(&version, "commit-hash").ok_or_else(|| {
                                                   error(format!("{} does not report a commit \
                                                                  hash",
                                                                 rustc.display()))
                                               })?;

    if commit != pin.rustc_commit
    {
        return Err(error(format!("the pinned toolchain {} provides rustc {commit} but the pin \
                                  records {}; install the pinned toolchain or update \
                                  platforms/rust/sdk.json deliberately",
                                 pin.rustup_toolchain, pin.rustc_commit)));
    }

    let host = field(&version, "host").ok_or_else(|| error("rustc does not report a host triple"))?;

    let cargo_version = run_output({
        let mut command = Command::new(&cargo);
        command.arg("-vV");
        command
    })?;

    let cargo_commit = field(&cargo_version, "commit-hash").ok_or_else(|| {
                                                               error("cargo does not report a \
                                                                      commit hash")
                                                           })?;

    if cargo_commit != pin.cargo_commit
    {
        return Err(error(format!("the pinned toolchain {} provides cargo {cargo_commit} but \
                                  the pin records {}",
                                 pin.rustup_toolchain, pin.cargo_commit)));
    }

    let sysroot = PathBuf::from(run_output({
                                    let mut command = Command::new(&rustc);
                                    command.args(["--print", "sysroot"]);
                                    command
                                })?.trim());

    let version = cargo_version.lines().next().unwrap_or_default().to_string();

    Ok(Compiler { rustc,
                  cargo,
                  sysroot,
                  host: host.to_string(),
                  rustc_commit: commit.to_string(),
                  cargo_version: version })
}

fn field<'a>(text: &'a str, name: &str) -> Option<&'a str>
{
    text.lines().find_map(|line| line.strip_prefix(&format!("{name}: ")))
}

fn run_output(mut command: Command) -> Result<String>
{
    let output =
        command.output().map_err(|cause| error(format!("cannot run {command:?}: {cause}")))?;

    if !output.status.success()
    {
        return Err(error(format!("{command:?} failed: {}",
                                 String::from_utf8_lossy(&output.stderr).trim())));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// A content digest of every file under `root`, independent of mtimes.
fn digest_tree(root: &Path) -> Result<String>
{
    let mut files = Vec::new();

    walk(root, &mut files)?;

    let mut sorted: Vec<PathBuf> =
        files.into_iter()
             .filter_map(|path| path.strip_prefix(root).ok().map(Path::to_path_buf))
             .collect();
    sorted.sort();

    let mut hasher = Sha256::new();

    for relative in sorted
    {
        let path = root.join(&relative);
        let name = relative.to_string_lossy().replace('\\', "/");
        let data = fs::read(&path).map_err(|cause| {
                                      error(format!("cannot read {}: {cause}", path.display()))
                                  })?;

        hasher.update((name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update((data.len() as u64).to_le_bytes());
        hasher.update(&data);
    }

    Ok(hex(hasher.finalize()))
}

fn digest(path: &Path) -> Result<String>
{
    let data =
        fs::read(path).map_err(|cause| error(format!("cannot read {}: {cause}", path.display())))?;

    Ok(hex(Sha256::digest(&data)))
}

fn walk(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()>
{
    let Ok(entries) = fs::read_dir(directory)
    else
    {
        return Ok(());
    };

    for entry in entries
    {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;

        if metadata.file_type().is_symlink()
        {
            continue;
        }

        if metadata.is_dir()
        {
            walk(&path, files)?;
        }
        else if metadata.is_file()
        {
            files.push(path);
        }
    }

    Ok(())
}

/// Mirror `from` into `to`, skipping the named entries at the top level.
fn mirror(from: &Path, to: &Path, skip: &[&str]) -> Result<()>
{
    for entry in fs::read_dir(from).map_err(|cause| {
                                       error(format!("cannot read {}: {cause}", from.display()))
                                   })?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();

        if skip.contains(&name.as_str())
        {
            continue;
        }

        let destination = to.join(&name);

        if entry.path().is_dir()
        {
            link_or_copy(&entry.path(), &destination)?;
        }
        else
        {
            fs::copy(entry.path(), &destination)?;
        }
    }

    Ok(())
}

#[cfg(unix)]
fn link_or_copy(from: &Path, to: &Path) -> Result<()>
{
    Ok(std::os::unix::fs::symlink(from, to)?)
}

#[cfg(windows)]
fn link_or_copy(from: &Path, to: &Path) -> Result<()>
{
    match std::os::windows::fs::symlink_dir(from, to)
    {
        Ok(()) => Ok(()),
        Err(_) => copy_tree(from, to)
    }
}

fn copy_tree(from: &Path, to: &Path) -> Result<()>
{
    fs::create_dir_all(to)?;

    for entry in fs::read_dir(from).map_err(|cause| {
                                       error(format!("cannot read {}: {cause}", from.display()))
                                   })?
    {
        let entry = entry?;
        let destination = to.join(entry.file_name());
        let metadata = fs::symlink_metadata(entry.path())?;

        if metadata.is_dir()
        {
            copy_tree(&entry.path(), &destination)?;
        }
        else if metadata.is_file()
        {
            fs::copy(entry.path(), &destination)?;
        }
    }

    Ok(())
}

fn hex(bytes: impl AsRef<[u8]>) -> String
{
    bytes.as_ref().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Strip the Windows extended-length prefix so paths can be handed to cargo,
/// ninja and scripts that do not understand it.
fn plain(path: &Path) -> String
{
    let text = path.display().to_string();

    text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}
