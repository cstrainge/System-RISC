//! Which workspace packages a platform installs.
//!
//! The build tool discovers the crate graph from Cargo: package names,
//! manifests, the `guest` feature that marks a no_std guest image, and the
//! `resources.json` that gives a package its installed name. This record only
//! selects from that graph. Adding an application, a driver or an extension to
//! a platform is therefore an edit here, and never an edit to the build tool.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{Result, error};

/// Whether a platform intentionally installs no guest packages.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Set
{
    /// The platform installs the packages its arrays name; an empty array is a
    /// record defect.
    #[default]
    Packages,
    /// The platform intentionally has no guest set yet. It must give a reason,
    /// and every build stage that would be empty refuses with that reason
    /// instead of stamping a silent success.
    None
}

/// An ordered selection of workspace packages plus per-package features and
/// the guest folders they are installed into.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Install
{
    /// What this platform's installed set is for. Optional prose.
    #[serde(default)]
    pub description:  String,
    /// Whether the installed set is a real selection or an intentional empty
    /// set. The default keeps existing records honest: an empty package list
    /// under `packages` is rejected rather than passing as success.
    #[serde(default)]
    pub set:          Set,
    /// Why the platform installs no guest set. Required when `set` is `none`.
    #[serde(default)]
    pub reason:       String,
    /// Application packages, in the order they should be installed.
    #[serde(default)]
    pub applications: Vec<String>,
    /// Resident system, driver and extension packages. Their stage follows from
    /// the package's own directory, so the order here is not significant.
    #[serde(default)]
    pub modules:      Vec<String>,
    /// The Cargo features each installed package is built with, keyed by
    /// package name.
    ///
    /// This is the shipped set: what every build of this platform compiles. A
    /// package that is not installed cannot carry features, and an unknown
    /// package is an error rather than a silently ignored entry.
    ///
    /// Host-side diagnostics are deliberately *not* here. `text-edit`'s
    /// `input-trace` and `mac-system`'s `event-trace` trace what the guest is
    /// doing, and a record that enabled one would turn it on for everybody who
    /// built that platform while making the shipped set depend on a debugging
    /// build. The build tool enables them from `cargo xtask check` or from the
    /// environment variable named beside each one, so they never appear in this
    /// map and never reach a release by being described here.
    #[serde(default)]
    pub features:     BTreeMap<String, Vec<String>>,
    /// Where each kind of package is installed on the boot volume, keyed by the
    /// package's directory group. The boot volume's folder layout is a property
    /// of the platform's file system, not of the build tool.
    #[serde(default)]
    pub folders:      Folders,
    /// Packages installed somewhere other than the folder their group implies,
    /// keyed by package name.
    #[serde(default)]
    pub exceptions:   BTreeMap<String, String>
}

/// The guest folder each group of packages is installed into.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Folders
{
    /// Native applications.
    #[serde(default = "applications_folder")]
    pub applications: String,
    /// The resident System image.
    #[serde(default = "system_folder")]
    pub system:       String,
    /// Resident drivers.
    #[serde(default = "drivers_folder")]
    pub drivers:      String,
    /// System extensions.
    #[serde(default = "extensions_folder")]
    pub extensions:   String
}

impl Default for Folders
{
    fn default() -> Self
    {
        Self { applications: applications_folder(),
               system:       system_folder(),
               drivers:      drivers_folder(),
               extensions:   extensions_folder() }
    }
}

fn applications_folder() -> String
{
    "Applications".to_string()
}

fn system_folder() -> String
{
    "System Folder".to_string()
}

fn drivers_folder() -> String
{
    "System Folder/Drivers".to_string()
}

fn extensions_folder() -> String
{
    "System Folder/Extensions".to_string()
}

impl Install
{
    /// Validate that the record says which kind of installed set it is.
    ///
    /// An empty package list is not success: it is either an intentional
    /// `none` set with a reason, or a record defect. The build stages refuse
    /// an empty stage with this declaration, so a later typo that empties an
    /// array cannot silently produce a green build.
    pub fn check(&self, platform: &str) -> Result<()>
    {
        match self.set
        {
            Set::None =>
            {
                if !self.applications.is_empty() || !self.modules.is_empty()
                {
                    return Err(error(format!("platform '{platform}' install.set is none but \
                                              install.applications or install.modules names \
                                              packages; remove them or set install.set to \
                                              packages")));
                }
                if !self.features.is_empty() || !self.exceptions.is_empty()
                {
                    return Err(error(format!("platform '{platform}' install.set is none but \
                                              install.features or install.exceptions is \
                                              populated; remove them")));
                }
                if self.reason.trim().is_empty()
                {
                    return Err(error(format!("platform '{platform}' install.set is none \
                                              requires a nonempty install.reason explaining \
                                              why this platform has no guest set")));
                }
            }
            Set::Packages =>
            {
                if self.applications.is_empty() && self.modules.is_empty()
                {
                    return Err(error(format!("platform '{platform}' install.set is packages \
                                              but lists no applications or modules; add the \
                                              packages or set install.set to none with a \
                                              reason")));
                }
            }
        }
        Ok(())
    }

    /// Every selected package, applications first.
    pub fn packages(&self) -> impl Iterator<Item = &String>
    {
        self.applications.iter().chain(self.modules.iter())
    }

    /// Features requested for `package`, empty when none are declared.
    pub fn features_for(&self, package: &str) -> &[String]
    {
        self.features.get(package).map(Vec::as_slice).unwrap_or_default()
    }

    /// The folder `package` is installed into, given the directory group its
    /// sources live in (`apps`, `system`, `drivers`, `extensions`).
    pub fn folder(&self, package: &str, group: &str) -> Result<&str>
    {
        if let Some(exception) = self.exceptions.get(package)
        {
            return Ok(exception);
        }

        let folder = match group
        {
            "apps" => &self.folders.applications,
            "system" => &self.folders.system,
            "drivers" => &self.folders.drivers,
            "extensions" => &self.folders.extensions,
            other =>
            {
                return Err(error(format!("package '{package}' lives in '{other}', which this \
                                          platform installs nowhere; add it to \
                                          install.exceptions")));
            }
        };

        Ok(folder)
    }
}
