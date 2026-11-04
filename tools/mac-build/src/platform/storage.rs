//! What a platform installs on media, described as a layout rather than as a
//! sequence of image-building commands.
//!
//! Storage lives at the platform level, not under `emulation` or `hardware`,
//! because the same described medium has to serve both: an emulator attaches it
//! as a disk, and a real board is written to removable media with it. Emulation
//! and hardware therefore *reference* an image by name instead of describing
//! their own.
//!
//! A `system7-filesystem` partition has no file list. Its contents are derived
//! from the installed set the platform already declares — the system files,
//! drivers, extensions and applications, with their per-package features — so
//! there is exactly one place to add an application to a platform.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{Result, error};

/// The named media a platform can produce, attach and write.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Storage
{
    pub images: BTreeMap<String, Image>
}

/// One medium. Either the whole medium is a single filesystem, or it carries a
/// partition table, or it is a raw payload at an offset, or it is a container.
/// Those are genuinely different shapes and the schema keeps them apart instead
/// of collapsing them onto one optional `offset` field.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Image
{
    /// What this medium is, in one line.
    pub description: String,
    /// The file this medium is built as, under the build's `dist` directory.
    /// Declared rather than derived from the image's key so a record can name
    /// the file a guest, a board or a person expects.
    #[serde(default)]
    pub output:      Option<String>,
    pub layout:      Layout
}

impl Image
{
    /// The file name this medium is written to.
    pub fn file(&self, name: &str) -> String
    {
        self.output.clone().unwrap_or_else(|| format!("{name}.img"))
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Layout
{
    /// The whole medium is one filesystem with no partition table, which is
    /// what a guest that expects a bare volume needs.
    Filesystem
    {
        filesystem: FilesystemKind,
        size:       SizeRule,
        /// Explicit contents. Refused for `system7-filesystem`, whose contents
        /// are derived from the installed set.
        #[serde(default)]
        files:      Vec<Entry>
    },
    /// An ordered partition list behind a partition table.
    Partitioned
    {
        table: Table, partitions: Vec<Partition>
    },
    /// A payload at an absolute offset, for SPI flash, pflash or ROM media.
    Raw
    {
        offset: String, payload: Source, size: SizeRule
    },
    /// A named container or archive format holding files at named paths.
    Container
    {
        format: ContainerFormat, files: Vec<Entry>
    }
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FilesystemKind
{
    Fat32,
    /// The OS's own filesystem, populated from the platform's installed set.
    System7
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TableKind
{
    Mbr,
    Gpt
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Table
{
    pub kind: TableKind,
    /// Index into `partitions` of the partition marked bootable.
    #[serde(default)]
    pub boot: Option<usize>
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PartitionKind
{
    Fat32,
    System7Filesystem,
    Raw
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Partition
{
    pub kind:    PartitionKind,
    /// The label a person or another tool sees on the partition.
    pub label:   String,
    pub size:    SizeRule,
    /// Ordered contents, for `fat32` only.
    #[serde(default)]
    pub files:   Vec<Entry>,
    /// Absolute offset and payload, for `raw` only.
    #[serde(default)]
    pub offset:  Option<String>,
    #[serde(default)]
    pub payload: Option<Source>
}

/// How much room a medium or partition gets. `remaining` consumes whatever the
/// other declarations leave, so a layout states sizes instead of magic numbers
/// that must add up.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SizeRule
{
    Bytes
    {
        bytes: String
    },
    Sectors
    {
        sectors: u64
    },
    Remaining
}

impl SizeRule
{
    /// The fixed size in bytes, or `None` for `remaining`.
    pub fn bytes(&self) -> Result<Option<u64>>
    {
        match self
        {
            Self::Bytes { bytes } => Ok(Some(quantity(bytes)?)),
            Self::Sectors { sectors } =>
            {
                sectors.checked_mul(512)
                       .map(Some)
                       .ok_or_else(|| error(format!("{sectors} sectors does not fit in a u64")))
            }
            Self::Remaining => Ok(None)
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry
{
    pub source: Source,
    /// Destination path inside the partition, with `/` separators.
    pub path:   String
}

/// Where a file's bytes come from. A built artifact is named, never guessed
/// from a path convention, so renaming an artifact is one edit.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Source
{
    /// A build artifact by its declared name.
    Artifact
    {
        artifact: String
    },
    /// A file in the workspace, relative to the repository root.
    File
    {
        file: String
    },
    /// A file rendered from platform data at build time.
    BoardConfig
    {
        lines: Vec<Setting>
    }
}

/// One `key=value` line in a generated board configuration file.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Setting
{
    pub key:   String,
    pub value: String
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerFormat
{
    Uf2,
    BootBin,
    Zip
}

impl Storage
{
    pub fn check(&self, platform: &str) -> Result<()>
    {
        if self.images.is_empty()
        {
            return Err(error(format!("platform '{platform}' storage.images must declare at \
                                      least one image")));
        }
        for (name, image) in &self.images
        {
            image.check(platform, name)?;
        }
        Ok(())
    }

    pub fn image(&self, name: &str) -> Option<&Image>
    {
        self.images.get(name)
    }

    pub fn names(&self) -> String
    {
        self.images.keys().cloned().collect::<Vec<_>>().join(", ")
    }
}

impl Image
{
    fn check(&self, platform: &str, name: &str) -> Result<()>
    {
        let reject = |field: &str, why: String| {
            error(format!("platform '{platform}' storage.images.{name}.{field}: {why}"))
        };
        if self.description.trim().is_empty()
        {
            return Err(reject("description", "must describe the medium".into()));
        }
        if self.output.as_ref().is_some_and(|output| output.trim().is_empty())
        {
            return Err(reject("output", "must name a file when present".into()));
        }
        match &self.layout
        {
            Layout::Filesystem { filesystem,
                                 size,
                                 files } =>
            {
                size.bytes().map_err(|cause| reject("layout.size", cause.to_string()))?;
                if *filesystem == FilesystemKind::System7 && !files.is_empty()
                {
                    return Err(reject("layout.files",
                                      "must be empty for system7-filesystem, whose contents \
                                       are derived from the platform's installed set; add the \
                                       package to the installed set instead"
                                                                            .into()));
                }
                entries(platform, name, "layout.files", files)?;
            }
            Layout::Partitioned { table,
                                  partitions } => partitioned(platform, name, table, partitions)?,
            Layout::Raw { offset,
                          payload,
                          size } =>
            {
                let offset =
                    quantity(offset).map_err(|cause| reject("layout.offset", cause.to_string()))?;
                if offset == 0
                {
                    return Err(reject("layout.offset",
                                      "must be a nonzero absolute offset".into()));
                }
                size.bytes().map_err(|cause| reject("layout.size", cause.to_string()))?;
                source(platform, name, "layout.payload", payload)?;
            }
            Layout::Container { format: _,
                                files } =>
            {
                if files.is_empty()
                {
                    return Err(reject("layout.files", "must list the files to package".into()));
                }
                entries(platform, name, "layout.files", files)?;
            }
        }
        Ok(())
    }
}

fn partitioned(platform: &str, image: &str, table: &Table, partitions: &[Partition]) -> Result<()>
{
    let reject = |field: &str, why: String| {
        error(format!("platform '{platform}' storage.images.{image}.{field}: {why}"))
    };
    if partitions.is_empty()
    {
        return Err(reject("layout.partitions", "must declare at least one partition".into()));
    }
    if let Some(boot) = table.boot
    {
        if boot >= partitions.len()
        {
            return Err(reject("layout.table.boot",
                              format!("index {boot} does not name a partition; there are {}",
                                      partitions.len())));
        }
    }
    let mut remaining = None;
    for (index, partition) in partitions.iter().enumerate()
    {
        let field = format!("layout.partitions[{index}]");
        if partition.label.trim().is_empty()
        {
            return Err(reject(&format!("{field}.label"), "must not be empty".into()));
        }
        if partitions[..index].iter().any(|seen| seen.label == partition.label)
        {
            return Err(reject(&format!("{field}.label"),
                              format!("duplicates the label '{}'", partition.label)));
        }
        let fixed = partition.size
                             .bytes()
                             .map_err(|cause| reject(&format!("{field}.size"), cause.to_string()))?;
        match (partition.size.clone(), fixed)
        {
            (SizeRule::Remaining, _) =>
            {
                if remaining.replace(index).is_some()
                {
                    return Err(reject("layout.partitions",
                                      "only one partition may consume the remaining space".into()));
                }
                if index + 1 != partitions.len()
                {
                    return Err(reject(&format!("{field}.size"),
                                      "remaining must be the last partition so the layout is \
                                       unambiguous"
                                                   .into()));
                }
            }
            (_, Some(0)) =>
            {
                return Err(reject(&format!("{field}.size"), "must be nonzero".into()));
            }
            _ =>
            {}
        }
        match partition.kind
        {
            PartitionKind::System7Filesystem =>
            {
                if !partition.files.is_empty()
                {
                    return Err(reject(&format!("{field}.files"),
                                      "must be empty for system7-filesystem, whose contents \
                                       are derived from the platform's installed set; add the \
                                       package to the installed set instead"
                                                                            .into()));
                }
            }
            PartitionKind::Fat32 =>
            {
                if partition.payload.is_some()
                {
                    return Err(reject(&format!("{field}.payload"),
                                      "belongs to a raw partition; a fat32 partition lists \
                                       files"
                                             .into()));
                }
                entries(platform, image, &format!("{field}.files"), &partition.files)?;
            }
            PartitionKind::Raw =>
            {
                if !partition.files.is_empty()
                {
                    return Err(reject(&format!("{field}.files"),
                                      "belongs to a fat32 partition; a raw partition states a \
                                       payload and an offset"
                                                             .into()));
                }
                let Some(offset) = &partition.offset
                else
                {
                    return Err(reject(&format!("{field}.offset"),
                                      "is required for a raw partition".into()));
                };
                quantity(offset).map_err(|cause| {
                                    reject(&format!("{field}.offset"), cause.to_string())
                                })?;
                let Some(payload) = &partition.payload
                else
                {
                    return Err(reject(&format!("{field}.payload"),
                                      "is required for a raw partition".into()));
                };
                source(platform, image, &format!("{field}.payload"), payload)?;
            }
        }
    }
    Ok(())
}

fn entries(platform: &str, image: &str, field: &str, files: &[Entry]) -> Result<()>
{
    let reject =
        |why: String| error(format!("platform '{platform}' storage.images.{image}.{field}: {why}"));
    for (index, entry) in files.iter().enumerate()
    {
        let path = entry.path.trim();
        if path.is_empty()
        {
            return Err(reject(format!("entry {index} has an empty destination path")));
        }
        if path.starts_with('/') || path.contains('\\')
        {
            return Err(reject(format!("destination '{}' must be a relative path with / \
                                       separators",
                                      entry.path)));
        }
        if path.split('/').any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(reject(format!("destination '{}' must not contain empty, . or .. \
                                       segments",
                                      entry.path)));
        }
        if files[..index].iter().any(|seen| seen.path == entry.path)
        {
            return Err(reject(format!("destination '{}' is listed twice", entry.path)));
        }
        source(platform, image, field, &entry.source)?;
    }
    Ok(())
}

fn source(platform: &str, image: &str, field: &str, source: &Source) -> Result<()>
{
    let reject =
        |why: String| error(format!("platform '{platform}' storage.images.{image}.{field}: {why}"));
    match source
    {
        Source::Artifact { artifact } =>
        {
            if artifact.trim().is_empty()
            {
                return Err(reject("must name a build artifact".into()));
            }
        }
        Source::File { file } =>
        {
            let file = file.trim();
            if file.is_empty()
            {
                return Err(reject("must name a workspace file".into()));
            }
            if file.starts_with('/') || file.contains('\\') || file.contains("..")
            {
                return Err(reject(format!("workspace file '{file}' must be a relative path \
                                           inside the repository")));
            }
        }
        Source::BoardConfig { lines } =>
        {
            if lines.is_empty()
            {
                return Err(reject("a generated board config must declare at least one line"
                                      .into()));
            }
            for (index, line) in lines.iter().enumerate()
            {
                if line.key.trim().is_empty()
                   || line.key.contains('=')
                   || line.key.contains(char::is_whitespace)
                {
                    return Err(reject(format!("line {index} has an invalid key '{}'", line.key)));
                }
                if line.value.contains('\n')
                {
                    return Err(reject(format!("line {index} value must be a single line")));
                }
            }
        }
    }
    Ok(())
}

/// A byte count written either in hex or in decimal.
pub fn quantity(text: &str) -> Result<u64>
{
    let text = text.trim();
    let parsed = match text.strip_prefix("0x")
    {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse::<u64>()
    };
    parsed.map_err(|_| {
              error(format!("'{text}' is not a byte count; use decimal or 0x-prefixed hex"))
          })
}
