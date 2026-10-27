//! Real hardware permission refusals, separate from recoverable SDK errors.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::Path,
          process::{Command, Stdio},
          thread,
          time::{Duration, Instant}};

#[test]
#[ignore = "requires current optimized native ABI artifacts; deliberately terminates private guests"]
fn system_stack_guards_and_code_permissions_fault_loudly_on_actual_sv39()
{
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let build = mac_build::paths::build_dir(root.parent().unwrap());
    let platform = mac_build::platform::current();
    let abi = build.join("abi-smoke");
    let descriptor = |name: &str| {
        let file=mac_formats::macbinary::decode(&fs::read(build.join("dist").join(format!("{name}.bin"))).unwrap()).unwrap();
        let resources = mac_formats::resource::decode(&file.resources).unwrap();
        mac_formats::native::Descriptor::decode(&resources.iter()
                                                          .find(|r| r.kind == *b"NABI" && r.id == 0)
                                                          .unwrap()
                                                          .data).unwrap()
    };
    let system = descriptor("System");
    let finder = descriptor("Finder");
    for (case, cause) in [("lower_guard", 13), ("upper_guard", 13), ("code_write", 15)]
    {
        let state = abi.join(format!("permission-{case}"));
        fs::create_dir_all(&state).unwrap();
        let mut compile = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
        compile.current_dir(&root)
               .args(["build", "--locked", "--release", "--target", &platform.rust_target,
                      "--target-dir"])
               .arg(build.join("target"))
               .args(["--package", "abi-smoke"])
               .env("SYSTEM7_SYSTEM_REGIONS", abi.join("system-regions.rs"))
               .env("SYSTEM7_SYSTEM_ENTRY", system.entry.to_string())
               .env("SYSTEM7_FINDER_ENTRY", finder.entry.to_string())
               .env("SYSTEM7_PAGE_PROBE", case);
        mac_build::platform::configure(&mut compile);
        let output = compile.output().unwrap();
        fs::write(state.join("build.log"), [output.stdout, output.stderr].concat()).unwrap();
        assert!(output.status.success(), "{case}: see {}", state.join("build.log").display());
        let serial = state.join("serial.log");
        let diagnostics = fs::File::create(state.join("qemu.log")).unwrap();
        let mut command = Command::new("qemu-system-riscv64");
        command.args(["-machine", "virt", "-cpu", &platform.qemu_cpu, "-m", &platform.qemu_ram,
                      "-smp", "1", "-bios", "none", "-kernel"])
               .arg(build.join("target").join(&platform.rust_target).join("release/abi-smoke"))
               .args(["-display", "none", "-monitor", "none", "-nic", "none", "-serial"])
               .arg(format!("file:{}", serial.display()))
               .arg("-device")
               .arg(format!("loader,file={},addr={},force-raw=on",
                            abi.join("System.raw").display(),
                            platform.memory().unwrap().system_image_storage().start))
               .arg("-device")
               .arg(format!("loader,file={},addr={},force-raw=on",
                            abi.join("Finder.raw").display(),
                            finder.load_base))
               .stdin(Stdio::null())
               .stdout(diagnostics.try_clone().unwrap())
               .stderr(diagnostics);
        fs::write(state.join("command.txt"), format!("{command:?}\n")).unwrap();
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop
        {
            if let Some(status) = child.try_wait().unwrap()
            {
                break status;
            }
            if Instant::now() >= deadline
            {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{case}: native page fault did not terminate loudly:\n{}",
                       fs::read_to_string(&serial).unwrap_or_default());
            }
            thread::sleep(Duration::from_millis(20));
        };
        let text = fs::read_to_string(&serial).unwrap();
        assert!(!status.success(), "{case}: protection failure returned success:\n{text}");
        for marker in ["MMU_READY mode=sv39",
                       "System: native service table ready",
                       &format!("NATIVE_PAGE_PROBE case={case}"),
                       &format!("SYSTEM_TRAP: cause={cause} "),
                       "SYSTEM_SHUTDOWN: fatal privileged fault; recovery is not proven"]
        {
            assert!(text.contains(marker), "{case}: missing {marker}:\n{text}");
        }
        assert!(!text.contains("NATIVE_PAGE_PROBE_UNEXPECTED_SUCCESS"), "{text}");
        assert!(!text.contains("ABI_MACHINE_ESCAPE"),
                "{case}: ordinary resident fault escaped the saved-state handler:\n{text}");
    }
}
