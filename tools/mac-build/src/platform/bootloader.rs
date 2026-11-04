//! Which bootloader a platform builds.
//!
//! A bootloader is inherently platform-specific: it carries its own entry
//! assembly, its own linker script, and its own page-table crate. Several
//! bootloaders coexist in the workspace and each platform record names the one
//! it builds, so nothing in the build assumes a particular package name or a
//! particular payload path: both come from the record.
//!
//! A platform may declare no bootloader at all. Vendor firmware as the first
//! stage is a legitimate arrangement, and so is a board whose boot path this
//! workspace does not own yet; the build then produces no boot payload and says
//! so rather than inventing one.

use serde::Deserialize;

use crate::{Result, error};

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Definition
{
    /// The workspace package that builds the bootloader.
    pub package:       String,
    /// The name the boot payload is published under, without an extension.
    pub artifact:      String,
    /// The entry assembly the package includes, relative to the repository root.
    pub entry:         String,
    /// The linker script the package links with, relative to the repository root.
    pub linker_script: String,
    /// The architecture page-table crate the bootloader links, if any. The
    /// bootloader may depend on one; the shared packager may not.
    #[serde(default)]
    pub page_table:    Option<String>,
    /// One line describing the boot path, for `xtask platforms` and errors.
    pub description:   String
}

impl Definition
{
    pub fn check(&self, platform: &str) -> Result<()>
    {
        let reject = |field: &str, why: String| {
            error(format!("platform '{platform}' bootloader.{field}: {why}"))
        };
        for (field, value) in [("package", &self.package),
                               ("artifact", &self.artifact),
                               ("entry", &self.entry),
                               ("linker_script", &self.linker_script),
                               ("description", &self.description)]
        {
            if value.trim().is_empty()
            {
                return Err(reject(field, "must not be empty".into()));
            }
        }
        if self.artifact.contains(['/', '\\', '.'])
        {
            return Err(reject("artifact",
                              format!("'{}' must be a bare artifact name", self.artifact)));
        }
        for (field, path) in [("entry", &self.entry), ("linker_script", &self.linker_script)]
        {
            if path.starts_with('/') || path.contains('\\') || path.contains("..")
            {
                return Err(reject(field,
                                  format!("'{path}' must be a relative path inside the \
                                           repository")));
            }
        }
        if let Some(crate_name) = &self.page_table
        {
            if crate_name.trim().is_empty()
            {
                return Err(reject("page_table", "must name a crate or be omitted".into()));
            }
        }
        Ok(())
    }
}
