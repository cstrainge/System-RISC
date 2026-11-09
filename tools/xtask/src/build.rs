//! The build itself: what to compile, in what order, and what the checks run.
//!
//! Nothing here names a package. The set to compile comes from the platform
//! record's `install` section plus the package its `bootloader` section names,
//! and each package's binary name, file type and installed folder come from
//! that package's own `resources.json`. Adding an application to a platform is
//! an edit to `platforms/<platform>.json`.

use std::{env, fs, path::Path};

use mac_build::{Result, error, package, platform::install::Set};
use serde_json::{Value, json};

use crate::{Workspace, authority, boot,
            crates::{self, Crate, Crates},
            disk, docs, emulator, modules, ninja, output, run, sdk, write_json};

/// The one command a first-time developer runs: ensure the toolchain, then
/// hand the graph to ninja.
pub fn build(workspace: &Workspace) -> Result<()>
{
    // Name the resolved machine before compiling, so a record edit is visible
    // in the build's own output rather than only in the file that was edited.
    if let Some(emulation) = workspace.platform.emulation()
    {
        println!("Platform {}: {}; {} harts",
                 workspace.platform.name, emulation.description, emulation.harts);
    }
    sdk::prerequisites(workspace)?;
    ninja::run(workspace, &[])
}

/// One stage of the graph. Ninja runs these; a person normally does not.
pub fn stage(workspace: &Workspace, name: &str) -> Result<()>
{
    match name
    {
        "definitions" =>
        {
            // Definitions are resources of the resident System. A platform
            // that installs no guest module has no System to consume them, so
            // there is nothing to compile; building another architecture's
            // definition set would fail on its own RISC-V-only runtime gate.
            let crates = Crates::read(workspace)?;
            let packages = if workspace.platform.install.modules.is_empty()
            {
                Vec::new()
            }
            else
            {
                names(&crates.in_stage(crates::Stage::Definitions))
            };
            require_stage_output(workspace, "definitions", &packages)?;
            guest_build(workspace, &packages)?
        }
        "guest" =>
        {
            let packages = guest_packages(workspace)?;
            require_stage_output(workspace, "guest", &packages)?;
            guest_build(workspace, &packages)?
        }
        "apps" =>
        {
            let apps = applications(workspace)?;
            let packages: Vec<String> = apps.iter().map(|app| app.package.clone()).collect();
            require_stage_output(workspace, "apps", &packages)?;
            sdk::applications(workspace, &apps)?;
        }
        "extract" => extract(workspace)?,
        "package" => package_all(workspace)?,
        "image" =>
        {
            disk::image(workspace)?;
        }
        other => return Err(error(format!("unknown build stage '{other}'")))
    }
    let stamps = workspace.dist().join("ninja").join("stamps");
    fs::create_dir_all(&stamps)?;
    crate::stamp(&stamps.join(name), workspace.dist())
}

/// Build the resources each package takes from the disc.
///
/// A package that has an `extract-list.json` beside its `resources.json` takes
/// resources from the disc that list names, so it also has a generated
/// `extracted_resources.json` that its manifest includes. Producing it here is
/// what keeps a fresh checkout buildable: the include is a build product, so the
/// build makes it. Nothing names System or Finder — a package is a manifest, a
/// list and the outputs beside them.
fn extract(workspace: &Workspace) -> Result<()>
{
    let crates = Crates::read(workspace)?;
    let mut packages: Vec<&crate::crates::Crate> =
        crates.packages_with_extract_list().into_iter().collect();
    packages.sort_by(|left, right| left.relative.cmp(&right.relative));
    for entry in packages
    {
        let list = entry.directory.join("extract-list.json");
        println!("Extracting resources for {} from {}", entry.name, list.display());
        // The extractor's inputs are the list and the disc it names. Whether a
        // resource is the right one to take was settled when the list was
        // authored against our bytes and reviewed, so there is no third check
        // here and no parity failure of its own to turn into a build error.
        mac_extract::extract(&list, false)?;
    }
    Ok(())
}

/// Refuse a stage whose selected package set is empty.
///
/// A stage that produces nothing must not stamp a success. The record says
/// whether the emptiness is intentional (`install.set` is `none`, with a
/// reason) or a defect (`packages` selected none for this stage), and the
/// error names the stage and that declaration.
fn require_stage_output(workspace: &Workspace, stage: &str, packages: &[String]) -> Result<()>
{
    if !packages.is_empty()
    {
        return Ok(());
    }
    let install = &workspace.platform.install;
    let why = match install.set
    {
        Set::None =>
        {
            format!("platform '{}' declares install.set none: {}",
                    workspace.platform.name,
                    install.reason.trim())
        }
        Set::Packages =>
        {
            format!("platform '{}' declares install.set packages but selected no packages for \
                     this stage; add the packages or set install.set to none with a reason",
                    workspace.platform.name)
        }
    };
    Err(error(format!("{stage} stage produced no artifact: {why}")))
}

/// Every package this platform's guest images are compiled from: the installed
/// resident modules, plus the bootloader its record names.
///
/// The bootloader is not installed into the boot volume — it executes in place
/// from read-only flash — so it is not in `install.modules`. It is still a
/// guest package this build must produce, and the record says which one.
fn guest_packages(workspace: &Workspace) -> Result<Vec<String>>
{
    let crates = Crates::read(workspace)?;
    let mut packages: Vec<String> = workspace.platform.install.modules.clone();
    if let Some(bootloader) = &workspace.platform.bootloader
    {
        packages.push(bootloader.package.clone());
    }
    for name in &packages
    {
        crates.get(name)?;
    }
    Ok(packages)
}

fn names(entries: &[&Crate]) -> Vec<String>
{
    entries.iter().map(|entry| entry.name.clone()).collect()
}

/// Compile guest images in one cargo invocation.
///
/// One invocation, so the shared crates are compiled once for every package
/// that needs them rather than once per target directory, and so cargo's
/// feature unification sees the whole set at once.
fn guest_build(workspace: &Workspace, packages: &[String]) -> Result<()>
{
    if packages.is_empty()
    {
        return Ok(());
    }
    let crates = Crates::read(workspace)?;
    // Resolving the whole set once names the package a bad record points at,
    // before cargo is asked to compile anything.
    let entries = crates.select(packages)?;
    let mut command = workspace.cargo();
    command.args(["build", "--locked", "--release", "--target", &workspace.platform.rust_target,
                  "--target-dir"])
           .arg(workspace.target());
    // The instruction profile is the platform record's, turned into a flag here
    // rather than repeated in .cargo/config.toml.
    mac_build::platform::configure_for(&workspace.platform, &mut command);
    for package in packages
    {
        command.args(["--package", package]);
    }
    for entry in &entries
    {
        let package = &entry.name;
        // The stage and the `guest` feature are checked against each other
        // when the graph is read, so this is one fact, not a second guess.
        if entry.is_guest()
        {
            command.arg("--features").arg(format!("{package}/guest"));
        }
        for feature in features(workspace, package)
        {
            command.arg("--features").arg(format!("{package}/{feature}"));
        }
    }
    run(&mut command)
}

/// A diagnostic a caller enables, rather than one a platform ships.
///
/// Tracing is not platform data: a record that enabled it would turn it on for
/// everyone who built that platform, and the shipped set would stop being the
/// thing the record describes. So these are reached either by `cargo xtask
/// check`, or by the environment variable named beside them, and they live in
/// one table so a package cannot be traced by two different rules.
struct Diagnostic
{
    package:  &'static str,
    feature:  &'static str,
    variable: &'static str,
    /// Whether `cargo xtask check` enables it without the variable being set.
    checking: bool
}

const DIAGNOSTICS: &[Diagnostic] = &[Diagnostic { package:  "mac-system",
                                                  feature:  "event-trace",
                                                  variable: "SYSTEM7_EVENT_TRACE",
                                                  checking: true },
                                     Diagnostic { package:  "mac-system",
                                                  feature:  "display-event-debug",
                                                  variable: "SYSTEM7_DISPLAY_EVENT_DEBUG",
                                                  checking: false },
                                     Diagnostic { package:  "text-edit",
                                                  feature:  "input-trace",
                                                  variable: "SR_TEXTEDIT_INPUT_TRACE",
                                                  checking: true }];

/// Every feature `package` is built with: the record's, then any diagnostic the
/// caller asked for. The record's list is the shipped set; the diagnostics are
/// never part of it.
fn features(workspace: &Workspace, package: &str) -> Vec<String>
{
    let mut selected: Vec<String> = workspace.platform.install.features_for(package).to_vec();
    for diagnostic in DIAGNOSTICS.iter().filter(|row| row.package == package)
    {
        let enabled = (diagnostic.checking && workspace.checking)
                      || env::var_os(diagnostic.variable).is_some();
        if enabled && !selected.iter().any(|feature| feature == diagnostic.feature)
        {
            selected.push(diagnostic.feature.to_string());
        }
    }
    if !selected.is_empty()
    {
        println!("{package}: features {}", selected.join(", "));
    }
    selected
}

/// The applications this platform installs, with the features it asks for.
fn applications(workspace: &Workspace) -> Result<Vec<sdk::App>>
{
    let crates = Crates::read(workspace)?;
    let mut apps = Vec::new();
    for name in &workspace.platform.install.applications
    {
        let entry = crates.get(name)?;
        if entry.stage != crates::Stage::Application
        {
            return Err(error(format!("platform '{}' installs '{name}' as an application, but \
                                      that package is a {}",
                                     workspace.platform.name,
                                     entry.stage.describe())));
        }
        let resources = entry.resources()?;
        let features = features(workspace, name);
        let manifest =
            entry.manifest
                 .clone()
                 .ok_or_else(|| error(format!("application '{name}' has no resources.json")))?;
        apps.push(sdk::App { package: name.clone(),
                             manifest: entry.directory.join("Cargo.toml"),
                             resources: manifest,
                             binary: resources.binary,
                             features });
    }
    Ok(apps)
}

/// Package every compiled module and record what was built.
pub fn package_all(workspace: &Workspace) -> Result<()>
{
    let crates = Crates::read(workspace)?;
    let installed = crates::installed(workspace, &crates)?;
    let mut selected: Vec<String> = installed.iter().map(|entry| entry.name.clone()).collect();
    if let Some(bootloader) = &workspace.platform.bootloader
    {
        selected.push(bootloader.package.clone());
    }
    require_stage_output(workspace, "package", &selected)?;
    let _ = boot::package_rom(workspace)?;
    let mut reports = Vec::new();
    for entry in &installed
    {
        let package = crates.get(&entry.package)?;
        // The application stage already packaged these, with the SDK's own
        // linker output and its receipt; packaging them twice would produce two
        // different files under one name.
        if package.stage == crates::Stage::Application
        {
            continue;
        }
        let resources = package.resources()?;
        let executable = workspace.target()
                                  .join(&workspace.platform.rust_target)
                                  .join("release")
                                  .join(&resources.binary);
        let report = package::build(&resources.path, &executable, workspace.dist())?;
        println!("Packaged {} ({}): entry 0x{:x}, {} resources",
                 entry.name,
                 resources.role,
                 report["entry"].as_u64().unwrap_or_default(),
                 report["resources"].as_array().map(Vec::len).unwrap_or_default());
        reports.push(report);
    }
    disk::validate_modules(workspace)?;
    // The receipts carry the role beside each report; packaging only needs the
    // report itself.
    reports.extend(application_reports(workspace)?.into_iter().map(|(_, report)| report));
    // The application authority policy is a resource of the resident System.
    // A platform that installs no System has no policy to bind and no
    // System.bin to attach it to.
    if reports.iter().any(|report| report["role"] == "system")
    {
        authority::install(workspace, &mut reports)?;
    }
    // No pairwise preferred-placement exclusion here, and that is a resolved
    // merge rather than a deletion. It used to compare every pair of mappings
    // and refuse overlapping reservations, excluding `code_resource`. User-mode
    // application isolation changed the model underneath it: an application is
    // now linked at the checked private Process image origin and mapped into its
    // own Process root, so every application shares the same base by design -
    // all eight report base 0x400000 - and the old rule rejected a correct
    // build. `system` is mapped once, so it has no pair, and `code_resource`
    // already shared origins. The overlap question is answered where it belongs
    // now, by the per-Process image geometry
    // `virtual_memory()?.application_image_range()` and the loader's own
    // per-root checks.
    let capacity = workspace.platform.virtual_memory()?.module_capacity();
    let module_bytes = modules::footprint(&reports, capacity)?;
    println!("Resident module execution spans: {module_bytes} of {capacity} virtual bytes; \
              payload pages admitted at runtime");
    let emulation = workspace.platform.emulation();
    let report = json!({"platform": workspace.platform.name,
                        "target": &workspace.platform.rust_target,
                        "harts": emulation.map(|emulation| emulation.harts.get()),
                        "machine": emulation.map(|emulation| emulation.description.as_str()),
                        "guest_profile": "release",
                        "build_dir": workspace.describe(&workspace.build),
                        "toolchain": {"cargo": output(workspace.cargo().arg("--version"))?,
                                      "rustc": output(&mut workspace.rustc())?},
                        "packages": reports});
    write_json(&workspace.dist().join("build.json"), &report)
}

/// The reports the application stage left behind, with the kind each package
/// declares, in a stable order.
fn application_reports(workspace: &Workspace) -> Result<Vec<(String, Value)>>
{
    let mut receipts = Vec::new();
    for entry in fs::read_dir(workspace.dist())?
    {
        let path = entry?.path();
        if path.to_string_lossy().ends_with(".sdk.json")
        {
            receipts.push(path);
        }
    }
    receipts.sort();
    let mut reports = Vec::new();
    for path in receipts
    {
        let receipt: Value = serde_json::from_slice(&fs::read(&path)?)?;
        let manifest = receipt["resource_manifest"].as_str().ok_or_else(|| {
                                                                 error(format!("{} records no \
                                                                                resource manifest",
                                                                               path.display()))
                                                             })?;
        let role = mac_build::Manifest::read(Path::new(manifest))?.role;
        reports.push((role, receipt["package"].clone()));
    }
    Ok(reports)
}

/// The package reports the packaging stage recorded.
pub fn packages(workspace: &Workspace) -> Result<Vec<Value>>
{
    let path = workspace.dist().join("build.json");
    let report: Value = serde_json::from_slice(&fs::read(&path)?)?;
    Ok(report["packages"].as_array().cloned().unwrap_or_default())
}

/// Build the declared bootloader and package it as the platform's boot ROM.
pub fn rom(workspace: &Workspace) -> Result<()>
{
    let Some(bootloader) = workspace.platform.bootloader.clone()
    else
    {
        println!("Platform '{}' declares no bootloader; no boot payload is produced.",
                 workspace.platform.name);
        return Ok(());
    };
    // Only this package is selected, and it declares no features, so this
    // cannot disturb the feature set the shared guest build resolves.
    guest_build(workspace, std::slice::from_ref(&bootloader.package))?;
    let _ = boot::package_rom(workspace)?;
    Ok(())
}

/// Build, then boot the platform in its emulator.
pub fn run_platform(workspace: &Workspace, mode: emulator::Mode, disk: Option<&str>,
                    extra: &[String])
                    -> Result<()>
{
    build(workspace)?;
    let media = match disk
    {
        Some(path) => vec![Path::new(path).canonicalize()?],
        None => Vec::new()
    };
    boot::run(workspace, mode, &media, extra)
}

/// Remove the writable disk copies a check leaves behind.
///
/// A check that fails part way through must not strand them, so the caller runs
/// this whether or not the check succeeded.
pub fn prune(workspace: &Workspace) -> Result<()>
{
    python(workspace, &["scripts/prune_system7_images.py"])
}

/// Build one std application with the private SDK, selected by its manifest.
///
/// This is the SDK's own acceptance route: it takes a `Cargo.toml` or the
/// `resources.json` beside it, and reports the package it produced.
pub fn sdk_app(workspace: &Workspace, manifest: &str, features: &[String]) -> Result<()>
{
    let path = crate::workspace::absolute(workspace.root(), Path::new(manifest).to_path_buf());
    let directory =
        path.parent().ok_or_else(|| error(format!("'{manifest}' has no parent directory")))?;
    let cargo_toml = directory.join("Cargo.toml");
    let text = fs::read_to_string(&cargo_toml).map_err(|why| {
                                                  error(format!("cannot read {}: {why}",
                                                                cargo_toml.display()))
                                              })?;
    // `str::parse` reads one TOML value, not a document, so a manifest must go
    // through `toml::from_str` or every `[package]` line is rejected.
    let declared: toml::Value = toml::from_str(&text).map_err(|why| {
                                                         error(format!("cannot parse {}: {why}",
                                                                       cargo_toml.display()))
                                                     })?;
    let package = declared["package"]["name"].as_str()
                                             .ok_or_else(|| {
                                                 error(format!("{} does not name a package",
                                                               cargo_toml.display()))
                                             })?
                                             .to_string();
    let crates = Crates::read(workspace)?;
    let entry = crates.get(&package)?;
    if entry.stage != crates::Stage::Application
    {
        return Err(error(format!("'{package}' is a {}, not a native application",
                                 entry.stage.describe())));
    }
    let resources = entry.resources()?;
    let app = sdk::App { package,
                         manifest: cargo_toml,
                         resources: resources.path,
                         binary: resources.binary,
                         features: features.to_vec() };
    for report in sdk::applications(workspace, &[app])?
    {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}

/// The host, disk, ROM boot and desktop check pipeline.
///
/// The check scripts under `scripts/` are outside this rewrite: they are host
/// capture and comparison tools, not part of the build. This function is the
/// driver that runs them, and every report path below is where the script
/// writes it, under `results/`.
pub fn check(workspace: &Workspace) -> Result<()>
{
    let results = workspace.results();
    python(workspace, &["-m", "unittest", "discover", "-s", "scripts/tests", "-p", "test_*.py"])?;
    python(workspace, &["scripts/prune_system7_images.py"])?;
    run(workspace.cargo().args(["fmt", "--all", "--", "--check"]))?;
    docs::generate(workspace)?;
    build(workspace)?;
    let packages = packages(workspace)?;
    python(workspace, &["scripts/check_system7_boundaries.py"])?;
    let boundaries = read(results, "system7/toolbox-boundaries.json")?;
    let image = disk::image(workspace)?;
    let boot = boot::check(workspace)?;
    python(workspace,
           &["scripts/system7_vm.py",
             "check",
             "--no-build",
             "--build-dir",
             &workspace.build.to_string_lossy()])?;
    let desktop = read(results, "system7/desktop-checks.json")?;
    python(workspace, &["scripts/check_wdef_pixels.py"])?;
    let appearance = read(results, "system7/wdef-appearance-checks.json")?;
    let mut quickdraw = serde_json::Map::new();
    for (name, script, report) in
        [("primitives",
          "scripts/check_quickdraw_primitives.py",
          "quickdraw-reference/primitive-checks.json"),
         ("fonts", "scripts/check_quickdraw_fonts.py", "system7/font-comparison-checks.json"),
         ("icons", "scripts/check_quickdraw_icons.py", "quickdraw-reference/icon-checks.json"),
         ("resources",
          "scripts/check_system7_resources.py",
          "system7/resource-manager-checks.json")]
    {
        python(workspace, &[script])?;
        quickdraw.insert(name.into(), read(results, report)?);
    }
    let mut finder = serde_json::Map::new();
    for (name, script, report) in
        [("icons", "scripts/check_finder_icons.py", "system7/finder-icon-checks.json"),
         ("mounted_volumes",
          "scripts/check_finder_volumes.py",
          "system7/finder-volume-checks.json")]
    {
        python(workspace, &[script])?;
        finder.insert(name.into(), read(results, report)?);
    }
    python(workspace, &["scripts/check_system7_files.py", "--skip-clippy"])?;
    let filesystem = read(results, "system7/filesystem-checks.json")?;
    let event_manager = read(results, "system7/event-manager-checks.json")?;
    let clipboard = read(results, "system7/clipboard-checks.json")?;
    let textedit = read(results, "system7/textedit-graphical-checks.json")?;
    let typing = read(results, "system7/typing-checks.json")?;
    let processes = read(results, "system7/process-checks.json")?;
    let application_ui = read(results, "system7/application-ui-checks.json")?;
    let retained_update = read(results, "system7/retained-update-checks.json")?;
    let event_latency = read(results, "system7/event-latency-checks.json")?;
    let cursors = read(results, "system7/cursor-checks.json")?;
    let panics = read(results, "system7/panic-checks.json")?;
    let report = json!({
        "platform": workspace.platform.name, "target": &workspace.platform.rust_target,
        "packages": packages, "toolbox_boundaries": boundaries, "disk": image,
        "event_manager": event_manager, "clipboard": clipboard, "textedit": textedit,
        "typing": typing, "processes": processes, "application_ui": application_ui,
        "retained_update": retained_update, "event_latency": event_latency,
        "cursors": cursors, "panics": panics, "boot": boot, "desktop": desktop,
        "wdef_appearance": appearance, "quickdraw": quickdraw, "filesystem": filesystem,
        "finder": finder,
        "scope": "Native resource-fork System/Finder, flash ROM boot from blessed HFS-B, \
                  resource-loaded definitions, resident QuickDraw/fonts/icons/pictures, \
                  resource menus/alerts, captured Event/Scrap and monochrome TextEdit services \
                  with Finder exercisers, shared native HFS-B read/write services with UTF-8 \
                  names and microsecond dates, unchanged classic HFS exchange access, and \
                  read-only resource-fork inspection. Original visual comparisons are strict \
                  for primitives/icons/bitmap fonts; TrueType and synthetic styles retain \
                  measured pixel differences. Original 68k execution and full classic Toolbox \
                  remain incomplete."
    });
    let path = results.join("system7/toolchain-checks.json");
    write_json(&path, &report)?;
    println!("System/Finder toolchain checks passed: {}", path.display());
    Ok(())
}

fn read(results: &Path, relative: &str) -> Result<Value>
{
    Ok(serde_json::from_slice(&fs::read(results.join(relative))?)?)
}

/// Run a host check script.
///
/// The check pipeline is not part of this rewrite, and those scripts need a
/// Python 3 interpreter. `SR_PYTHON` selects one; otherwise the name that works
/// on this host is tried. The build path never comes through here.
fn python(workspace: &Workspace, arguments: &[&str]) -> Result<()>
{
    let program =
        env::var_os("SR_PYTHON").unwrap_or_else(|| {
                                    if cfg!(windows) { "python".into() } else { "python3".into() }
                                });
    run(workspace.command(&program).current_dir(workspace.root()).args(arguments))
}
