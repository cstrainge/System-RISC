//! Initial host HFS adapter. Never touches the installed classic-Mac disk.
//! Future native disk apps will use guest File Manager/block-device services.
use std::{env,
          fs::{self, File, OpenOptions},
          path::{Path, PathBuf},
          process::Command};

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

struct Hfs
{
    tools: PathBuf,
    home:  PathBuf
}
impl Hfs
{
    fn call(&self, tool: &str, args: &[&std::ffi::OsStr]) -> Result<String>
    {
        output(Command::new(self.tools.join(tool)).args(args)
                                                  .env("HOME", &self.home)
                                                  .env("TZ", "UTC"))
    }
}
impl Drop for Hfs
{
    fn drop(&mut self)
    {
        let _ = Command::new(self.tools.join("humount")).env("HOME", &self.home)
                                                        .env("TZ", "UTC")
                                                        .output();
    }
}

fn tool_path(workspace: &Workspace) -> Result<PathBuf>
{
    let tools =
        env::var_os("SYSTEM7_HFSUTILS").map(PathBuf::from).unwrap_or_else(|| {
                                                              workspace.repo.join(".toolchains/\
                                                                                   hfsutils/rootfs/\
                                                                                   usr/bin")
                                                          });
    for tool in
        ["hformat", "hmount", "hmkdir", "hattrib", "hcopy", "hls", "humount", "hdel", "hrename"]
    {
        if !tools.join(tool).is_file()
        {
            return Err(error(format!("missing {}: run python3 ../scripts/install_hfsutils.py \
                                      or set SYSTEM7_HFSUTILS",
                                     tools.join(tool).display())));
        }
    }
    Ok(tools)
}

/// Offline fixture edits, using private hfsutils mount state.
pub(super) fn edit(workspace: &Workspace, image: &Path, home: &Path,
                   commands: &[Vec<std::ffi::OsString>])
                   -> Result<()>
{
    fs::create_dir_all(home)?;
    let hfs = Hfs { tools: tool_path(workspace)?,
                    home:  home.to_path_buf() };
    hfs.call("hmount", &[image.as_os_str()])?;
    for command in commands
    {
        let tool = command[0].to_str().ok_or_else(|| error("invalid HFS command"))?;
        hfs.call(tool, &command[1..].iter().map(|s| s.as_os_str()).collect::<Vec<_>>())?;
    }
    hfs.call("humount", &[])?;
    Ok(())
}

pub fn image(workspace: &Workspace) -> Result<Value>
{
    let tools = tool_path(workspace)?;
    let scratch = Scratch::create(&workspace.build)?;
    let raw = scratch.path.join("system7.hfs");
    OpenOptions::new().write(true).create_new(true).open(&raw)?.set_len(DISK_SIZE)?;
    let home = scratch.path.join("hfs-state");
    fs::create_dir(&home)?;
    let hfs = Hfs { tools,
                    home };
    hfs.call("hformat", &["-l".as_ref(), "System RISC".as_ref(), raw.as_os_str()])?;
    for directory in [":System Folder", ":Applications", ":Documents", ":System Folder:Preferences"]
    {
        hfs.call("hmkdir", &[directory.as_ref()])?;
    }
    let mut verified = Vec::new();
    for (directory, name) in [("System Folder", "System"),
                              ("System Folder", "Finder"),
                              ("Applications", "QuickDraw Test"),
                              ("Applications", "TextEdit Exerciser")]
    {
        let source = workspace.build.join("dist").join(format!("{name}.bin"));
        let original = macbinary::decode(&fs::read(&source)?)?;
        let expected = package::inspect(&fs::read(&source)?)?;
        hfs.call("hcopy", &["-m".as_ref(), source.as_os_str(), format!(":{directory}:").as_ref()])?;
        let extracted = scratch.path.join(format!("{name}-roundtrip.bin"));
        hfs.call("hcopy",
                 &["-m".as_ref(), format!(":{directory}:{name}").as_ref(), extracted.as_os_str()])?;
        let copied = macbinary::decode(&fs::read(&extracted)?)?;
        if original.name != copied.name
           || original.file_type != copied.file_type
           || original.creator != copied.creator
           || original.data != copied.data
           || original.resources != copied.resources
        {
            return Err(error(format!("HFS fork or metadata roundtrip failed for {name}")));
        }
        if package::inspect(&fs::read(&extracted)?)?["elf_sha256"] != expected["elf_sha256"]
        {
            return Err(error("extracted native executable differs"));
        }
        verified.push(json!({"path": format!("{directory}/{name}"), "file_type": text::decode(&copied.file_type), "creator": text::decode(&copied.creator), "data_length": copied.data.len(), "resource_length": copied.resources.len(), "resource_fork_sha256": package::sha256(&copied.resources), "fork_and_metadata_roundtrip": "passed"}));
    }
    hfs.call("hattrib", &["-b".as_ref(), ":System Folder".as_ref()])?;
    let listing =
        hfs.call("hls", &["-l".as_ref(), ":System Folder".as_ref(), ":Applications".as_ref()])?;
    hfs.call("humount", &[])?;
    drop(hfs);
    let mut header = [0; 512];
    use std::io::{Read, Seek, SeekFrom};
    let mut disk = File::open(&raw)?;
    disk.seek(SeekFrom::Start(1024))?;
    disk.read_exact(&mut header)?;
    let blessed_id = u32::from_be_bytes(header[92..96].try_into().unwrap());
    if &header[..2] != b"BD" || blessed_id == 0 || disk.metadata()?.len() != DISK_SIZE
    {
        return Err(error("invalid HFS volume or missing System Folder blessing"));
    }
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
    // Existing images here are generated artifacts; active guest disks will
    // live separately as overlays when a real disk bootstrap is introduced.
    fs::rename(&raw, dist.join("system7.hfs"))?;
    fs::rename(&qcow, dist.join("system7.qcow2"))?;
    let report = json!({"raw": "build/system7/dist/system7.hfs", "qcow2": "build/system7/dist/system7.qcow2", "virtual_size": DISK_SIZE, "layout": "unpartitioned HFS", "volume_name": "System RISC", "blessed_system_folder_id": blessed_id, "files": verified, "listing": listing, "qcow2_check": "passed", "raw_qcow2_comparison": "passed", "riscv_disk_boot_available": true, "boot_rom": "build/system7/dist/bootrom.bin"});
    write_json(&dist.join("disk.json"), &report)?;
    println!("Built 500 MiB HFS/qcow2; System, Finder and Applications fork roundtrips passed.");
    Ok(report)
}
