//! Flash-ROM packaging, serial-console launch and actual disk boot checks.
use std::{fs::{self, File, OpenOptions},
          io::{Read, Seek, SeekFrom, Write},
          path::Path,
          process::{Child, Command, Stdio},
          thread,
          time::{Duration, Instant}};

use mac_build::{Result, error, package};
use mac_formats::{macbinary, native::Descriptor, resource};
use serde_json::{Value, json};

use crate::{Workspace, disk, output, run, write_json};

pub fn package_rom(workspace: &Workspace) -> Result<Value>
{
    let dist = workspace.build.join("dist");
    fs::create_dir_all(&dist)?;
    let elf = workspace.build
                       .join("target")
                       .join(&workspace.platform.rust_target)
                       .join("release/bootrom");
    let rom = dist.join("bootrom.bin");
    run(Command::new("llvm-objcopy").args(["-O", "binary"]).arg(&elf).arg(&rom))?;
    let length = rom.metadata()?.len();
    let memory = workspace.platform.memory()?;
    if length == 0 || length > memory.flash_bytes()
    {
        return Err(error("ROM payload does not fit the selected platform flash bank"));
    }
    OpenOptions::new().write(true).open(&rom)?.set_len(memory.flash_bytes())?;
    fs::copy(&elf, dist.join("bootrom.elf"))?;
    let report = json!({"image": workspace.describe(&rom), "elf": workspace.describe(&dist.join("bootrom.elf")), "flash_base": memory.flash_base(), "payload_size": length, "flash_bank_size": memory.flash_bytes(), "sha256": package::sha256(&fs::read(rom)?), "source": "System7/bootrom", "execution": "M-mode execute-in-place from read-only QEMU virt pflash"});
    write_json(&dist.join("bootrom.json"), &report)?;
    println!("Packaged boot ROM: {length} bytes in {} MiB platform flash bank.",
             memory.flash_bytes() / (1024 * 1024));
    Ok(report)
}

pub fn disk_format(path: &Path) -> Result<&'static str>
{
    let mut magic = [0; 4];
    File::open(path)?.read_exact(&mut magic)?;
    Ok(if magic == *b"QFI\xfb" { "qcow2" } else { "raw" })
}

fn command(workspace: &Workspace, disks: &[(&Path, &str)]) -> Command
{
    let mut command = Command::new("qemu-system-riscv64");
    let harts = workspace.platform.qemu_harts.to_string();
    command.args(["-machine",
                  "virt",
                  "-cpu",
                  workspace.platform.qemu_cpu.as_str(),
                  "-m",
                  workspace.platform.qemu_ram.as_str(),
                  "-smp",
                  &harts,
                  "-bios",
                  "none",
                  "-drive"])
           .arg(format!("if=pflash,unit=0,format=raw,readonly=on,file={}",
                        workspace.build.join("dist/bootrom.bin").display()))
           .args(["-global", "virtio-mmio.force-legacy=false", "-display", "none", "-monitor",
                  "none", "-nic", "none", "-device", "ramfb", "-device",
                  "virtio-keyboard-device", "-device", "virtio-tablet-device"]);
    for (index, (path, format)) in disks.iter().enumerate()
    {
        command.arg("-drive")
               .arg(format!("if=none,id=disk{index},readonly=on,format={format},file={}",
                            path.display()))
               .arg("-device")
               .arg(format!("virtio-blk-device,drive=disk{index},bus=virtio-mmio-bus.{index}"));
    }
    command
}

pub fn interactive(workspace: &Workspace, path: &Path, format: &str) -> Result<()>
{
    println!("Booting read-only disk through flash ROM. Ctrl+C stops QEMU.");
    run(command(workspace, &[(path, format)]).args(["-serial", "stdio"]))
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

fn case(workspace: &Workspace, directory: &Path, name: &str, disks: &[(&Path, &str)],
        expected: &[&str], boots: bool)
        -> Result<Value>
{
    let serial = directory.join(format!("{name}.serial.log"));
    let stderr = directory.join(format!("{name}.qemu.log"));
    // Do not accept a readiness marker left by a previous invocation while the
    // new QEMU process is still opening its serial backend.
    File::create(&serial)?;
    let mut command = command(workspace, disks);
    command.arg("-serial")
           .arg(format!("file:{}", serial.display()))
           .stdin(Stdio::null())
           .stdout(Stdio::null())
           .stderr(File::create(&stderr)?);
    let launch = format!("{command:?}");
    let mut child = Guest(command.spawn()?);
    let deadline = Instant::now() + Duration::from_secs(15);
    let log = loop
    {
        let log = fs::read_to_string(&serial).unwrap_or_default();
        // UART output can be observed halfway through a formatted line. Wait
        // for its newline before validating markers or stopping the guest.
        if log.split_inclusive('\n').any(|line| {
                                        line.ends_with('\n')
                                        && (line.contains("DESKTOP_READY:")
                                            || line.contains("BOOT_NO_SYSTEM:"))
                                    })
        {
            break log;
        }
        if log.contains("BOOT_TRAP:")
           || log.contains("BOOT_PANIC:")
           || log.contains("BOOT_ALLOCATION_FAILED:")
           || log.contains("SYSTEM_PANIC:")
           || log.contains("SYSTEM_ALLOCATION_FAILED:")
           || (log.contains("SYSTEM_STARTUP_FAILED:")
               && !expected.iter().any(|marker| marker.contains("SYSTEM_STARTUP_FAILED:")))
           || Instant::now() >= deadline
           || child.0.try_wait()?.is_some()
        {
            return Err(error(format!("ROM check {name} failed; see {} and {}",
                                     serial.display(),
                                     stderr.display())));
        }
        thread::sleep(Duration::from_millis(20));
    };
    drop(child);
    if expected.iter().any(|marker| !log.contains(marker))
       || log.contains("DESKTOP_READY:") != boots
       || (!boots
           && log.contains("BOOT: entering System")
           && !expected.iter().any(|marker| marker.contains("SYSTEM_STARTUP_FAILED:")))
    {
        return Err(error(format!("unexpected ROM result for {name}:\n{log}")));
    }
    println!("ROM check {name}: passed");
    Ok(json!({"name": name, "status": "passed", "booted": boots, "command": launch, "expected_markers": expected, "serial": workspace.describe(&serial), "log": log}))
}

fn clone_raw(source: &Path, destination: &Path) -> Result<()>
{
    run(Command::new("qemu-img").args(["convert", "-f", "raw", "-O", "raw", "-S", "4k"])
                                .arg(source)
                                .arg(destination))
}


fn altered_system(workspace: &Workspace, directory: &Path, image: &Path, alteration: &str)
                  -> Result<()>
{
    let source = workspace.build.join("dist/System.bin");
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
                fs::read(workspace.build.join("dist/System.runtime.elf"))?;
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
    let raw = workspace.build.join("dist/system7.hfsb");
    let qcow = workspace.build.join("dist/system7.qcow2");
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
                              &[(&raw, "raw")],
                              &["System RISC Boot ROM v1 at 0x200", "CNID=16",
                                "System: native service table ready", "BOOT_SYSTEM_READY:"],
                              true)?,
                         case(workspace,
                              &directory,
                              "qcow2-hfs-b",
                              &[(&qcow, "qcow2")],
                              &["BOOT_SYSTEM_READY:"],
                              true)?,];
    let large = directory.join("large-system-fork.hfsb");
    clone_raw(&raw, &large)?;
    altered_system(workspace, &directory, &large, "large-fork")?;
    cases.push(case(workspace,
                    &directory,
                    "system-resource-fork-over-4mib",
                    &[(&large, "raw")],
                    &["BOOT_SYSTEM_READY:", "DESKTOP_READY:"],
                    true)?);
    let classic = workspace.root.join("crates/mac-toolbox/tests/data/files/dates-2026.hfs");
    cases.push(case(workspace,
                    &directory,
                    "classic-hfs-is-not-native-startup",
                    &[(&classic, "raw")],
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
                    &[(&renamed, "raw")],
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
                    &[(&unblessed, "raw")],
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
                    &[(&missing, "raw")],
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
                    &[(&missing_finder, "raw")],
                    &["startup file is missing", "BOOT_NO_SYSTEM:"],
                    false)?);
    for (alteration, expected) in [("code", "invalid RV64 segment resource header"),
                                   ("elf-resource", "ELF-backed code resources are unsupported"),
                                   ("obsolete-nabi", "obsolete ELF-backed NABI layout"),
                                   ("role", "not a compatible System"),
                                   ("address", "reservation overlaps firmware")]
    {
        let path = directory.join(format!("bad-{alteration}.hfsb"));
        clone_raw(&raw, &path)?;
        altered_system(workspace, &directory, &path, alteration)?;
        cases.push(case(workspace,
                        &directory,
                        &format!("bad-{alteration}"),
                        &[(&path, "raw")],
                        &[expected, "BOOT_NO_SYSTEM:"],
                        false)?);
    }
    let partitioned = directory.join("partitioned.hfsb");
    apm(&raw, &partitioned)?;
    cases.push(case(workspace,
                    &directory,
                    "apple-partition-map",
                    &[(&partitioned, "raw")],
                    &["at sector 2048", "BOOT_SYSTEM_READY:"],
                    true)?);
    // Explicit bus placement makes the unblessed disk the first one scanned.
    cases.push(case(workspace,
                    &directory,
                    "disk-fallback",
                    &[(&unblessed, "raw"), (&raw, "raw")],
                    &["blessed System Folder CNID is missing", "BOOT_SYSTEM_READY:"],
                    true)?);
    let malformed = directory.join("bad-code.hfsb");
    cases.push(case(workspace,
                    &directory,
                    "invalid-system-fallback",
                    &[(&malformed, "raw"), (&raw, "raw")],
                    &["invalid RV64 segment resource header", "BOOT_SYSTEM_READY:"],
                    true)?);
    if package::sha256(&fs::read(&qcow)?) != hash_before
    {
        return Err(error("boot checks changed the generated qcow2"));
    }
    let report = json!({"status": "passed", "method": "QEMU cold reset to read-only pflash; virtio-block reads HFS-B and loads System, Finder, WDEF and CDEF resources entirely inside the guest", "cases": cases, "disk_boot_tested": true, "finder_launched": true, "rom": serde_json::from_slice::<Value>(&fs::read(workspace.build.join("dist/bootrom.json"))?)?, "qcow2_unchanged": true, "qemu": output(Command::new("qemu-system-riscv64").arg("--version"))?});
    write_json(&workspace.repo.join("results/system7/boot-checks.json"), &report)?;
    println!("{} ROM boot checks passed.", cases.len());
    Ok(report)
}
