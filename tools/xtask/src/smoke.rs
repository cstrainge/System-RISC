use std::{fs::{self, File},
          process::{Command, Stdio},
          thread,
          time::{Duration, Instant}};

use mac_build::{Result, error, package};
use mac_formats::{macbinary,
                  native::{Descriptor, image::LoadPlan},
                  resource};
use serde_json::{Value, json};

use crate::{TARGET, Workspace, output, run};

pub fn check(workspace: &Workspace, reports: &[Value]) -> Result<Value>
{
    let out = workspace.build.join("abi-smoke");
    fs::create_dir_all(&out)?;
    for name in ["System", "Finder"]
    {
        let file = macbinary::decode(&fs::read(workspace.build
                                                        .join("dist")
                                                        .join(format!("{name}.bin")))?)?;
        let resources = resource::decode(&file.resources)?;
        let code = resources.iter()
                            .find(|r| r.kind == *b"RV64" && r.id == 0)
                            .ok_or_else(|| error("native executable missing"))?;
        let descriptor = Descriptor::decode(&resources.iter()
                                                      .find(|r| r.kind == *b"NABI" && r.id == 0)
                                                      .ok_or_else(|| error("native ABI missing"))?
                                                      .data)?;
        let relocations =
            resources.iter().find(|r| r.kind == *b"RLOC" && r.id == 0).map(|r| r.data.as_slice());
        let plan = LoadPlan::new(&code.data, &descriptor, relocations, descriptor.load_base)?;
        let mut memory = vec![0; plan.length()];
        plan.write(&mut memory)?;
        fs::write(out.join(format!("{name}.raw")), memory)?;
    }
    run(workspace.cargo()
                 .args(["build", "--locked", "--release", "--target", TARGET, "--target-dir"])
                 .arg(workspace.build.join("target"))
                 .args(["--package", "abi-smoke"])
                 .env("SYSTEM7_SYSTEM_ENTRY", reports[0]["entry"].as_u64().unwrap().to_string())
                 .env("SYSTEM7_FINDER_ENTRY", reports[1]["entry"].as_u64().unwrap().to_string()))?;
    let kernel = workspace.build.join("target").join(TARGET).join("release/abi-smoke");
    let serial = out.join("serial.log");
    let stderr = out.join("qemu.log");
    File::create(&serial)?;
    let mut command = Command::new("qemu-system-riscv64");
    command.args(["-machine",
                  "virt",
                  "-cpu",
                  mac_build::platform::current().qemu_cpu.as_str(),
                  "-m",
                  "128M",
                  "-smp",
                  "1",
                  "-bios",
                  "none",
                  "-kernel"])
           .arg(&kernel)
           .args(["-display", "none", "-monitor", "none", "-nic", "none", "-serial"])
           .arg(format!("file:{}", serial.display()))
           .arg("-device")
           .arg(format!("loader,file={},addr={},force-raw=on",
                        out.join("System.raw").display(),
                        reports[0]["load_base"].as_u64().unwrap()))
           .arg("-device")
           .arg(format!("loader,file={},addr={},force-raw=on",
                        out.join("Finder.raw").display(),
                        reports[1]["load_base"].as_u64().unwrap()))
           .stdin(Stdio::null())
           .stdout(Stdio::null())
           .stderr(Stdio::from(File::create(&stderr)?));
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop
    {
        if let Some(status) = child.try_wait()?
        {
            break status;
        }
        if Instant::now() >= deadline
        {
            child.kill()?;
            child.wait()?;
            return Err(error(format!("RV64 ABI smoke check timed out; see {}", serial.display())));
        }
        thread::sleep(Duration::from_millis(50));
    };
    let log = fs::read_to_string(&serial)?;
    for marker in ["System: native service table ready",
                   "Finder: native application entry reached", "NATIVE_TRAP_ABI_PASS",
                   "CLASSIC_TRAP_ABI_PASS", "QUICKDRAW_TRAP_ABI_PASS", "FILE_TRAP_ABI_PASS",
                   "TOOLBOX_USERSPACE_PASS geometry=20 ovals=267 handles=48",
                   "SCRAP_USERSPACE_PASS original=18 native=17", "ABI_SMOKE_PASS"]
    {
        if !status.success() || !log.contains(marker)
        {
            return Err(error(format!("RV64 execution failed ({status}); see {} and {}",
                                     serial.display(),
                                     stderr.display())));
        }
    }
    print!("{log}");
    Ok(json!({"status": "passed", "method": "Native segment resources validated and materialized by LoadPlan; QEMU raw-memory loader plus ABI test harness", "qemu": output(Command::new("qemu-system-riscv64").arg("--version"))?, "serial": workspace.describe(&serial), "harness_sha256": package::sha256(&fs::read(kernel)?), "checks": ["System entry publishes service table", "Finder calls System log service", "Finder rejects null and incompatible service tables", "System rejects incompatible BootInfo"], "disk_boot_tested": false, "userspace_conformance": {"source": "System7/crates/mac-toolbox/tests/conformance", "geometry_contracts": 20, "original_oval_records": 267, "original_handle_observations": 48, "original_pointer_observations": 3, "original_scrap_contracts": 18, "native_scrap_contracts": 17,  "transport": "public SDK through actual RV64 ECALL"}}))
}
