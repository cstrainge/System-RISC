//! Native file-backed mount acceptance; all guest assertions use public SDK.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};
#[test]
#[ignore = "requires fresh private image, QEMU and the backing-file provider fixture"]
fn file_backed_mounts_pin_sources_validate_positions_and_reject_reused_ids()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("native-file-sources-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/capture_file_sources.py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .output()
                                        .unwrap();
    assert!(result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let capture: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["original_before"], capture["original_after"]);
    assert_eq!(capture["termination"], "guest-outcome");
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(serial.contains("FILE_SOURCE_PASS"), "{serial}");
    for failure in ["FILE_SOURCE_FAIL", "MODULE_REJECTED", "SYSTEM_PANIC:", "SYSTEM_TRAP:",
                    "DISK_REQUEST_FAILED"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
    let report = serde_json::json!({"status":"passed", "public_sdk":"mount file; repeated mount; distinct copied files; catalog, 64 KiB and boundary reads, seek, EOF, both forks, source position independence, source mutation exclusion, busy unmount, failed-mount cleanup, foreign volume IDs, forced slot reuse and stale volume rejection; lease released after teardown",
        "provider":"actual FSYS DRVR invokes resident FORK service; source remains valid in Unmount callback and becomes stale after acknowledgement/reuse",
        "not_covered":"provider-backed source I/O, StuffIt parsing/decompression, cancellation during deferred source reads", "capture":capture});
    fs::write(output.join("rust-acceptance.json"), serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    fs::write(root.join("results/system7/file-source-checks.json"),
              serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}
