//! The build graph.
//!
//! Ninja is the scheduler, not the implementation: every rule runs `xtask`
//! itself, so the work stays in one Rust program while ninja owns the question
//! "what is out of date". Each stage writes a stamp whose content digests the
//! files that stage produced, and the rule is `restat`, so a stage that
//! rebuilt nothing leaves its dependents alone. That is what makes the image
//! depend on the applications it contains rather than on the build happening.
//!
//! The graph is generated from the platform record and the workspace, never
//! edited by hand, and it lives inside the platform's output directory so
//! ninja's log and dependency files land there too.
use std::{fs,
          io::Write,
          path::{Path, PathBuf}};

use mac_build::{Result, error};
use serde_json::json;

use crate::{Workspace,
            crates::{Crates, Stage}};

/// A path as ninja spells it: forward slashes, with the characters that are
/// syntax escaped.
fn path(path: &Path) -> String
{
    let text = path.to_string_lossy().replace('\\', "/");
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars()
    {
        match character
        {
            '$' => escaped.push_str("$$"),
            ' ' => escaped.push_str("$ "),
            ':' => escaped.push_str("$:"),
            other => escaped.push(other)
        }
    }
    escaped
}

/// The directory holding the graph, its log and its dependency files.
fn directory(workspace: &Workspace) -> PathBuf
{
    workspace.dist().join("ninja")
}

/// Generate the graph and return its path.
pub fn generate(workspace: &Workspace) -> Result<PathBuf>
{
    let directory = directory(workspace);
    let stamps = directory.join("stamps");
    fs::create_dir_all(&stamps)?;
    let crates = Crates::read(workspace)?;
    let dist = workspace.dist();
    let mut text = String::new();
    text.push_str("ninja_required_version = 1.10\n");
    text.push_str(&format!("builddir = {}\n", path(&directory)));
    let xtask = format!("\"{}\"", std::env::current_exe()?.display());
    // The stage children inherit the output directory through `SR_DIST`, which
    // names the platform's directory itself. Passing `--dist` as well would
    // name a root and have the child append the platform to it a second time.
    let options = format!("--platform {} --target-dir \"{}\"",
                          workspace.platform.name,
                          workspace.target().display());
    text.push_str(&format!("xtask = {xtask} {options}\n"));
    text.push_str("\nrule definitions\n  command = $xtask stage definitions\n  description = \
                   definitions\n  restat = 1\n");
    text.push_str("\nrule guest\n  command = $xtask stage guest\n  description = system and \
                   modules\n  restat = 1\n");
    text.push_str("\nrule apps\n  command = $xtask stage apps\n  description = applications\n  \
                   restat = 1\n");
    text.push_str("\nrule extract\n  command = $xtask stage extract\n  description = extracted \
                   resources\n  restat = 1\n");
    text.push_str("\nrule package\n  command = $xtask stage package\n  description = packages \
                   and boot payload\n  restat = 1\n");
    text.push_str("\nrule image\n  command = $xtask stage image\n  description = storage \
                   image\n  restat = 1\n");
    text.push_str("\nrule boot\n  command = $xtask run --no-build\n  description = boot\n");

    // The installed set is the platform record's, so a record that installs a
    // different application changes this graph without touching this file.
    let installed = &workspace.platform.install;
    let definitions = packages(&crates, Stage::Definitions);
    let mut guest = installed.modules.clone();
    if let Some(bootloader) = &workspace.platform.bootloader
    {
        guest.push(bootloader.package.clone());
    }
    let apps = installed.applications.clone();
    let definitions = borrow(&definitions);
    let guest = borrow(&guest);
    let apps = borrow(&apps);
    // Extraction feeds every stage that compiles a package whose manifest
    // includes generated resources: the include must exist before the package's
    // build script reads the manifest. So the graph orders it first.
    let extracted = extraction(workspace, &crates, &stamps)?;
    let extract = path(&stamps.join("extract"));
    text.push_str(&format!("\nbuild {extract}: extract | {extracted}\n"));
    for (name, roots) in [("definitions", &definitions), ("guest", &guest), ("apps", &apps)]
    {
        let mut inputs = source_files(workspace, &crates, roots)?;
        inputs.extend(platform_records(workspace)?);
        inputs.push(mac_build::platform::compiler_path(workspace.root(), &workspace.platform)?);
        if name == "apps"
        {
            inputs.extend(crate::crates::files(&workspace.root().join("platforms/rust"))?);
            // SDK builder changes can select a new sysroot without changing
            // the applications' own sources or the port overlay.
            inputs.push(workspace.root().join("tools/xtask/src/sdk.rs"));
        }
        inputs.sort();
        inputs.dedup();
        let inputs = inputs.iter().map(|input| path(input)).collect::<Vec<_>>().join(" ");
        let order = if name == "definitions" { String::new() } else { format!(" || {extract}") };
        text.push_str(&format!("\nbuild {}: {name} | {inputs}{order}\n", path(&stamps.join(name))));
    }
    // The package stage consumes what the compile stages produced, plus the
    // resource manifests it packages and the platform record that names the
    // installed set.
    let mut packaged = Vec::new();
    for package in installed.packages()
    {
        let entry = crates.get(package)?;
        if let Some(manifest) = &entry.manifest
        {
            packaged.push(manifest.clone());
        }
    }
    if let Some(bootloader) = &workspace.platform.bootloader
    {
        let entry = crates.get(&bootloader.package)?;
        if let Some(manifest) = &entry.manifest
        {
            packaged.push(manifest.clone());
        }
    }
    packaged.extend(platform_records(workspace)?);
    packaged.sort();
    packaged.dedup();
    let inputs = packaged.iter().map(|input| path(input)).collect::<Vec<_>>().join(" ");
    // Extraction feeds packaging: the generated include is a build product, so
    // the graph orders it rather than asking a developer to run it.
    text.push_str(&format!("\nbuild {}: package | {} {} {} {} {inputs}\n",
                           path(&stamps.join("package")),
                           path(&stamps.join("definitions")),
                           path(&stamps.join("guest")),
                           path(&stamps.join("apps")),
                           path(&stamps.join("extract"))));
    match image(workspace)?
    {
        Some(image) =>
        {
            let image = path(&dist.join(image));
            // The stage writes two things: the medium and its stamp. Declaring
            // both means deleting either one reruns the stage, and the stamp is
            // tracked the same way the other four are.
            text.push_str(&format!("\nbuild {} {image}: image | {}\n",
                                   path(&stamps.join("image")),
                                   path(&stamps.join("package"))));
            text.push_str(&format!("\nbuild boot: boot | {image}\n"));
            text.push_str(&format!("\ndefault {image}\n"));
        }
        None =>
        {
            // A platform with no machine attached still builds its packages;
            // saying so keeps the default target from being empty.
            text.push_str(&format!("\ndefault {}\n", path(&stamps.join("package"))));
        }
    }
    let file = directory.join("build.ninja");
    let mut output = fs::File::create(&file)?;
    output.write_all(text.as_bytes())?;
    Ok(file)
}

fn borrow<'a>(names: &'a [String]) -> Vec<&'a str>
{
    names.iter().map(String::as_str).collect()
}

/// The packages a stage of the tree contains, as Cargo package names.
fn packages(crates: &Crates, stage: Stage) -> Vec<String>
{
    crates.in_stage(stage).iter().map(|entry| entry.name.clone()).collect()
}

/// The storage image a bootable platform attaches to its machine.
///
/// A card image is the board's deliverable, not an emulator's: a platform flashed
/// and booted by its own firmware needs one just as much as a machine that attaches
/// it to an emulated disk, and more so, because there is no other way to run it.
/// Which medium the platform is given is a record fact — `hardware.boot_image` names
/// it — so the image stage follows the record rather than the presence of an
/// emulator. Before this, deleting an emulation section silently removed the image
/// from the build graph and left a successful build with no card to write.
fn image(workspace: &Workspace) -> Result<Option<String>>
{
    let Some(storage) = workspace.platform.storage.as_ref()
    else
    {
        return Ok(None);
    };
    if workspace.platform
                .hardware
                .as_ref()
                .and_then(|hardware| hardware.boot_image.as_deref())
                .is_none()
       && workspace.platform.emulation().is_none()
    {
        return Ok(None);
    }
    let name = workspace.boot_image_name()?;
    let image = storage.image(&name).ok_or_else(|| {
                                         error(format!("platform '{}' storage.images has no \
                                                        image named '{name}'",
                                                       workspace.platform.name))
                                     })?;
    Ok(Some(image.file(&name)))
}

/// Every source file of `roots` and everything they depend on, plus the
/// workspace manifests that decide how they compile.
fn source_files(workspace: &Workspace, crates: &Crates, roots: &[&str]) -> Result<Vec<PathBuf>>
{
    let mut files = Vec::new();
    for entry in crates.closure(roots)?
    {
        files.extend(crate::crates::files(&entry.directory)?);
    }
    for shared in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml"]
    {
        let path = workspace.root().join(shared);
        if path.is_file()
        {
            files.push(path);
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// The inputs of the extraction step: every package's extract list and the disc
/// it names, plus the extractor's own sources so changing it re-runs the step.
///
/// A list names its disc by filename in the repository's `iso` directory, so a
/// clone without the disc fails here, naming the disc and where to put it,
/// rather than much later in a package that cannot find its include.
fn extraction(workspace: &Workspace, crates: &Crates, stamps: &Path) -> Result<String>
{
    let mut packages: Vec<&crate::crates::Crate> = crates.packages_with_extract_list();
    packages.sort_by(|left, right| left.relative.cmp(&right.relative));
    let mut inputs = Vec::new();
    for entry in &packages
    {
        let manifest = entry.manifest.as_ref().ok_or_else(|| {
                                                   error(format!("package '{}' has an extract \
                                                                  list but no resources.json",
                                                                 entry.name))
                                               })?;
        let list = manifest.with_file_name("extract-list.json");
        let parsed: mac_extract::ExtractList =
            serde_json::from_slice(&fs::read(&list).map_err(|cause| {
                                                       error(format!("cannot read {}: {cause}",
                                                                     list.display()))
                                                   })?).map_err(|cause| {
                                                           error(format!("{} is not an extract \
                                                                          list: {cause}",
                                                                         list.display()))
                                                       })?;
        let disc = workspace.root().join(mac_extract::DISC_DIRECTORY).join(&parsed.iso);
        if !disc.is_file()
        {
            return Err(error(format!("package '{}' takes its resources from the {}, and that \
                                      disc is not present: expected {} in the repository's {} \
                                      directory ({}). Download the {}, save it under exactly \
                                      that filename there, and run the build again; it \
                                      continues once the disc is in place",
                                     entry.name,
                                     parsed.description,
                                     parsed.iso,
                                     mac_extract::DISC_DIRECTORY,
                                     disc.display(),
                                     parsed.description)));
        }
        inputs.push(list);
        inputs.push(disc);
    }
    // The extractor is an input: changing it must re-run the step.
    if let Ok(entry) = crates.get("mac-extract")
    {
        inputs.extend(crate::crates::files(&entry.directory)?);
    }
    inputs.sort();
    inputs.dedup();
    let _ = stamps;
    Ok(inputs.iter().map(|input| path(input)).collect::<Vec<_>>().join(" "))
}

/// The platform record and everything it includes.
fn platform_records(workspace: &Workspace) -> Result<Vec<PathBuf>>
{
    mac_build::platform::record_files(&workspace.platform.name)
}

/// Resolve ninja, naming the install command when it is missing.
pub fn ensure() -> Result<PathBuf>
{
    crate::tools::resolve("ninja")
}

/// Run the graph. Without targets ninja builds the default target.
pub fn run(workspace: &Workspace, targets: &[String]) -> Result<()>
{
    let ninja = ensure()?;
    let file = generate(workspace)?;
    // Ninja's own children inherit this process's environment, and they do not
    // run in the checkout. They therefore need the same platform, build and
    // root this build selected, not whatever the operator happened to export:
    // a child that falls back to the default platform builds the wrong
    // architecture and reports success.
    let mut command = workspace.command(&ninja);
    command.env(crate::LEASE_ENV, workspace.dist()).arg("-f").arg(&file);
    // An explicitly serial Cargo build also needs serial compile stages.
    if std::env::var("CARGO_BUILD_JOBS").as_deref() == Ok("1")
    {
        command.args(["-j", "1"]);
    }
    for target in targets
    {
        command.arg(target);
    }
    crate::run(&mut command)?;
    write_report(workspace, &ninja, &file)
}

/// Record what this build produced. Every path in the report is relative to
/// the checkout, so reports from different platforms compare directly.
fn write_report(workspace: &Workspace, ninja: &Path, graph: &Path) -> Result<()>
{
    let dist = workspace.dist();
    let root = workspace.root();
    let mut artifacts = Vec::new();
    for entry in fs::read_dir(dist)?
    {
        let entry = entry?;
        if entry.file_name() == "ninja"
        {
            continue;
        }
        artifacts.push(relative(root, &entry.path()));
    }
    artifacts.sort();
    let report = json!({
        "platform": workspace.platform.name,
        "root": root.display().to_string(),
        "toolchain": workspace.toolchain,
        "output": relative(root, dist),
        "build": relative(root, &workspace.build),
        "target": relative(root, &workspace.target),
        "results": relative(root, workspace.results()),
        "ninja": {
            "program": relative(root, ninja),
            "graph": relative(root, graph),
        },
        "artifacts": artifacts,
    });
    fs::write(dist.join("build-report.json"), serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}

fn relative(root: &Path, path: &Path) -> String
{
    path.strip_prefix(root).unwrap_or(path).display().to_string()
}
