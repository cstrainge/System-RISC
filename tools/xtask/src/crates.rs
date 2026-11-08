//! The workspace crate graph, read from Cargo itself.
//!
//! The build never holds a list of packages. A package's directory says which
//! stage it belongs to, its `guest` feature says whether it is a no_std guest
//! image, and its `resources.json` says what it installs as. Cargo is the
//! authority for all of that, so adding a driver, an extension or an
//! application needs no edit here.
//!
//! The platform record chooses *which* of these packages a machine installs;
//! see `mac_build::platform::install`. Selection is platform data, properties
//! are crate data.
//!
//! Reading the graph deliberately does not read any `resources.json`. Asking
//! "what packages exist" must not also ask "is every manifest in the workspace
//! valid", or one package mid-edit — or a host tool the platform never
//! installs — fails every build. A manifest is read only for a package the
//! platform actually installs, so the error names the package it was asked for.

use std::{collections::{BTreeMap, BTreeSet},
          fs,
          path::{Path, PathBuf},
          process::Command};

use mac_build::{Result, error};

use crate::Workspace;

/// What a package's `resources.json` declares, once it has been read.
pub struct Resources
{
    pub path:   PathBuf,
    pub role:   String,
    pub name:   String,
    pub binary: String
}

/// Which build stage a package's sources belong to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage
{
    /// Relocatable definitions. Each packages itself from its own build script.
    Definitions,
    /// The resident System, drivers and extensions.
    Guest,
    /// Native applications, built against the private SDK.
    Application,
    /// Host tooling. Built by whoever needs it, not installed on a machine.
    Host
}

impl Stage
{
    /// What this stage is, for an error that has to name the contradiction.
    pub fn describe(self) -> &'static str
    {
        match self
        {
            Stage::Definitions => "definition",
            Stage::Guest => "guest module",
            Stage::Application => "native application",
            Stage::Host => "host tool"
        }
    }
}

pub struct Crate
{
    pub name:         String,
    /// Directory relative to the checkout with forward slashes, `apps/finder`.
    pub relative:     String,
    pub directory:    PathBuf,
    /// The package's `resources.json`, when it has one.
    pub manifest:     Option<PathBuf>,
    pub stage:        Stage,
    /// Path dependencies, by package name.
    pub dependencies: Vec<String>
}

impl Crate
{
    /// Whether this package is a no_std guest image. That is the same fact as
    /// its stage: the `guest` feature and the directory it lives in are
    /// checked against each other when the graph is read, so one predicate
    /// follows from the other rather than competing with it.
    pub fn is_guest(&self) -> bool
    {
        matches!(self.stage, Stage::Definitions | Stage::Guest)
    }

    /// Read this package's `resources.json`, naming the package when it is
    /// missing or malformed.
    pub fn resources(&self) -> Result<Resources>
    {
        let path = self.manifest.as_ref().ok_or_else(|| {
                                              error(format!("package '{}' is installed by a \
                                                             platform but has no resources.json",
                                                            self.name))
                                          })?;
        let manifest = mac_build::Manifest::read(path).map_err(|why| {
                                                          error(format!("package '{}': {why}",
                                                                        self.name))
                                                      })?;
        Ok(Resources { path:   path.clone(),
                       binary: manifest.binary_name().to_string(),
                       role:   manifest.role,
                       name:   manifest.name })
    }
}

/// The workspace graph, indexed by package name.
pub struct Crates
{
    packages: BTreeMap<String, Crate>
}

impl Crates
{
    pub fn read(workspace: &Workspace) -> Result<Self>
    {
        let mut command: Command = workspace.cargo();
        let output =
            command.args(["metadata", "--no-deps", "--format-version", "1", "--locked"]).output()?;
        if !output.status.success()
        {
            return Err(error(format!("cannot read the workspace crate graph:\n{}",
                                     String::from_utf8_lossy(&output.stderr))));
        }
        let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let members: BTreeSet<&str> = metadata["workspace_members"].as_array()
                                                                   .into_iter()
                                                                   .flatten()
                                                                   .filter_map(|id| id.as_str())
                                                                   .collect();
        // Directory to package name, so a path dependency can be resolved the
        // way Cargo resolves it rather than by guessing at the directory name.
        let mut by_directory: BTreeMap<PathBuf, String> = BTreeMap::new();
        let mut packages: BTreeMap<String, Crate> = BTreeMap::new();
        for package in metadata["packages"].as_array().into_iter().flatten()
        {
            let Some(id) = package["id"].as_str()
            else
            {
                continue;
            };
            if !members.contains(id)
            {
                continue;
            }
            // `cargo metadata` always reports both fields for a member, so a
            // missing one is a malformed answer rather than a package with an
            // empty name. Skipping is the same policy the id check above uses:
            // an entry without an identity is not a package.
            let (Some(name), Some(manifest)) =
                (package["name"].as_str(), package["manifest_path"].as_str())
            else
            {
                continue;
            };
            let name = name.to_string();
            let manifest = PathBuf::from(manifest);
            let directory = manifest.parent().unwrap_or(Path::new(".")).to_path_buf();
            let relative = relative(&workspace.root, &directory)?;
            let features: BTreeSet<String> =
                package["features"].as_object()
                                   .into_iter()
                                   .flat_map(|features| features.keys().cloned())
                                   .collect();
            let manifest = directory.join("resources.json");
            let mut stage = stage(&name, &relative, &features)?;
            // A vendored executable can declare its application role without
            // moving its source fork into the OS-owned apps directory.
            if stage == Stage::Host
               && relative.starts_with("vendor/")
               && manifest.is_file()
               && mac_build::Manifest::read(&manifest)?.role == "application"
            {
                stage = Stage::Application;
            }
            by_directory.insert(canonical(&directory), name.clone());
            packages.insert(name.clone(),
                            Crate { name,
                                    relative,
                                    directory,
                                    manifest: manifest.is_file().then_some(manifest),
                                    stage,
                                    dependencies: Vec::new() });
        }
        for package in metadata["packages"].as_array().into_iter().flatten()
        {
            let Some(name) = package["name"].as_str()
            else
            {
                continue;
            };
            let Some(entry) = packages.get_mut(name)
            else
            {
                continue;
            };
            let mut dependencies = Vec::new();
            for dependency in package["dependencies"].as_array().into_iter().flatten()
            {
                let Some(path) = dependency["path"].as_str()
                else
                {
                    continue;
                };
                if let Some(name) = by_directory.get(&canonical(Path::new(path)))
                {
                    dependencies.push(name.clone());
                }
            }
            dependencies.sort();
            dependencies.dedup();
            entry.dependencies = dependencies;
        }
        Ok(Self { packages })
    }

    pub fn get(&self, name: &str) -> Result<&Crate>
    {
        self.packages
            .get(name)
            .ok_or_else(|| error(format!("'{name}' is not a package in this workspace")))
    }

    /// Packages that declare an extract list beside their manifest. The list's
    /// presence is what says a package takes resources from a disc, so a new
    /// package needs a file, not a change to the build.
    pub fn packages_with_extract_list(&self) -> Vec<&Crate>
    {
        self.packages
            .values()
            .filter(|entry| {
                entry.manifest
                     .as_ref()
                     .is_some_and(|manifest| manifest.with_file_name("extract-list.json").is_file())
            })
            .collect()
    }

    /// The named packages, in the order given, after checking each exists.
    pub fn select(&self, names: &[String]) -> Result<Vec<&Crate>>
    {
        names.iter().map(|name| self.get(name)).collect()
    }

    pub fn in_stage(&self, stage: Stage) -> Vec<&Crate>
    {
        self.packages.values().filter(|package| package.stage == stage).collect()
    }

    /// The named packages and everything they depend on by path, as a sorted
    /// deduplicated set. The order is the tree's, so it does not depend on the
    /// order the dependencies happened to be discovered in.
    pub fn closure(&self, roots: &[&str]) -> Result<Vec<&Crate>>
    {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut pending: Vec<&str> = Vec::new();
        for root in roots
        {
            // A root that is not a package is a defect in the caller, not an
            // empty answer.
            pending.push(&self.get(root)?.name);
        }
        let mut found = Vec::new();
        while let Some(name) = pending.pop()
        {
            if !seen.insert(name)
            {
                continue;
            }
            let package = self.get(name)?;
            found.push(package);
            pending.extend(package.dependencies.iter().map(String::as_str));
        }
        found.sort_by(|left, right| left.relative.cmp(&right.relative));
        Ok(found)
    }
}

/// One file a platform installs on its boot volume: which workspace package it
/// came from, the name the guest sees, and the guest folder it lives in.
pub struct Installed
{
    pub package: String,
    pub name:    String,
    pub folder:  String
}

impl Installed
{
    /// The guest path, `<folder>/<name>`.
    pub fn path(&self) -> String
    {
        format!("{}/{}", self.folder, self.name)
    }
}

/// Resolve a platform's installed set to the files it puts on the volume.
///
/// The package comes from the platform record, the display name from the
/// package's own `resources.json`, and the folder from the record's folder
/// table. Nothing here is a second hand-maintained list of the installed set.
pub fn installed(workspace: &Workspace, crates: &Crates) -> Result<Vec<Installed>>
{
    let install = &workspace.platform.install;
    let mut found = Vec::new();
    for package in install.packages()
    {
        let entry = crates.get(package)?;
        let resources = entry.resources()?;
        // `relative` is documented as the package's directory inside the
        // checkout, so a package the tree cannot place is a fact to report
        // rather than one to classify as sitting in the root.
        let group = entry.relative
                         .split('/')
                         .next()
                         .filter(|group| !group.is_empty())
                         .ok_or_else(|| {
                             error(format!("package '{}' has no directory inside the checkout",
                                           entry.name))
                         })?;
        found.push(Installed { package: package.clone(),
                               name:    resources.name.clone(),
                               folder:  install.folder(package, group)?.to_string() });
    }
    Ok(found)
}

/// Every source file under a package directory, excluding generated output.
/// A directory that cannot be read is an error: the answer feeds dependency
/// tracking, and a silently smaller answer is a silently stale cache.
pub fn files(directory: &Path) -> Result<Vec<PathBuf>>
{
    let mut found = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(directory) = pending.pop()
    {
        let entries = fs::read_dir(&directory).map_err(|why| {
                                                  error(format!("cannot list '{}': {why}",
                                                                directory.display()))
                                              })?;
        for entry in entries
        {
            let path = entry?.path();
            let Some(name) = path.file_name().map(|name| name.to_string_lossy().into_owned())
            else
            {
                continue;
            };
            if path.is_dir()
            {
                if !mac_build::paths::is_output_directory(&name) && !name.starts_with('.')
                {
                    pending.push(path);
                }
            }
            else if !name.starts_with('.')
            {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// A package's stage follows from where it lives in the tree, so a new driver
/// directory is picked up without naming it anywhere. The `guest` feature says
/// the same thing, and the two are checked against each other here: one
/// authority with a contradiction reported, rather than two predicates that
/// can disagree.
fn stage(name: &str, relative: &str, features: &BTreeSet<String>) -> Result<Stage>
{
    let stage = if relative.starts_with("apps/")
    {
        Stage::Application
    }
    else if relative.starts_with("definitions/")
    {
        Stage::Definitions
    }
    else if relative == "system"
              || relative.starts_with("drivers/")
              || relative.starts_with("extensions/")
    {
        Stage::Guest
    }
    else
    {
        Stage::Host
    };
    let guest_image = features.contains("guest");
    if guest_image != matches!(stage, Stage::Definitions | Stage::Guest)
    {
        return Err(error(format!("package '{name}' lives in '{relative}' but its manifest {} \
                                  a `guest` feature; the tree and the manifest disagree",
                                 if guest_image { "declares" } else { "does not declare" })));
    }
    Ok(stage)
}

fn relative(root: &Path, directory: &Path) -> Result<String>
{
    let relative = directory.strip_prefix(root).map_err(|_| {
                                                    error(format!("package directory '{}' is not \
                                                                   inside the checkout '{}'",
                                                                  directory.display(),
                                                                  root.display()))
                                                })?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

/// A comparison key that does not depend on spelling, junction or case.
fn canonical(path: &Path) -> PathBuf
{
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let text = resolved.to_string_lossy().into_owned();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text).to_string();
    if cfg!(windows) { PathBuf::from(text.to_lowercase()) } else { PathBuf::from(text) }
}
