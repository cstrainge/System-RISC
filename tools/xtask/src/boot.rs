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

use crate::{TARGET, Workspace, disk, output, run, write_json};

pub fn package_rom(workspace: &Workspace) -> Result<Value>
{
    let dist = workspace.build.join("dist");
    fs::create_dir_all(&dist)?;
    let elf = workspace.build.join("target").join(TARGET).join("release/bootrom");
    let rom = dist.join("bootrom.bin");
    run(Command::new("llvm-objcopy").args(["-O", "binary"]).arg(&elf).arg(&rom))?;
    let length = rom.metadata()?.len();
    if length == 0 || length > 32 * 1024 * 1024
    {
        return Err(error("ROM payload does not fit QEMU flash bank"));
    }
    OpenOptions::new().write(true).open(&rom)?.set_len(32 * 1024 * 1024)?;
    fs::copy(&elf, dist.join("bootrom.elf"))?;
    let report = json!({"image": "build/system7/dist/bootrom.bin", "elf": "build/system7/dist/bootrom.elf", "flash_base": 0x20000000u64, "payload_size": length, "flash_bank_size": 32 * 1024 * 1024, "sha256": package::sha256(&fs::read(rom)?), "source": "System7/bootrom", "execution": "M-mode execute-in-place from read-only QEMU virt pflash"});
    write_json(&dist.join("bootrom.json"), &report)?;
    println!("Packaged boot ROM: {length} bytes in 32 MiB flash bank.");
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
    command.args(["-machine", "virt", "-cpu", "rv64", "-m", "128M", "-smp", "1", "-bios", "none",
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
           || (log.contains("FINDER_LOAD_FAILED:")
               && !expected.iter().any(|marker| marker.contains("FINDER_LOAD_FAILED:")))
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
           && !expected.iter().any(|marker| marker.contains("FINDER_LOAD_FAILED:")))
    {
        return Err(error(format!("unexpected ROM result for {name}:\n{log}")));
    }
    println!("ROM check {name}: passed");
    Ok(json!({"name": name, "status": "passed", "booted": boots, "command": launch, "expected_markers": expected, "serial": serial.strip_prefix(&workspace.repo)?.display().to_string(), "log": log}))
}

fn clone_raw(source: &Path, destination: &Path) -> Result<()>
{
    run(Command::new("qemu-img").args(["convert", "-f", "raw", "-O", "raw", "-S", "4k"])
                                .arg(source)
                                .arg(destination))
}

fn patch(path: &Path, position: u64, bytes: &[u8]) -> Result<()>
{
    let mut file = OpenOptions::new().write(true).open(path)?;
    file.seek(SeekFrom::Start(position))?;
    file.write_all(bytes)?;
    Ok(())
}

fn altered_system(workspace: &Workspace, directory: &Path, image: &Path, alteration: &str)
                  -> Result<()>
{
    let source = workspace.build.join("dist/System.bin");
    let mut file = macbinary::decode(&fs::read(source)?)?;
    let mut resources = resource::decode(&file.resources)?;
    match alteration
    {
        "elf" => resources.iter_mut().find(|r| r.kind == *b"RV64").unwrap().data[0] = 0,
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
    file.resources = resource::encode(&resources)?;
    let transport = directory.join(format!("System-{alteration}.bin"));
    fs::write(&transport, macbinary::encode(&file)?)?;
    disk::edit(workspace,
               image,
               &directory.join(format!("hfs-{alteration}")),
               &[vec!["hdel".into(), ":System Folder:System".into()],
                 vec!["hcopy".into(),
                      "-m".into(),
                      transport.into_os_string(),
                      ":System Folder:".into()]])
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
    for (index, start, sectors, kind) in
        [(1u64, 1u32, 2u32, "Apple_partition_map"), (2, 2048, (length / 512) as u32, "Apple_HFS")]
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
    let raw = workspace.build.join("dist/system7.hfs");
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
                              "raw-hfs",
                              &[(&raw, "raw")],
                              &["RISCy System Boot ROM v1 at 0x200", "CNID=16",
                                "System: native service table ready", "BOOT_SYSTEM_READY:"],
                              true)?,
                         case(workspace,
                              &directory,
                              "qcow2-hfs",
                              &[(&qcow, "qcow2")],
                              &["BOOT_SYSTEM_READY:"],
                              true)?,];
    let renamed = directory.join("renamed.hfs");
    clone_raw(&raw, &renamed)?;
    disk::edit(workspace,
               &renamed,
               &directory.join("hfs-rename"),
               &[vec!["hrename".into(), ":System Folder".into(), ":Renamed Startup".into()]])?;
    cases.push(case(workspace,
                    &directory,
                    "renamed-blessed-folder",
                    &[(&renamed, "raw")],
                    &["blessed folder 'Renamed Startup' CNID=16", "BOOT_SYSTEM_READY:"],
                    true)?);
    let unblessed = directory.join("unblessed.hfs");
    clone_raw(&raw, &unblessed)?;
    patch(&unblessed, 1024 + 92, &[0; 4])?;
    cases.push(case(workspace,
                    &directory,
                    "unblessed",
                    &[(&unblessed, "raw")],
                    &["volume has no blessed System Folder", "BOOT_NO_SYSTEM:"],
                    false)?);
    let missing = directory.join("missing.hfs");
    clone_raw(&raw, &missing)?;
    disk::edit(workspace,
               &missing,
               &directory.join("hfs-missing"),
               &[vec!["hdel".into(), ":System Folder:System".into()]])?;
    cases.push(case(workspace,
                    &directory,
                    "missing-system",
                    &[(&missing, "raw")],
                    &["startup file is missing", "BOOT_NO_SYSTEM:"],
                    false)?);
    let missing_finder = directory.join("missing-finder.hfs");
    clone_raw(&raw, &missing_finder)?;
    disk::edit(workspace,
               &missing_finder,
               &directory.join("hfs-missing-finder"),
               &[vec!["hdel".into(), ":System Folder:Finder".into()]])?;
    cases.push(case(workspace,
                    &directory,
                    "missing-finder",
                    &[(&missing_finder, "raw")],
                    &["FINDER_LOAD_FAILED:", "BOOT_NO_SYSTEM:"],
                    false)?);
    for (alteration, expected) in [("elf", "expected ELF64"),
                                   ("role", "not a compatible System"),
                                   ("address", "reservation overlaps firmware")]
    {
        let path = directory.join(format!("bad-{alteration}.hfs"));
        clone_raw(&raw, &path)?;
        altered_system(workspace, &directory, &path, alteration)?;
        cases.push(case(workspace,
                        &directory,
                        &format!("bad-{alteration}"),
                        &[(&path, "raw")],
                        &[expected, "BOOT_NO_SYSTEM:"],
                        false)?);
    }
    let partitioned = directory.join("partitioned.hfs");
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
                    &["volume has no blessed System Folder", "BOOT_SYSTEM_READY:"],
                    true)?);
    let malformed = directory.join("bad-elf.hfs");
    cases.push(case(workspace,
                    &directory,
                    "invalid-system-fallback",
                    &[(&malformed, "raw"), (&raw, "raw")],
                    &["expected ELF64", "BOOT_SYSTEM_READY:"],
                    true)?);
    if package::sha256(&fs::read(&qcow)?) != hash_before
    {
        return Err(error("boot checks changed the generated qcow2"));
    }
    let report = json!({"status": "passed", "method": "QEMU cold reset to read-only pflash; virtio-block reads HFS and loads System, Finder, WDEF and CDEF resources entirely inside the guest", "cases": cases, "disk_boot_tested": true, "finder_launched": true, "rom": serde_json::from_slice::<Value>(&fs::read(workspace.build.join("dist/bootrom.json"))?)?, "qcow2_unchanged": true, "qemu": output(Command::new("qemu-system-riscv64").arg("--version"))?});
    write_json(&workspace.repo.join("results/system7/boot-checks.json"), &report)?;
    println!("{} ROM boot checks passed.", cases.len());
    Ok(report)
}
