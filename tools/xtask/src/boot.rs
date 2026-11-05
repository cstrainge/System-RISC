//! Boot payload packaging, the interactive launcher and disk boot checks.
use std::{fs::{self, File, OpenOptions},
          io::{Read, Seek, SeekFrom, Write},
          path::{Path, PathBuf},
          process::{Child, Command, Stdio},
          sync::{Arc, Mutex},
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
    let dist = workspace.dist().to_path_buf();
    fs::create_dir_all(&dist)?;
    // A machine that supplies no device tree is given one, built from its own
    // emulation profile. The blob is a build artifact rather than something
    // embedded in the bootloader: the emulator is handed it with `-dtb` and
    // passes it back in x0, so the bootloader reads one pointer whatever the
    // source and never knows where the tree came from.
    if mac_build::devicetree::generates(&workspace.platform)
    {
        let blob = mac_build::devicetree::generate_for(&workspace.platform)?;
        let path = dist.join(format!("{}.dtb", workspace.platform.name));
        fs::write(&path, &blob)?;
        println!("Generated {} for the emulated machine: {} bytes, passed to the emulator with \
                  -dtb.",
                 workspace.describe(&path),
                 blob.len());
    }
    let Some(bootloader) = workspace.platform.bootloader.clone()
    else
    {
        println!("Platform '{}' declares no bootloader; no boot payload is produced.",
                 workspace.platform.name);
        return Ok(None);
    };
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
    // A machine that fetches its first stage from flash places the payload in
    // the flash window it declares. A machine the emulator loads a bootable
    // image into has no flash window at all, and its payload goes into the
    // window that machine loads its first stage into. Both are statements the
    // record already makes; nothing here invents a window.
    let (window, medium) = match memory.flash()
    {
        Some(flash) => (flash, "flash"),
        None => (memory.boot_window(), "load")
    };
    let window_bytes = window.end - window.start;
    let flattened = elf::flatten(&fs::read(&elf)?, window_bytes)?;
    let rom = dist.join(format!("{}.bin", bootloader.artifact));
    fs::write(&rom, &flattened.bytes)?;
    OpenOptions::new().write(true).open(&rom)?.set_len(window_bytes)?;
    let elf_copy = dist.join(format!("{}.elf", bootloader.artifact));
    fs::copy(&elf, &elf_copy)?;
    let report = json!({"image": workspace.describe(&rom),
                        "elf": workspace.describe(&elf_copy),
                        "bootloader": bootloader.package,
                        "medium": medium,
                        "window_base": window.start,
                        "window_bytes": window_bytes,
                        "load_base": flattened.base,
                        "entry": flattened.entry,
                        "payload_size": flattened.bytes.len(),
                        "sha256": package::sha256(&fs::read(&rom)?),
                        "source": bootloader.package,
                        "execution": bootloader.description});
    write_json(&dist.join(format!("{}.json", bootloader.artifact)), &report)?;
    println!("Packaged {}: {} bytes at 0x{:x} in the {medium} window (0x{:x} bytes).",
             bootloader.package,
             flattened.bytes.len(),
             flattened.base,
             window_bytes);
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
///
/// The console is copied to `<platform>/serial.log` as it arrives, so a run
/// leaves evidence a reader can inspect afterwards. The record still decides
/// where the guest's console goes; this only keeps a copy of what the host saw.
pub fn run(workspace: &Workspace, mode: emulator::Mode, media: &[PathBuf], extra: &[String])
           -> Result<()>
{
    // No media named means the machine the record describes; media named means
    // those disks instead.
    let media =
        if media.is_empty() { emulator::Media::Declared } else { emulator::Media::Given(media) };
    let invocation = emulator::invocation(&workspace.platform,
                                          &workspace.root,
                                          workspace.dist(),
                                          media,
                                          mode,
                                          extra)?;
    println!("Booting {} through {}. Ctrl+C stops the emulator.",
             workspace.platform
                      .emulation()
                      .map(|emulation| emulation.description.as_str())
                      .unwrap_or("the declared machine"),
             invocation.describe());
    let log = workspace.dist().join("serial.log");
    let mut recording = File::create(&log)?;
    let mut command = invocation.command();
    command.stdout(Stdio::piped());
    let mut child = command.spawn()?;
    if let Some(mut console) = child.stdout.take()
    {
        let mut buffer = [0u8; 4096];
        loop
        {
            match console.read(&mut buffer)
            {
                Ok(0) | Err(_) => break,
                Ok(count) =>
                {
                    let chunk = &buffer[..count];
                    let _ = std::io::stdout().write_all(chunk);
                    let _ = std::io::stdout().flush();
                    recording.write_all(chunk)?;
                }
            }
        }
    }
    recording.flush()?;
    let status = child.wait()?;
    println!("Console log: {}", log.display());
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
    // Each case states the disks it is about; the record's own medium is not
    // attached underneath it.
    //
    // The console is read from a pipe rather than written by QEMU into a file:
    // QEMU's file chardev buffers, so a reader watching the file sees nothing
    // until a buffer fills or the emulator exits, which would make readiness
    // look like a stall. Reading the pipe and writing the file here means the
    // log is complete as it arrives.
    let invocation = emulator::invocation(&workspace.platform,
                                          &workspace.root,
                                          workspace.dist(),
                                          emulator::Media::Given(media),
                                          emulator::Mode::Headless,
                                          &[])?;
    let mut command = invocation.command();
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(File::create(&stderr)?);
    let launch = emulator::render(&invocation);
    let mut child = Guest(command.spawn()?);
    let mut console =
        child.0.stdout.take().ok_or_else(|| error(format!("no console pipe for {name}")))?;
    let mut recording = File::create(&serial)?;
    let collected = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&collected);
    // A console that cannot be read or written is a failure of the recorder,
    // not of the guest. It is reported here rather than left to look like a
    // boot that never reached its marker on a log this process truncated.
    let unrecorded: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let recorder = Arc::clone(&unrecorded);
    let log_path = serial.clone();
    thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop
        {
            let count = match console.read(&mut buffer)
            {
                Ok(0) => break,
                Ok(count) => count,
                Err(why) =>
                {
                    if let Ok(mut slot) = recorder.lock()
                    {
                        *slot = Some(format!("cannot read the guest console: {why}"));
                    }
                    break;
                }
            };
            // The in-memory copy is what readiness is judged from, so it is
            // updated even when the file write below fails.
            if let Ok(mut log) = sink.lock()
            {
                log.push_str(&String::from_utf8_lossy(&buffer[..count]));
            }
            if let Err(why) = recording.write_all(&buffer[..count]).and_then(|()| recording.flush())
            {
                if let Ok(mut slot) = recorder.lock()
                {
                    *slot = Some(format!("cannot write {}: {why}", log_path.display()));
                }
                break;
            }
        }
    });
    let started = Instant::now();
    let deadline = started + invocation.timeout;
    let log = loop
    {
        let log = collected.lock().map(|log| log.clone()).unwrap_or_default();
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
            return Err(error(format!("ROM check {name} failed after {:?}: {}; launched as \
                                      {launch}; see {} and {}",
                                     started.elapsed(),
                                     if failing
                                     {
                                         "the guest reported a failure"
                                     }
                                     else if child.0.try_wait()?.is_some()
                                     {
                                         "the emulator exited before reaching the readiness marker"
                                     }
                                     else
                                     {
                                         "the readiness marker did not arrive within the \
                                          platform's timeout"
                                     },
                                     serial.display(),
                                     stderr.display())));
        }
        thread::sleep(std::time::Duration::from_millis(20));
    };
    drop(child);
    if let Some(why) = unrecorded.lock().ok().and_then(|slot| slot.clone())
    {
        return Err(error(format!("the console log for {name} was not recorded: {why}")));
    }
    // `boots` is about the guest OS starting, which is a different fact from the
    // ROM reaching a terminal state: a machine with no disk is expected to say
    // so and stop, and that is a pass, not a boot.
    let started = log.contains("BOOT: entering System");
    if expected.iter().any(|marker| !log.contains(marker)) || started != boots
    {
        return Err(error(format!("unexpected ROM result for {name}:\n{log}")));
    }
    println!("ROM check {name}: passed");
    Ok(json!({"name": name, "status": "passed", "booted": boots, "command": launch, "expected_markers": expected, "serial": workspace.describe(&serial), "log": log}))
}
fn clone_raw(source: &Path, destination: &Path) -> Result<()>
{
    run_command(Command::new(crate::tools::resolve("qemu-img")?).args(["convert", "-f", "raw",
                                                                       "-O", "raw", "-S", "4k"])
                                                                .arg(source)
                                                                .arg(destination))
}


fn altered_system(workspace: &Workspace, image: &Path, alteration: &str) -> Result<()>
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
    let directory = workspace.dist().join("boot-checks");
    fs::create_dir_all(&directory)?;
    let raw = workspace.disk_image()?;
    // A disk image format is an emulator convenience, so it is produced here,
    // on the way to the guest, rather than in the build. The conversion tool is
    // a host extra, so its absence skips that one case and says so, rather than
    // failing the checks that only need the build's own output.
    let qcow = directory.join("system7.qcow2");
    let mut skipped: Vec<Value> = Vec::new();
    let converted = match crate::tools::resolve("qemu-img")
    {
        Ok(tool) =>
        {
            run_command(Command::new(tool).args(["convert", "-f", "raw", "-O", "qcow2"])
                                          .arg(&raw)
                                          .arg(&qcow))?;
            Some(package::sha256(&fs::read(&qcow)?))
        }
        Err(why) =>
        {
            skipped.push(json!({"name": "qcow2-hfs-b", "status": "skipped",
                                "reason": why.to_string()}));
            None
        }
    };
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
                              // The catalog number is not asserted: it shifts whenever
                              // the volume's contents change, and what matters is that a
                              // blessed System Folder was found at all.
                              &["System RISC Boot ROM v1 at 0x200",
                                "blessed folder 'System Folder' CNID=",
                                "System: native service table ready", "BOOT_SYSTEM_READY:"],
                              true)?];
    if converted.is_some()
    {
        cases.push(case(workspace,
                        &directory,
                        "qcow2-hfs-b",
                        &[qcow.clone()],
                        &["BOOT_SYSTEM_READY:"],
                        true)?);
    }
    let large = directory.join("large-system-fork.hfsb");
    clone_raw(&raw, &large)?;
    altered_system(workspace, &large, "large-fork")?;
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
                    &["blessed folder 'Renamed Startup' CNID=", "BOOT_SYSTEM_READY:"],
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
        altered_system(workspace, &path, alteration)?;
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
    if let Some(before) = &converted
    {
        if package::sha256(&fs::read(&qcow)?) != *before
        {
            return Err(error("boot checks changed the generated qcow2"));
        }
    }
    let report = json!({"status": "passed", "method": "QEMU cold reset to read-only pflash; virtio-block reads HFS-B and loads System, Finder, WDEF and CDEF resources entirely inside the guest", "cases": cases, "skipped": skipped, "disk_boot_tested": true, "finder_launched": true, "rom": serde_json::from_slice::<Value>(&fs::read(bootloader_report(workspace)?)?)?, "qcow2_unchanged": converted.is_some(), "qemu": output(Command::new(tools::resolve(&workspace.platform.require_emulation()?.binary)?).arg("--version"))?});
    write_json(&workspace.results().join("system7/boot-checks.json"), &report)?;
    for entry in &skipped
    {
        println!("Skipped {}: {}",
                 entry["name"].as_str().unwrap_or("a case"),
                 entry["reason"].as_str().unwrap_or("no reason recorded"));
    }
    println!("{} ROM boot checks passed. Serial logs and reports: {}",
             cases.len(),
             directory.display());
    Ok(report)
}
