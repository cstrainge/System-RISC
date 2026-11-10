//! The boot volume, built from the platform's declared storage layout.
//!
//! The medium's size, filesystem and contents come from the platform record.
//! This module is the only place that knows how to realise one, and it refuses
//! a layout it cannot produce rather than substituting a bare volume: a record
//! that validates and is then ignored advertises a contract the build does not
//! honour.
//!
//! What lands on the volume is the platform's installed set, resolved through
//! the workspace crate graph and each package's own `resources.json`. There is
//! no second list of packages here.
use std::{collections::BTreeMap,
          env, fs,
          path::{Path, PathBuf},
          time::{SystemTime, UNIX_EPOCH}};

use mac_build::{Result, error, package,
                platform::storage::{FilesystemKind, Layout}};
use mac_formats::{macbinary, text};
use serde_json::{Value, json};

use crate::{Workspace,
            crates::{Crates, Installed},
            write_json};

mod partition;

/// The layouts this builder can produce, so an unsupported shape is reported
/// against the record that declared it.
const SUPPORTED: &str = "a single unpartitioned system7-filesystem volume or a partitioned medium";

/// Check that every installed package can be named uniquely inside its folder.
pub(super) fn validate_modules(workspace: &Workspace) -> Result<()>
{
    let crates = Crates::read(workspace)?;
    let installed = crate::crates::installed(workspace, &crates)?;
    crate::modules::validate_identities(&workspace.dist(),
                                        installed.iter().map(|entry| entry.name.as_str()))
}

/// A private working directory for one image build.
///
/// Uniquely named, so two builds never collide, and outside the platform's
/// output directory, so a build in progress does not leave the output
/// directory dirty. A failure to remove it is reported: a scratch directory
/// that outlives its build is untidy, but it must never be mistaken for
/// another build running.
struct Scratch
{
    path: PathBuf
}

impl Scratch
{
    fn create(build: &Path) -> Result<Self>
    {
        fs::create_dir_all(build)?;
        let unique = format!("image-work-{}-{}",
                             std::process::id(),
                             SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos());
        let path = build.join(unique);
        fs::create_dir(&path)?;
        Ok(Self { path })
    }
}

impl Drop for Scratch
{
    fn drop(&mut self)
    {
        if let Err(cause) = fs::remove_dir_all(&self.path)
        {
            eprintln!("warning: could not remove the image scratch directory {}: {cause}",
                      self.path.display());
        }
    }
}

/// Offline native edits use the same crate as the formatter and resident OS.
pub(super) fn edit<R>(image: &Path,
                      apply: impl FnOnce(&mut mac_hfs_b::Volume<'_, hfs_b::Image>) -> Result<R>)
                      -> Result<R>
{
    let mut image = hfs_b::Image::open(image, true)?;
    let partition = image.partition();
    let mut volume = mac_hfs_b::Volume::mount(&mut image, partition, Default::default())?;
    apply(&mut volume)
}

/// A layout's shape, for an error that names what the record asked for.
fn describe(layout: &Layout) -> String
{
    match layout
    {
        Layout::Filesystem { filesystem,
                             .. } => format!("a {filesystem:?} filesystem"),
        Layout::Partitioned { table,
                              .. } => format!("a {table:?} partition table"),
        Layout::Raw { .. } => "a raw payload".to_string(),
        Layout::Container { format,
                            .. } => format!("a {format:?} container")
    }
}

pub fn image(workspace: &Workspace) -> Result<Value>
{
    let platform = &workspace.platform.name;
    let storage =
        workspace.platform.storage.as_ref().ok_or_else(|| {
                                                error(format!("platform '{platform}' declares no \
                                                               storage.images, so it has no boot \
                                                               volume to build"))
                                            })?;
    let name = workspace.boot_image_name()?;
    let image = storage.image(name).ok_or_else(|| {
                                        error(format!("platform '{platform}' storage.images has \
                                                       no image named '{name}'"))
                                    })?;
    let crates = Crates::read(workspace)?;
    let installed = crate::crates::installed(workspace, &crates)?;
    // Refuse collisions before any image mutation, including callers reusing
    // an already packaged dist directory.
    crate::modules::validate_identities(&workspace.dist(),
                                        installed.iter().map(|entry| entry.name.as_str()))?;
    let dist = workspace.dist();
    let scratch = Scratch::create(&workspace.build)?;
    let output = workspace.disk_image()?;
    // Say so when the image being replaced no longer matches the artifacts it
    // embeds. The rebuild below fixes it, but a silent fix is how a stale image
    // stays in someone's notes as "verified".
    if output.is_file()
    {
        if let Err(why) = verify(workspace)
        {
            println!("warning: {why}");
            println!("warning: rebuilding the image from the current artifacts");
        }
    }
    let (report, summary) = match &image.layout
    {
        Layout::Filesystem { filesystem,
                             size,
                             .. } =>
        {
            if *filesystem != FilesystemKind::System7
            {
                return Err(error(format!("platform '{platform}' storage.images.{name}: this \
                                          build produces {SUPPORTED}, not {filesystem:?}")));
            }
            let bytes = size.bytes()?.ok_or_else(|| {
                                          error(format!("platform '{platform}' \
                                                         storage.images.{name}: layout.size must \
                                                         state a size"))
                                      })?;
            let raw = scratch.path.join("boot.hfsb");
            let mut report = build_system7(workspace, &raw, bytes, &installed)?;
            fs::rename(&raw, &output)?;
            report["raw"] = json!(workspace.describe(&output));
            report["virtual_size"] = json!(bytes);
            report["layout"] = json!(format!("unpartitioned {filesystem:?}"));
            // The payload's name comes from the record's bootloader section, so
            // a board that builds a different first stage is not reported under
            // another board's artifact name. A platform with no bootloader of
            // its own has no payload and says so by omission.
            if workspace.platform.bootloader.is_some()
            {
                report["boot_rom"] =
                    json!(workspace.describe(&crate::boot::payload(&workspace.platform, &dist)?));
            }
            let summary = format!("Built {} MiB HFS-B v1 as unpartitioned System7; all installed \
                                   forks, metadata, blessing and structural checks passed.",
                                  bytes / (1024 * 1024));
            (report, summary)
        }
        Layout::Partitioned { table,
                              size,
                              partitions } =>
        {
            // Before anything is written: an image whose boot partition lacks the
            // firmware is intact, verifiable and unbootable.
            require_firmware(workspace, partitions)?;
            // And, now that the machine's own description is here, use it to
            // check the record rather than trusting the record to describe it.
            cross_check_tree(workspace)?;
            let total = size.bytes()?.ok_or_else(|| {
                                          error(format!("platform '{platform}' \
                                                         storage.images.{name}: layout.size must \
                                                         state a size"))
                                      })?;
            let raw = scratch.path.join("sd-card.img");
            let mut report =
                partition::build(workspace, &raw, total, table, partitions, &installed)?;
            fs::rename(&raw, &output)?;
            report["raw"] = json!(workspace.describe(&output));
            report["virtual_size"] = json!(total);
            report["layout"] = json!(format!("{:?} partitioned medium", table.kind));
            let summary = format!("Built {} MiB partitioned medium as {:?}; {} partition(s), \
                                   files read back.",
                                  total / (1024 * 1024),
                                  table.kind,
                                  partitions.len());
            (report, summary)
        }
        other =>
        {
            return Err(error(format!("platform '{platform}' storage.images.{name}: this build \
                                      produces {SUPPORTED}, not {}",
                                     describe(other))));
        }
    };
    write_json(&dist.join("disk.json"), &report)?;
    println!("{summary}");
    Ok(report)
}

/// Refuse a build whose boot partition would be unbootable.
///
/// The firmware binaries are Broadcom's: redistributable unmodified and only for
/// their own hardware, with the licence travelling alongside. They are therefore
/// developer-supplied input, never repository content, and a build that silently
/// omitted them would produce an image whose MBR, boot sector and kernel all
/// verify and which cannot start. Refusing by name, with the exact list, is the
/// difference between a documented prerequisite and a broken deliverable.
fn require_firmware(workspace: &Workspace, partitions: &[mac_build::platform::storage::Partition])
                    -> Result<()>
{
    use mac_build::platform::storage::{Partition, Source};
    let mut missing: Vec<String> = Vec::new();
    let mut wanted: Vec<String> = Vec::new();
    for partition in partitions
    {
        let Partition::Fat32 { files, .. } = partition
        else
        {
            continue;
        };
        for entry in files
        {
            let path = match &entry.source
            {
                Source::File { file } if file.starts_with("firmware/") =>
                {
                    Some(workspace.root().join(file))
                }
                // The board tree is the firmware distribution's own file for this
                // variant, which the record names.
                Source::BoardDeviceTree =>
                {
                    match workspace.platform
                                   .hardware
                                   .as_ref()
                                   .and_then(|hardware| hardware.board_device_tree.clone())
                    {
                        Some(name) =>
                        {
                            Some(workspace.root().join(partition::FIRMWARE_DIR).join(&name))
                        }
                        None => None
                    }
                }
                _ => continue
            };
            let Some(path) = path
            else
            {
                return Err(error(format!("platform '{}' stores the board device tree in its boot \
                                          partition but declares no hardware.board_device_tree",
                                         workspace.platform.name)));
            };
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            wanted.push(name.clone());
            if !path.is_file()
            {
                missing.push(format!("  {}  (expected at {})", name, path.display()));
            }
        }
    }
    if missing.is_empty()
    {
        return Ok(());
    }
    Err(error(format!("the boot partition needs {count} file(s) this checkout does not have:\n\
                       {missing}\n\
                       They are the board's own firmware, redistributable only unmodified and \
                       only with their licence, so they are developer-supplied and never \
                       committed. Copy {wanted} from the firmware distribution's boot/ directory \
                       into {dir}/ at the repository root, keeping the names as published. The \
                       board device tree is the file for this board variant; the record names \
                       which.",
                      count = missing.len(),
                      missing = missing.join("\n"),
                      wanted = wanted.join(", "),
                      dir = partition::FIRMWARE_DIR)))
}

/// Check the record against the machine's own description.
///
/// The record is a declaration used for module selection and validation; the
/// machine's tree is what the kernel actually walks. Comparing them is how a
/// wrong interrupt or a transposed address is caught by arithmetic instead of by
/// somebody re-reading a datasheet — the record has been wrong three times in one
/// day, each time found by hand.
///
/// A warning, not a refusal: this is a check against a source the project has not
/// yet validated, and a hard failure on first contact would block work on a false
/// positive. The comparison is **post-overlay** — the tree as the machine
/// actually has it, overlays applied — because that is also what the kernel sees.
/// A peripheral an overlay disables is therefore an expected finding.
fn cross_check_tree(workspace: &Workspace) -> Result<()>
{
    let Some(name) = workspace.platform
                              .hardware
                              .as_ref()
                              .and_then(|hardware| hardware.board_device_tree.clone())
    else
    {
        return Ok(());
    };
    let path = workspace.root().join(partition::FIRMWARE_DIR).join(&name);
    if !path.is_file()
    {
        // The missing-file refusal has already spoken; nothing to compare yet.
        return Ok(());
    }
    let blob = fs::read(&path)?;
    let check = match mac_build::devicetree::compare(&workspace.platform, &blob)
    {
        Ok(check) => check,
        Err(why) =>
        {
            println!("warning: cannot read the machine's device tree {name}: {why}");
            return Ok(());
        }
    };
    // Expected findings are printed too, and with the same detail. A check that
    // quietly accepted the differences it had been told to expect would be a check
    // nobody could audit; what changes is only that they are not warnings, because
    // nobody can act on them.
    if !check.expected.is_empty()
    {
        println!("device tree cross-check: {name}, differences the record accounts for \
                  (pre-overlay):");
        for expected in &check.expected
        {
            println!("  {}: {}", expected.model, expected.detail);
        }
    }
    if check.warnings.is_empty()
    {
        println!("device tree cross-check: the record agrees with {name} on all {} device(s) it \
                  declares{}",
                 workspace.platform.declared_devices()?.len(),
                 if check.agreed_without_interrupt.is_empty()
                 {
                     String::new()
                 }
                 else
                 {
                     format!(", {} of which declare no interrupt on either side",
                             check.agreed_without_interrupt.len())
                 });
        return Ok(());
    }
    println!("warning: the record and the machine's tree {name} disagree (pre-overlay):");
    for divergence in &check.warnings
    {
        println!("warning:   {}: {}", divergence.model, divergence.detail);
    }
    println!("warning: the machine's tree is the runtime contract; the record is a declaration. \
              Resolve the difference rather than adjusting one to match the other.");
    Ok(())
}

/// One artifact a report says the image was built from.
struct Claimed
{
    /// How a diagnostic names it: the image member the artifact was installed as.
    label:    String,
    artifact: String,
    sha256:   String
}

/// Every artifact a report claims it embedded, whichever layout it describes.
///
/// A partitioned Medium nests them under `partitions[].system7.files`; a bare System7
/// volume records the same fields at the top level. Exactly one of those shapes
/// applies, and this reads exactly one: collecting from both would count every
/// artifact twice if a report ever carried both keys, and a doubled list is a doubled
/// verdict.
fn claimed(report: &Value) -> Vec<Claimed>
{
    let mut claims = Vec::new();
    match report["partitions"].as_array()
    {
        Some(partitions) =>
        {
            for partition in partitions
            {
                collect(&partition["system7"], &mut claims);
            }
        }
        None => collect(report, &mut claims)
    }
    claims
}

fn collect(section: &Value, claims: &mut Vec<Claimed>)
{
    let Some(files) = section["files"].as_array()
    else
    {
        return;
    };
    for file in files
    {
        let (Some(artifact), Some(sha256)) =
            (file["artifact"].as_str(), file["artifact_sha256"].as_str())
        else
        {
            continue;
        };
        claims.push(Claimed { label: file["path"].as_str().unwrap_or(artifact).to_string(),
                              artifact: artifact.to_string(),
                              sha256: sha256.to_string() });
    }
}

/// Compare the claimed artifacts against what the checkout now holds.
///
/// `hash` is supplied so a test can witness both report shapes without a workspace or
/// a build. Returns how many were checked and a message per stale one, so a caller
/// can distinguish "no claims" from "claims that all agree".
fn stale_claims(report: &Value, hash: impl Fn(&str) -> String) -> (usize, Vec<String>)
{
    let claims = claimed(report);
    let mut stale = Vec::new();
    for claim in &claims
    {
        let current = hash(&claim.artifact);
        if current != claim.sha256
        {
            stale.push(format!("the image holds {} built from {}, which has changed since the \
                                image was written (recorded {}, current {})",
                               claim.label, claim.artifact, claim.sha256, current));
        }
    }
    (claims.len(), stale)
}

/// Refuse an image that no longer matches the artifacts it claims to contain.
///
/// The image records, in its own provenance, the hash of every artifact it
/// embedded and of every packaged file it installed. A payload rebuilt after the
/// image was written leaves the image claiming contents it does not have — which
/// reads exactly like a good image, because the file exists and its own bytes are
/// intact. This is the check that makes that difference visible, and it is called
/// by everything that hands an existing image to something: a card, an emulated
/// machine, or a check.
pub fn verify(workspace: &Workspace) -> Result<()>
{
    let image = workspace.disk_image()?;
    if !image.is_file()
    {
        return Err(error(format!("no storage image at {}; build it first", image.display())));
    }
    let provenance = workspace.dist().join("disk.json");
    let report: Value = serde_json::from_slice(&fs::read(&provenance).map_err(|why| {
        error(format!("cannot read {}: {why}; the image has no recorded provenance, so its \
                       contents cannot be checked against the artifacts", provenance.display()))
    })?)?;
    let mut checked = 0;
    let mut stale: Vec<String> = Vec::new();
    if let Some(partitions) = report["partitions"].as_array()
    {
        for partition in partitions
        {
            // A FAT partition names each file's source, so the artifact is
            // re-read and compared.
            if let Some(files) = partition["files"].as_array()
            {
                for file in files
                {
                    let Some(source) = file["source"].as_str()
                    else
                    {
                        continue;
                    };
                    let Some(recorded) = file["sha256"].as_str()
                    else
                    {
                        continue;
                    };
                    // A file the image copied from the checkout is re-read where
                    // it came from, so a swapped or regenerated firmware blob or
                    // board tree is caught by the same gate as a rebuilt payload.
                    let path = if let Some(artifact) = source.strip_prefix("artifact:")
                    {
                        workspace.dist().join(format!("{artifact}.bin"))
                    }
                    else if let Some(file) = source.strip_prefix("file:")
                    {
                        if !file.starts_with(partition::FIRMWARE_DIR)
                        {
                            checked += 1;
                            continue;
                        }
                        workspace.root().join(file)
                    }
                    else if let Some(name) = source.strip_prefix("firmware:")
                    {
                        workspace.root().join(partition::FIRMWARE_DIR).join(name)
                    }
                    else
                    {
                        continue;
                    };
                    checked += 1;
                    let current = fs::read(&path)
                        .map(|bytes| package::sha256(&bytes))
                        .unwrap_or_else(|_| "<missing>".into());
                    if current != recorded
                    {
                        stale.push(format!("{} holds {} (recorded {}), current {} is {}",
                                           image.file_name().unwrap_or_default().to_string_lossy(),
                                           file["path"].as_str().unwrap_or_default(),
                                           recorded,
                                           path.file_name().unwrap_or_default().to_string_lossy(),
                                           current));
                    }
                }
            }
        }
    }
    // Artifacts the image says it embedded, whichever layout the report describes.
    //
    // A partitioned Medium records them inside each partition; a bare System7 volume
    // records the same fields at the top level. This used to read only the partitioned
    // shape, so a bare image produced no verdict at all — verify reported that
    // provenance was missing when it was present under another key, which is a check
    // that silently did not apply to half the platforms it serves.
    let (artifacts, stale_artifacts) = stale_claims(&report, |artifact| {
        fs::read(workspace.dist().join(artifact)).map(|bytes| package::sha256(&bytes))
                                                 .unwrap_or_else(|_| "<missing>".into())
    });
    checked += artifacts;
    stale.extend(stale_artifacts);
    if checked == 0
    {
        return Err(error(format!("{} records no artifact provenance, so an image that predates \
                                  the payload cannot be told from a current one; rebuild it",
                                 provenance.display())));
    }
    if !stale.is_empty()
    {
        return Err(error(format!("the storage image {} is stale against the artifacts it \
                                  embeds:\n  {}",
                                 image.display(),
                                 stale.join("\n  "))));
    }
    Ok(())
}

/// Build a whole System7 volume at `path`. Both the unpartitioned layout and a
/// partitioned System7 partition use this one population path, so the installed
/// set, blessing and structural checks cannot drift between them.
fn build_system7(workspace: &Workspace, path: &Path, size: u64, installed: &[Installed])
                 -> Result<Value>
{
    let dist = workspace.dist();
    let started = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let mut image = hfs_b::Image::create(path, size)?;
    let partition = image.partition();
    let when = match env::var("SOURCE_DATE_EPOCH")
    {
        Ok(value) => mac_hfs_b::time::Timestamp::from_unix(value.parse()?, 0)?,
        Err(env::VarError::NotPresent) => hfs_b::now()?,
        Err(e) => return Err(e.into())
    };
    let mut uuid = [0; 16];
    getrandom::fill(&mut uuid).map_err(|e| error(format!("cannot create filesystem UUID: {e}")))?;
    mac_hfs_b::format(&mut image,
                      partition,
                      mac_hfs_b::FormatOptions { name: "System RISC",
                                                 created: when,
                                                 uuid },
                      Default::default())?;
    let mut volume = mac_hfs_b::Volume::mount(&mut image, partition, Default::default())?;

    // The volume's folders are the platform's installed layout, so a record
    // that installs into a new folder creates it.
    let install = &workspace.platform.install;
    let mut folders: BTreeMap<String, u32> = BTreeMap::new();
    for folder in [&install.folders.applications,
                   &install.folders.system,
                   &install.folders.drivers,
                   &install.folders.extensions,
                   &"Documents".to_string()]
    {
        create_folder(&mut folders, &mut volume, folder, when)?;
    }
    let system = *folders.get(&install.folders.system)
                         .ok_or_else(|| error("the system folder was not created"))?;
    for name in ["Fonts", "Control Panels", "Preferences", mac_abi::files::PIPES_FOLDER_NAME]
    {
        let folder = create_folder(&mut folders,
                                   &mut volume,
                                   &format!("{}/{}", install.folders.system, name),
                                   when)?;
        if name == "Fonts"
        {
            install_fonts(workspace, &mut volume, folder, when)?;
        }
        if name == "Preferences"
        {
            let file = volume.create(folder, "File Types", false, when)?.id;
            let mut finder_info = [0; 32];
            finder_info[..4].copy_from_slice(b"JSON");
            finder_info[4..8].copy_from_slice(b"TEDT");
            volume.set_finder_metadata(file, 0, finder_info, when)?;
            hfs_b::write_stream(&mut volume,
                                file,
                                mac_hfs_b::ForkKind::Data,
                                mac_file_types::DEFAULT_JSON,
                                when)?;
            // Derive the persistent key from the actual packaged EABI identity,
            // just as resident MPRE does. Display names confer no storage scope.
            if let Some(clipboard) =
                installed.iter()
                         .find(|entry| entry.package == "host-clipboard-extension")
                         .map(|entry| dist.join(format!("{}.bin", entry.name)))
            {
                let bytes = fs::read(clipboard)?;
                let module = macbinary::decode(&bytes)?;
                let resources = mac_formats::resource::decode(&module.resources)?;
                let package = mac_formats::module::Package::inspect(&resources, module.file_type)?;
                let mut buffer = [0; 37];
                let filename =
                    mac_module::preferences::filename(package.contract.identity, &mut buffer);
                let filename = core::str::from_utf8(filename)?;
                let file = volume.create(folder, filename, false, when)?.id;
                volume.set_finder_metadata(file, 0, finder_info, when)?;
                let mut bytes = [0; mac_host_clipboard::MAX_SETTINGS_BYTES];
                let length = mac_host_clipboard::Settings::default().encode(&mut bytes)?;
                hfs_b::write_stream(&mut volume,
                                    file,
                                    mac_hfs_b::ForkKind::Data,
                                    &bytes[..length],
                                    when)?;
            }
        }
    }

    let mut verified = Vec::new();
    for entry in installed
    {
        let parent = create_folder(&mut folders, &mut volume, &entry.folder, when)?;
        let bytes = fs::read(dist.join(format!("{}.bin", entry.name)))?;
        let original = macbinary::decode(&bytes)?;
        let created =
            hfs_b::import_macbinary_at(&mut volume, parent, &original, &entry.name, when)?;
        let mut data = Vec::new();
        let mut resources = Vec::new();
        hfs_b::read_stream(&mut volume, created.id, mac_hfs_b::ForkKind::Data, &mut data)?;
        hfs_b::read_stream(&mut volume, created.id, mac_hfs_b::ForkKind::Resource, &mut resources)?;
        if data != original.data
           || resources != original.resources
           || created.created.0 != u64::from(original.created) * 1_000_000
           || created.modified.0 != u64::from(original.modified) * 1_000_000
           || created.finder_info[..4] != original.file_type
           || created.finder_info[4..8] != original.creator
           || created.finder_info[8..10] != original.flags.to_be_bytes()
        {
            return Err(error(format!("HFS-B fork or metadata roundtrip failed for {}",
                                     entry.name)));
        }
        let artifact = format!("{}.bin", entry.name);
        verified.push(json!({"path":entry.path(),"package":entry.package,"cnid":created.id,
            "artifact":artifact,"artifact_sha256":package::sha256(&bytes),
            "file_type":text::decode(&original.file_type),
            "creator":text::decode(&original.creator),"data_length":data.len(),
            "resource_length":resources.len(),
            "resource_fork_sha256":package::sha256(&resources),
            "fork_and_metadata_roundtrip":"passed"}));
    }
    volume.bless(system, when)?;
    drop(volume);
    drop(image);
    // A fresh mount, not the formatter's retained state, verifies the blessing.
    let mut image = hfs_b::Image::open(path, false)?;
    let mut volume = mac_hfs_b::Volume::mount(&mut image, partition, Default::default())?;
    if volume.blessed_folder()?.id != system
    {
        return Err(error("HFS-B blessing did not survive remount"));
    }
    let checked = volume.check()?;
    let h = volume.header().clone();
    drop(volume);
    drop(image);
    let finished = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    Ok(json!({"filesystem":"System7",
        "filesystem_version":1,
        "unicode_version":"17.0.0","volume_name":h.name,"created":h.created,"modified":h.modified,
        "image_started_unix":started,"image_finished_unix":finished,"time_basis":"1904 epoch microseconds, UTC",
        "blessed_system_folder_id":system,"files":verified,"catalog_files":checked.files,"catalog_directories":checked.directories}))
}
/// Create `path` on the volume if it does not exist yet, returning its CNID.
/// Copy the fonts a default install carries into the Fonts folder.
///
/// They come from `system/assets/fonts-folder` and land as ordinary files, so
/// a user can remove or replace one like any other installed font. Nothing here
/// decides whether the Font Manager admits a font; it only places the file.
fn install_fonts(workspace: &Workspace, volume: &mut mac_hfs_b::Volume<'_, hfs_b::Image>,
                 folder: u32, when: mac_hfs_b::time::Timestamp)
                 -> Result<()>
{
    let source = workspace.root.join("system/assets/fonts-folder");
    let Ok(entries) = fs::read_dir(&source)
    else
    {
        return Ok(());
    };
    let mut fonts: Vec<_> =
        entries.filter_map(|entry| entry.ok().map(|entry| entry.path()))
               .filter(|path| {
                   path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("ttf"))
               })
               .collect();
    // Directory order differs between hosts; the image must not.
    fonts.sort();
    for path in fonts
    {
        let Some(name) = path.file_name().and_then(|name| name.to_str())
        else
        {
            return Err(error(format!("font file name is not UTF-8: {}", path.display())));
        };
        let file = volume.create(folder, name, false, when)?.id;
        let mut finder_info = [0; 32];
        finder_info[..4].copy_from_slice(b"sfnt");
        volume.set_finder_metadata(file, 0, finder_info, when)?;
        hfs_b::write_stream(volume, file, mac_hfs_b::ForkKind::Data, fs::File::open(&path)?, when)?;
    }
    Ok(())
}

fn create_folder(folders: &mut BTreeMap<String, u32>,
                 volume: &mut mac_hfs_b::Volume<'_, hfs_b::Image>, path: &str,
                 when: mac_hfs_b::time::Timestamp)
                 -> Result<u32>
{
    let mut parent = 2;
    let mut id = 2;
    let mut so_far = String::new();
    for part in path.split('/').filter(|part| !part.is_empty())
    {
        if !so_far.is_empty()
        {
            so_far.push('/');
        }
        so_far.push_str(part);
        id = match folders.get(&so_far)
        {
            Some(id) => *id,
            None =>
            {
                let created = volume.create(parent, part, true, when)?.id;
                folders.insert(so_far.clone(), created);
                created
            }
        };
        parent = id;
    }
    Ok(id)
}

