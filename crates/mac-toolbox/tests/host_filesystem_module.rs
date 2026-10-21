//! Actual native driver/module SDK acceptance. Mounted public File Manager APIs
//! require their separate gate; this checks their native transport prerequisite.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};

#[test]
#[ignore = "requires fresh private cargo xtask image, Linux QEMU and host companion"]
fn native_filesystem_callback_reaches_the_real_host_through_relocated_modules()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("native-host-filesystem-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/capture_host_filesystem.\
                                                        py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .output()
                                        .unwrap();
    assert!(result.status.success(),
            "capture failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let capture: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["original_before"], capture["original_after"]);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    for name in ["Host Transport", "Host Filesystem", "Host Filesystem Probe"]
    {
        assert!(serial.contains(&format!("MODULE_PUBLISHED name={name} id=")),
                "{name} failed:\n{serial}");
    }
    let transport = serial.find("MODULE_PUBLISHED name=Host Transport id=").unwrap();
    let filesystem = serial.find("MODULE_ENTER name=Host Filesystem id=").unwrap();
    let probe = serial.find("MODULE_ENTER name=Host Filesystem Probe id=").unwrap();
    assert!(transport < filesystem && filesystem < probe, "dependency order: {serial}");
    for failure in ["MODULE_REJECTED", "SYSTEM_TRAP:", "SYSTEM_PANIC:"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
    assert!(serial.contains("DESKTOP_READY"), "{serial}");
    let report = serde_json::json!({"status":"passed", "native_module_callbacks":"FSYS Exports through HDIR/virtio and real Linux host companion",
        "mounted_public_file_api":"not covered", "capture":capture});
    fs::write(output.join("rust-acceptance.json"), serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    fs::write(root.join("results/system7/host-filesystem-module-checks.json"),
              serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}
