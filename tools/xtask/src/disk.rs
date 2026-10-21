//! Native System-volume builder using the shared HFS-B filesystem.
use std::{env,
          fs::{self, File},
          path::{Path, PathBuf},
          process::Command,
          time::{SystemTime, UNIX_EPOCH}};

use mac_build::{Result, error, package};
use mac_formats::{macbinary, text};
use serde_json::{Value, json};

use crate::{Workspace, output, run, write_json};

const DISK_SIZE: u64 = 500 * 1024 * 1024;

/// Serialize image builds without altering global hfsutils mount state.
struct Scratch
{
    path: PathBuf
}
impl Scratch
{
    fn create(build: &Path) -> Result<Self>
    {
        fs::create_dir_all(build)?;
        let path = build.join("image-work");
        fs::create_dir(&path).map_err(|e| {
                                 error(format!("cannot acquire image workspace {}: {e}; another \
                                                build may be active",
                                               path.display()))
                             })?;
        Ok(Self { path })
    }
}
impl Drop for Scratch
{
    fn drop(&mut self)
    {
        let _ = fs::remove_dir_all(&self.path);
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

pub fn image(workspace: &Workspace) -> Result<Value>
{
    let started = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let scratch = Scratch::create(&workspace.build)?;
    let raw = scratch.path.join("system7.hfsb");
    let mut image = hfs_b::Image::create(&raw, DISK_SIZE)?;
    let partition = image.partition();
    let when = match env::var("SOURCE_DATE_EPOCH")
    {
        Ok(value) => mac_hfs_b::time::Timestamp::from_unix(value.parse()?, 0)?,
        Err(env::VarError::NotPresent) => hfs_b::now()?,
        Err(e) => return Err(e.into())
    };
    let mut uuid = [0; 16];
    use std::io::Read;
    File::open("/dev/urandom")?.read_exact(&mut uuid)?;
    mac_hfs_b::format(&mut image,
                      partition,
                      mac_hfs_b::FormatOptions { name: "System RISC",
                                                 created: when,
                                                 uuid },
                      Default::default())?;
    let mut volume = mac_hfs_b::Volume::mount(&mut image, partition, Default::default())?;
    let system = volume.create(2, "System Folder", true, when)?.id;
    let applications = volume.create(2, "Applications", true, when)?.id;
    volume.create(2, "Documents", true, when)?;
    let drivers = volume.create(system, "Drivers", true, when)?.id;
    for name in ["Extensions", "Fonts", "Control Panels", "Preferences"]
    {
        let folder = volume.create(system, name, true, when)?.id;
        if name == "Preferences"
        {
            let file = volume.create(folder, "File Types.json", false, when)?.id;
            hfs_b::write_stream(&mut volume,
                                file,
                                mac_hfs_b::ForkKind::Data,
                                mac_file_types::DEFAULT_JSON,
                                when)?;
        }
    }
    let mut verified = Vec::new();
    for (parent, directory, name) in [(system, "System Folder", "System"),
                                      (system, "System Folder", "Finder"),
                                      (applications, "Applications", "QuickDraw Test"),
                                      (applications, "Applications", "TextEdit Exerciser"),
                                      (applications, "Applications", "Cursor Test"),
                                      (applications, "Applications", "Panic Test"),
                                      (applications, "Applications", "ResEdit"),
                                      (drivers, "System Folder/Drivers", "Host Transport"),
                                      (drivers, "System Folder/Drivers", "Host Filesystem"),
                                      (drivers, "System Folder/Drivers", "StuffIt Filesystem")]
    {
        let bytes = fs::read(workspace.build.join("dist").join(format!("{name}.bin")))?;
        let original = macbinary::decode(&bytes)?;
        let entry = hfs_b::import_macbinary_at(&mut volume, parent, &original, name, when)?;
        let mut data = Vec::new();
        let mut resources = Vec::new();
        hfs_b::read_stream(&mut volume, entry.id, mac_hfs_b::ForkKind::Data, &mut data)?;
        hfs_b::read_stream(&mut volume, entry.id, mac_hfs_b::ForkKind::Resource, &mut resources)?;
        if data != original.data
           || resources != original.resources
           || entry.created.0 != u64::from(original.created) * 1_000_000
           || entry.modified.0 != u64::from(original.modified) * 1_000_000
           || entry.finder_info[..4] != original.file_type
           || entry.finder_info[4..8] != original.creator
           || entry.finder_info[8..10] != original.flags.to_be_bytes()
        {
            return Err(error(format!("HFS-B fork or metadata roundtrip failed for {name}")));
        }
        verified.push(json!({"path":format!("{directory}/{name}"),"cnid":entry.id,"file_type":text::decode(&original.file_type),
            "creator":text::decode(&original.creator),"data_length":data.len(),"resource_length":resources.len(),
            "resource_fork_sha256":package::sha256(&resources),"fork_and_metadata_roundtrip":"passed"}));
    }
    volume.bless(system, when)?;
    let checked = volume.check()?;
    let h = volume.header().clone();
    drop(volume);
    drop(image);
    // A fresh mount, not the formatter's retained state, verifies the blessing.
    let mut image = hfs_b::Image::open(&raw, false)?;
    let mut volume = mac_hfs_b::Volume::mount(&mut image, partition, Default::default())?;
    if volume.blessed_folder()?.id != system
    {
        return Err(error("HFS-B blessing did not survive remount"));
    }
    volume.check()?;
    drop(volume);
    drop(image);
    let qcow = scratch.path.join("system7.qcow2");
    run(Command::new("qemu-img").args(["convert", "-f", "raw", "-O", "qcow2"])
                                .arg(&raw)
                                .arg(&qcow))?;
    run(Command::new("qemu-img").arg("check").arg(&qcow))?;
    run(Command::new("qemu-img").args(["compare", "-f", "raw", "-F", "qcow2"])
                                .arg(&raw)
                                .arg(&qcow))?;
    let qcow_info: Value =
        serde_json::from_str(&output(Command::new("qemu-img").args(["info", "--output=json"])
                                                             .arg(&qcow))?)?;
    if qcow_info["virtual-size"].as_u64() != Some(DISK_SIZE)
    {
        return Err(error("qcow2 virtual size mismatch"));
    }
    let dist = workspace.build.join("dist");
    fs::rename(&raw, dist.join("system7.hfsb"))?;
    fs::rename(&qcow, dist.join("system7.qcow2"))?;
    let finished = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let report = json!({"raw":workspace.describe(&dist.join("system7.hfsb")),"qcow2":workspace.describe(&dist.join("system7.qcow2")),
        "virtual_size":DISK_SIZE,"layout":"unpartitioned HFS-B","filesystem":"HFS-B","filesystem_version":1,
        "unicode_version":"17.0.0","volume_name":h.name,"created":h.created,"modified":h.modified,
        "image_started_unix":started,"image_finished_unix":finished,"time_basis":"1904 epoch microseconds, UTC",
        "blessed_system_folder_id":system,"files":verified,"catalog_files":checked.files,"catalog_directories":checked.directories,
        "qcow2_check":"passed","raw_qcow2_comparison":"passed","riscv_disk_boot_available":true,"boot_rom":workspace.describe(&dist.join("bootrom.bin"))});
    write_json(&dist.join("disk.json"), &report)?;
    println!("Built 500 MiB HFS-B v1/qcow2; all installed forks, metadata, blessing and \
              structural checks passed.");
    Ok(report)
}
