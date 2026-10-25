//! Real Finder Get Info through resident Files and external filesystem drivers.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/display.rs"]
mod display;
use std::{fs, path::PathBuf, process::Command};

#[test]
#[ignore = "requires a current private image, Linux QEMU and host companion"]
fn host_mount_roots_and_native_and_plain_items_open_get_info()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("remote-get-info-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/capture_remote_get_info.\
                                                        py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .output()
                                        .unwrap();
    assert!(result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    let observations = report["observations"].as_array().unwrap();
    assert_eq!(observations.len(), 10);
    let mut seen = std::collections::BTreeSet::new();
    for observation in observations
    {
        let opened = observation["opened"].as_str().unwrap();
        let name = opened.split_once(" name=").unwrap().1;
        let mount = observation["mount"].as_str().unwrap();
        if let Some(requested) = observation["requested"].as_str()
        {
            assert_eq!(name, requested, "{observation}");
        }
        assert!(seen.insert((mount, name)), "duplicate Info target: {observation}");
        assert_eq!(opened.contains(" folder=1 "), !matches!(name, "B File" | "D File"));
        let frame = fs::read(output.join(observation["frame"].as_str().unwrap())).unwrap();
        assert!(frame.starts_with(&display::header()));
    }
    for mount in ["Writable", "Readonly"]
    {
        for name in [mount, "A Folder", "B File", "C Folder", "D File"]
        {
            assert!(seen.contains(&(mount, name)), "missing {mount}/{name}");
        }
    }
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert_eq!(serial.matches("FINDER_INFO_CLOSED ").count(), 10, "{serial}");
    for failure in ["FINDER_INFO_CREATE_FAILED", "FINDER_ACTION_FAILED", "FINDER_INFO_ERROR",
                    "SYSTEM_TRAP:", "SYSTEM_PANIC:"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
}
