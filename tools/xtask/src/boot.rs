//! Boot payload packaging, the interactive launcher and disk boot checks.
use std::{fs::{self, File, OpenOptions},
          io::{Read, Seek, SeekFrom, Write},
          path::{Path, PathBuf},
          process::{Child, Command, Stdio},
          thread,
          time::Instant};

use mac_build::{Result, error, package};
use mac_formats::{macbinary, native::Descriptor, resource};
use serde_json::{Value, json};

use crate::{Workspace, disk, elf, emulator, output, run as run_command, tools, write_json};

/// The boot payload this platform produces, derived from the bootloader its
/// record declares. Nothing here names a package or an artifact.
pub fn payload(platform: &mac_build::platform::Platform, dist: &Path) -> Result<PathBuf>
{
    let bootloader =
        platform.bootloader.as_ref().ok_or_else(|| {
                                         error(format!("platform '{}' declares no bootloader, so \
                                                        it produces no boot payload. Declare a \
                                                        `bootloader` section naming the package \
                                                        that builds one, or build the medium \
                                                        with `cargo xtask image` for a platform \
                                                        whose first stage is vendor firmware.",
                                                       platform.name))
                                     })?;
    Ok(dist.join(format!("{}.bin", bootloader.artifact)))
}

/// A platform storage image, by the name the record gives it.
pub fn media(platform: &mac_build::platform::Platform, dist: &Path, image: &str)
             -> Result<PathBuf>
{
    let storage = platform.storage.as_ref().ok_or_else(|| {
                                                error(format!("platform '{}' attaches storage \
                                                               image '{image}' but declares no \
                                                               storage section",
                                                              platform.name))
                                            })?;
    let medium =
        storage.images.get(image).ok_or_else(|| {
                                      let declared: Vec<&str> =
                                          storage.images.keys().map(String::as_str).collect();
                                      error(format!("platform '{}' attaches storage image \
                                                     '{image}', which it does not declare; it \
                                                     declares: {}",
                                                    platform.name,
                                                    declared.join(", ")))
                                  })?;
    Ok(dist.join(medium.file(image)))
}

pub fn package_rom(workspace: &Workspace) -> Result<Option<Value>>
{
    let Some(bootloader) = workspace.platform.bootloader.clone()
    else
    {
        println!("Platform '{}' declares no bootloader; no boot payload is produced.",
                 workspace.platform.name);
        return Ok(None);
    };
    let dist = workspace.dist().to_path_buf();
    fs::create_dir_all(&dist)?;
    let elf = workspace.target()
                       .join(&workspace.platform.rust_target)
                       .join("release")
                       .join(&bootloader.package);
    if !elf.is_file()
    {
        return Err(error(format!("platform '{}' declares bootloader package '{}', which did \
                                  not produce {}; build that package or correct the platform \
                                  record",
                                 workspace.platform.name,
                                 bootloader.package,
                                 elf.display())));
    }
    let memory = workspace.platform.memory()?;
    let flattened = elf::flatten(&fs::read(&elf)?, memory.flash_bytes())?;
    let rom = dist.join(format!("{}.bin", bootloader.artifact));
    fs::write(&rom, &flattened.bytes)?;
    OpenOptions::new().write(true).open(&rom)?.set_len(memory.flash_bytes())?;
    let elf_copy = dist.join(format!("{}.elf", bootloader.artifact));
    fs::copy(&elf, &elf_copy)?;
    let report = json!({"image": workspace.describe(&rom),
                        "elf": workspace.describe(&elf_copy),
                        "bootloader": bootloader.package,
                        "flash_base": memory.flash_base(),
                        "load_base": flattened.base,
                        "entry": flattened.entry,
                        "payload_size": flattened.bytes.len(),
                        "flash_bank_size": memory.flash_bytes(),
                        "sha256": package::sha256(&fs::read(&rom)?),
                        "source": format!("System7/{}", bootloader.package),
                        "execution": bootloader.description});
    write_json(&dist.join(format!("{}.json", bootloader.artifact)), &report)?;
    println!("Packaged {}: {} bytes at 0x{:x} in {} MiB platform flash bank.",
             bootloader.package,
             flattened.bytes.len(),
             flattened.base,
             memory.flash_bytes() / (1024 * 1024));
    Ok(Some(report))
}

/// The packaging report for the declared bootloader, if the platform has one.
pub fn bootloader_report(workspace: &Workspace) -> Result<PathBuf>
{
    let bootloader =
        workspace.platform.bootloader.as_ref().ok_or_else(|| {
                                                   error(format!("platform '{}' declares no \
                                                                  bootloader, so there is no \
                                                                  packaging report",
                                                                 platform_name(workspace)))
                                               })?;
    Ok(workspace.dist().join(format!("{}.json", bootloader.artifact)))
}

fn platform_name(workspace: &Workspace) -> &str
{
    &workspace.platform.name
}

pub fn disk_format(path: &Path) -> Result<&'static str>
{
    let mut magic = [0; 4];
    File::open(path)?.read_exact(&mut magic)?;
    Ok(if magic == *b"QFI\xfb" { "qcow2" } else { "raw" })
}

/// Start the platform's machine. The invocation, the device set and the
/// readiness contract all come from the platform record.
pub fn run(workspace: &Workspace, mode: emulator::Mode, media: &[PathBuf], extra: &[String])
           -> Result<()>
{
    let invocation = emulator::invocation(&workspace.platform,
                                          &workspace.root,
                                          workspace.dist(),
                                          media,
                                          mode,
                                          emulator::Serial::Stdio,
                                          extra)?;
    println!("Booting {} through {}. Ctrl+C stops the emulator.",
             workspace.platform
                      .emulation()
                      .map(|emulation| emulation.description.as_str())
                      .unwrap_or("the declared machine"),
             invocation.describe());
    let status = invocation.command().status()?;
    if !status.success()
    {
        return Err(error(format!("emulator exited with {status}; the boot payload is {}",
                                 payload(&workspace.platform, workspace.dist())?.display())));
    }
    Ok(())
}

struct Guest(Child);
impl Drop for Guest
{
    fn drop(&mut self)
    {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn case(workspace: &Workspace, directory: &Path, name: &str, media: &[PathBuf], expected: &[&str],
        boots: bool)
        -> Result<Value>
{
    let serial = directory.join(format!("{name}.serial.log"));
    let stderr = directory.join(format!("{name}.qemu.log"));
    // Do not accept a readiness marker left by a previous invocation while the
    // new QEMU process is still opening its serial backend.
    File::create(&serial)?;
    let invocation = emulator::invocation(&workspace.platform,
                                          &workspace.root,
                                          workspace.dist(),
                                          media,
                                          emulator::Mode::Headless,
                                          emulator::Serial::File(serial.clone()),
                                          &[])?;
    let mut command = invocation.command();
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(File::create(&stderr)?);
    let launch = emulator::render(&invocation);
    let mut child = Guest(command.spawn()?);
    let deadline = Instant::now() + invocation.timeout;
    let log = loop
    {
        let log = fs::read_to_string(&serial).unwrap_or_default();
        // UART output can be observed halfway through a formatted line. Wait
        // for its newline before validating markers or stopping the guest.
        if log.split_inclusive('\n').any(|line| line.ends_with('\n') && invocation.booted_by(line))
        {
            break log;
        }
        let failing = log.split_inclusive('\n')
                         .any(|line| line.ends_with('\n') && invocation.failed_by(line));
        if (failing && !expected.iter().any(|marker| invocation.failed_by(marker)))
           || Instant::now() >= deadline
           || child.0.try_wait()?.is_some()
        {
            return Err(error(format!("ROM check {name} failed; see {} and {}",
                                     serial.display(),
                                     stderr.display())));
        }
        thread::sleep(std::time::Duration::from_millis(20));
    };
    drop(child);
    if expected.iter().any(|marker| !log.contains(marker))
       || invocation.booted_by(&log) != boots
       || (!boots
           && log.contains("BOOT: entering System")
           && !expected.iter().any(|marker| invocation.failed_by(marker)))
    {
        return Err(error(format!("unexpected ROM result for {name}:\n{log}")));
    }
    println!("ROM check {name}: passed");
    Ok(json!({"name": name, "status": "passed", "booted": boots, "command": launch, "expected_markers": expected, "serial": workspace.describe(&serial), "log": log}))
}
fn clone_raw(source: &Path, destination: &Path) -> Result<()>
{
    run_command(Command::new("qemu-img").args(["convert", "-f", "raw", "-O", "raw", "-S", "4k"])
                                        .arg(source)
                                        .arg(destination))
}


fn altered_system(workspace: &Workspace, directory: &Path, image: &Path, alteration: &str)
                  -> Result<()>
{
    let source = workspace.dist().join("System.bin");
    let mut file = macbinary::decode(&fs::read(source)?)?;
    let mut resources = resource::decode(&file.resources)?;
    match alteration
    {
        "large-fork" =>
        {
            // Grow a real payload rather than appending bytes outside RRF1's
            // declared regions. Native maps must end exactly at EOF.
            // ROM selects executable resources; System must load this additional
            // resource within its explicit selected-payload memory budget.
            // A fixed fixture increment is not a production quota. Startup now
            // admits against available resident backing, so this test must not
            // derive its payload from the unrelated ordinary-app decode cap.
            let extra =
                (64 * 1024).max((5 * 1024 * 1024 + 17usize).saturating_sub(file.resources.len()));
            resources.push(resource::Resource { kind:       *b"TEST",
                                                id:         32760,
                                                name:
                                                    Some(b"Large-fork boot acceptance".to_vec()),
                                                attributes: 0,
                                                data:       vec![0x5a; extra] });
        }
        "code" => resources.iter_mut().find(|r| r.kind == *b"RV64").unwrap().data[0] = 0,
        "elf-resource" =>
        {
            resources.iter_mut().find(|r| r.kind == *b"RV64").unwrap().data =
                fs::read(workspace.dist().join("System.runtime.elf"))?;
        }
        "obsolete-nabi" =>
        {
            resources.iter_mut().find(|r| r.kind == *b"NABI").unwrap().data.truncate(32)
        }
        "role" | "address" =>
        {
            let abi = resources.iter_mut().find(|r| r.kind == *b"NABI").unwrap();
            let mut descriptor = Descriptor::decode(&abi.data)?;
            if alteration == "role"
            {
                descriptor.role = mac_formats::native::Role::Application;
            }
            else
            {
                descriptor.load_base = 0x80000000;
            }
            abi.data = descriptor.encode()?.to_vec();
        }
        _ => return Err(error("unknown System alteration"))
    }
    file.resources = resource::encode_native(&resources)?;
    let _ = directory;
    disk::edit(image, |volume| {
        let folder = volume.blessed_folder()?.id;
        let old = volume.lookup(folder, "System")?;
        volume.remove(old.id, hfs_b::now()?)?;
        hfs_b::import_macbinary(volume, folder, &file)?;
        Ok(())
    })
}

/// Wrap the generated volume in an actual Apple partition map.
fn apm(source: &Path, destination: &Path) -> Result<()>
{
    let mut input = File::open(source)?;
    let length = input.metadata()?.len();
    let mut out = File::create(destination)?;
    out.set_len(length + 1024 * 1024)?;
    let mut buffer = vec![0; 1024 * 1024];
    out.seek(SeekFrom::Start(1024 * 1024))?;
    loop
    {
        let count = input.read(&mut buffer)?;
        if count == 0
        {
            break;
        }
        if buffer[..count].iter().any(|&b| b != 0)
        {
            out.write_all(&buffer[..count])?;
        }
        else
        {
            out.seek(SeekFrom::Current(count as i64))?;
        }
    }
    for (index, start, sectors, kind) in [(1u64, 1u32, 2u32, "Apple_partition_map"),
                                          (2, 2048, (length / 512) as u32, "System_RISC_HFS_B")]
    {
        let mut entry = [0; 512];
        entry[..2].copy_from_slice(b"PM");
        entry[4..8].copy_from_slice(&2u32.to_be_bytes());
        entry[8..12].copy_from_slice(&start.to_be_bytes());
        entry[12..16].copy_from_slice(&sectors.to_be_bytes());
        entry[48..48 + kind.len()].copy_from_slice(kind.as_bytes());
        entry[84..88].copy_from_slice(&sectors.to_be_bytes());
        entry[88..92].copy_from_slice(&0x37u32.to_be_bytes());
        out.seek(SeekFrom::Start(index * 512))?;
        out.write_all(&entry)?;
    }
    let mut descriptor = [0; 512];
    descriptor[..2].copy_from_slice(b"ER");
    descriptor[2..4].copy_from_slice(&512u16.to_be_bytes());
    descriptor[4..8].copy_from_slice(&((length / 512 + 2048) as u32).to_be_bytes());
    out.seek(SeekFrom::Start(0))?;
    out.write_all(&descriptor)?;
    Ok(())
}

pub fn check(workspace: &Workspace) -> Result<Value>
{
    let directory = workspace.build.join("boot-checks");
    fs::create_dir_all(&directory)?;
    let raw = workspace.dist().join("system7.hfsb");
    let qcow = workspace.dist().join("system7.qcow2");
    let hash_before = package::sha256(&fs::read(&qcow)?);
    let mut cases = vec![case(workspace,
                              &directory,
                              "no-disk",
                              &[],
                              &["BOOT_NO_SYSTEM: searched 0 disk(s)"],
                              false)?,
                         case(workspace,
                              &directory,
                              "raw-hfs-b",
                              &[raw.clone()],
                              &["System RISC Boot ROM v1 at 0x200", "CNID=16",
                                "System: native service table ready", "BOOT_SYSTEM_READY:"],
                              true)?,
                         case(workspace,
                              &directory,
                              "qcow2-hfs-b",
                              &[qcow.clone()],
                              &["BOOT_SYSTEM_READY:"],
                              true)?,];
    let large = directory.join("large-system-fork.hfsb");
    clone_raw(&raw, &large)?;
    altered_system(workspace, &directory, &large, "large-fork")?;
    cases.push(case(workspace,
                    &directory,
                    "system-resource-fork-over-4mib",
                    &[large.clone()],
                    &["BOOT_SYSTEM_READY:", "DESKTOP_READY:"],
                    true)?);
    let classic = workspace.root.join("crates/mac-toolbox/tests/data/files/dates-2026.hfs");
    cases.push(case(workspace,
                    &directory,
                    "classic-hfs-is-not-native-startup",
                    &[classic.clone()],
                    &["native boot requires HFS-B; recreate System disk", "BOOT_NO_SYSTEM:"],
                    false)?);
    let renamed = directory.join("renamed.hfsb");
    clone_raw(&raw, &renamed)?;
    disk::edit(&renamed, |v| {
        let id = v.blessed_folder()?.id;
        v.rename(id, 2, "Renamed Startup", hfs_b::now()?)?;
        Ok(())
    })?;
    cases.push(case(workspace,
                    &directory,
                    "renamed-blessed-folder",
                    &[renamed.clone()],
                    &["blessed folder 'Renamed Startup' CNID=16", "BOOT_SYSTEM_READY:"],
                    true)?);
    let unblessed = directory.join("unblessed.hfsb");
    clone_raw(&raw, &unblessed)?;
    disk::edit(&unblessed, |v| {
        v.bless(0, hfs_b::now()?)?;
        Ok(())
    })?;
    cases.push(case(workspace,
                    &directory,
                    "unblessed",
                    &[unblessed.clone()],
                    &["blessed System Folder CNID is missing", "BOOT_NO_SYSTEM:"],
                    false)?);
    let missing = directory.join("missing.hfsb");
    clone_raw(&raw, &missing)?;
    disk::edit(&missing, |v| {
        let parent = v.blessed_folder()?.id;
        let id = v.lookup(parent, "System")?.id;
        v.remove(id, hfs_b::now()?)?;
        Ok(())
    })?;
    cases.push(case(workspace,
                    &directory,
                    "missing-system",
                    &[missing.clone()],
                    &["startup file is missing", "BOOT_NO_SYSTEM:"],
                    false)?);
    let missing_finder = directory.join("missing-finder.hfsb");
    clone_raw(&raw, &missing_finder)?;
    disk::edit(&missing_finder, |v| {
        let parent = v.blessed_folder()?.id;
        let id = v.lookup(parent, "Finder")?.id;
        v.remove(id, hfs_b::now()?)?;
        Ok(())
    })?;
    cases.push(case(workspace,
                    &directory,
                    "missing-finder",
                    &[missing_finder.clone()],
                    &["startup file is missing", "BOOT_NO_SYSTEM:"],
                    false)?);
    for (alteration, expected) in
        [("code", "invalid RV64 segment resource header"),
         ("elf-resource", "ELF-backed code resources are unsupported"),
         ("obsolete-nabi", "obsolete ELF-backed NABI layout"),
         ("role", "not a compatible System"),
         ("address", "System execution reservation disagrees with the selected virtual platform")]
    {
        let path = directory.join(format!("bad-{alteration}.hfsb"));
        clone_raw(&raw, &path)?;
        altered_system(workspace, &directory, &path, alteration)?;
        cases.push(case(workspace,
                        &directory,
                        &format!("bad-{alteration}"),
                        &[path.clone()],
                        &[expected, "BOOT_NO_SYSTEM:"],
                        false)?);
    }
    let partitioned = directory.join("partitioned.hfsb");
    apm(&raw, &partitioned)?;
    cases.push(case(workspace,
                    &directory,
                    "apple-partition-map",
                    &[partitioned.clone()],
                    &["at sector 2048", "BOOT_SYSTEM_READY:"],
                    true)?);
    // Explicit bus placement makes the unblessed disk the first one scanned.
    cases.push(case(workspace,
                    &directory,
                    "disk-fallback",
                    &[unblessed.clone(), raw.clone()],
                    &["blessed System Folder CNID is missing", "BOOT_SYSTEM_READY:"],
                    true)?);
    let malformed = directory.join("bad-code.hfsb");
    cases.push(case(workspace,
                    &directory,
                    "invalid-system-fallback",
                    &[malformed.clone(), raw.clone()],
                    &["invalid RV64 segment resource header", "BOOT_SYSTEM_READY:"],
                    true)?);
    if package::sha256(&fs::read(&qcow)?) != hash_before
    {
        return Err(error("boot checks changed the generated qcow2"));
    }
    let report = json!({"status": "passed", "method": "QEMU cold reset to read-only pflash; virtio-block reads HFS-B and loads System, Finder, WDEF and CDEF resources entirely inside the guest", "cases": cases, "disk_boot_tested": true, "finder_launched": true, "rom": serde_json::from_slice::<Value>(&fs::read(bootloader_report(workspace)?)?)?, "qcow2_unchanged": true, "qemu": output(Command::new(tools::resolve(&workspace.platform.require_emulation()?.binary)?).arg("--version"))?});
    write_json(&workspace.repo.join("results/system7/boot-checks.json"), &report)?;
    println!("{} ROM boot checks passed.", cases.len());
    Ok(report)
}
