//! Production classic SIT DRVR acceptance through the public Toolbox facade.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};
#[test]
#[ignore = "requires the private image and QEMU"]
fn classic_archives_mount_read_forks_copy_out_and_reject_corruption_and_stale_ids()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("native-stuffit-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/capture_stuffit.py"))
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
    assert!(serial.contains("STUFFIT_PASS"), "{serial}");
    assert!(serial.contains("RETAINED_UPDATE_USERSPACE_PASS"), "{serial}");
    assert!(serial.contains("STUFFIT_CLASSIC2_USERSPACE_PASS"), "{serial}");
    assert_eq!(serial.matches("STUFFIT_V5_USERSPACE_PASS bad_crc=false").count(), 4, "{serial}");
    assert_eq!(serial.matches("STUFFIT_V5_USERSPACE_PASS bad_crc=true").count(), 1, "{serial}");
    for failure in
        ["STUFFIT_FAIL", "MODULE_REJECTED", "SYSTEM_PANIC:", "SYSTEM_TRAP:", "DISK_REQUEST_FAILED"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
    let report = serde_json::json!({"status":"passed","public_sdk":"production FSYS DRVR and resident FORK service; block-backed and writable/read-only host-backed sources; driver-declared file-code and untyped extension dispatch; 16/32 pixel icon rendering with bounded destinations and unclaimed file fallback; classic nested archive, case-insensitive Unicode lookup, actual archive root name, read-only capabilities, repeated mount, distinct copied sources, large reads, backward seek, EOF, independent public source position, busy unmount, source replacement exclusion, data copy-out, both nonempty forks, corruption reported on final full read, invalid archive cleanup, forced mounted-volume reuse and stale rejection, acknowledged teardown followed by immediate host source replacement",
        "not_covered":"full two-fork copy transaction, original Macintosh producer/runtime comparisons, guest cancellation during deferred source reads; real Finder double-click has a separate test in this gate",
        "capture":capture});
    fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    fs::write(root.join("results/system7/stuffit-checks.json"),
              serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}

#[test]
#[ignore = "requires the private image and QEMU"]
fn removing_driver_removes_archive_open_action_and_default_icons()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("native-stuffit-absent-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/capture_stuffit.py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .arg("--without-driver")
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
    assert!(serial.contains("STUFFIT_PASS"), "{serial}");
    assert!(!serial.contains("STUFFIT_FAIL") && !serial.contains("name=StuffIt Filesystem"),
            "{serial}");
}

#[test]
#[ignore = "requires the private image, QEMU and the real Finder"]
fn finder_double_click_mounts_and_opens_archive_from_host_volume()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("native-stuffit-finder-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/capture_stuffit.py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .arg("--finder")
                                        .output()
                                        .unwrap();
    assert!(result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let capture: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["original_before"], capture["original_after"]);
    assert_eq!(capture["termination"], "finder-opened-archive");
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(serial.lines()
                  .any(|l| {
                      l.starts_with("FINDER_FOLDER_OPEN ") && l.ends_with("name=Examples.sit")
                  }),
            "{serial}");
    for failure in
        ["FINDER_ERROR", "FINDER_ACTION_FAILED", "MODULE_REJECTED", "SYSTEM_PANIC:", "SYSTEM_TRAP:"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
    assert!(fs::metadata(output.join("archive-icon.ppm")).unwrap().len() > 640 * 480);
    assert!(fs::metadata(output.join("archive-volume.ppm")).unwrap().len() > 640 * 480);
    assert_no_update_storm(&output);
}

#[test]
#[ignore = "requires the private image, QEMU and the real Finder"]
fn finder_explains_raw_archive_failure_and_can_immediately_open_the_next_file()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("native-stuffit-error-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/capture_stuffit.py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .arg("--finder-error")
                                        .output()
                                        .unwrap();
    assert!(result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let capture: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["original_before"], capture["original_after"]);
    assert_eq!(capture["termination"], "finder-opened-archive");
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    let alerts: Vec<_> = serial.lines().filter(|l| l.starts_with("FINDER_ALERT_TEXT ")).collect();
    assert_eq!(alerts.len(), 1, "{serial}");
    assert!(alerts[0].contains("StuffIt archive version 99 is not supported by the installed \
                                driver."),
            "{serial}");
    assert!(alerts[0].contains("Archive(Version(99))"), "{serial}");
    assert!(serial.contains("FINDER_ALERT_DISMISSED")
            && serial.lines()
                     .any(|l| l.starts_with("FINDER_FOLDER_OPEN ") && l.ends_with("name=B.sit")),
            "{serial}");
    assert_eq!(serial.matches("FINDER_ACTION_FAILED").count(), 1, "{serial}");
    for failure in ["MODULE_REJECTED", "SYSTEM_PANIC:", "SYSTEM_TRAP:"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
    assert_no_update_storm(&output);
}

fn assert_no_update_storm(output: &std::path::Path)
{
    // Tracing is opt-in; assert bounded work whenever the image includes it.
    if let Ok(bytes) = fs::read(output.join("complete-trace.json"))
    {
        let trace: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(trace["counters"]["1"]["update"].as_u64().unwrap_or(0) < 100,
                "folder refresh repeatedly returned unacknowledged Update events: {trace}");
        assert!(trace["services"]["1"]["menu"][0].as_u64().unwrap_or(0) < 1000,
                "folder refresh flooded per-event menu work: {trace}");
    }
}


fn finder_archive(source: &std::path::Path, label: &str)
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    assert!(source.is_file(),
            "Provide this reference archive before running the ignored gate: {}",
            source.display());
    let output =
        root.join("results/system7").join(format!("native-stuffit-{label}-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/capture_stuffit.py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .arg("--finder")
                                        .arg("--archive")
                                        .arg(source)
                                        .output()
                                        .unwrap();
    assert!(result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let capture: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["original_before"], capture["original_after"]);
    assert_eq!(capture["termination"], "finder-opened-archive");
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(serial.lines()
                  .any(|l| {
                      l.starts_with("FINDER_FOLDER_OPEN ") && l.ends_with("name=Examples.sit")
                  }),
            "{serial}");
    for failure in
        ["FINDER_ERROR", "FINDER_ACTION_FAILED", "MODULE_REJECTED", "SYSTEM_PANIC:", "SYSTEM_TRAP:"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
    assert_no_update_storm(&output);
}
#[test]
#[ignore = "requires current private image, QEMU and real Finder"]
fn stuffit5_raw_host_archive_opens_through_real_finder()
{
    finder_archive(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../mac-sit/tests/fixtures/\
                                                                    arsenic-v5.sit"),
                   "v5-finder");
}
#[test]
#[ignore = "requires current private image, QEMU and the user-supplied SuperPaint reference archive"]
fn user_supplied_stuffit5_reference_opens_through_real_finder()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let source =
        std::env::var_os("SYSTEM7_STUFFIT_REFERENCE").map(PathBuf::from)
                                                     .unwrap_or(root.join("System7/transfer/\
                                                                           aldussuperpaint3.5.\
                                                                           sit"));
    finder_archive(&source, "reference-finder");
}

#[test]
#[ignore = "requires current private image, QEMU and user-supplied SuperPaint reference archive"]
fn user_supplied_stuffit5_reference_verifies_all_nine_forks_through_public_sdk()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let source =
        std::env::var_os("SYSTEM7_STUFFIT_REFERENCE").map(PathBuf::from)
                                                     .unwrap_or(root.join("System7/transfer/\
                                                                           aldussuperpaint3.5.\
                                                                           sit"));
    assert!(source.is_file(),
            "Provide SuperPaint reference archive at {} or SYSTEM7_STUFFIT_REFERENCE",
            source.display());
    let output = root.join("results/system7")
                     .join(format!("native-stuffit-reference-sdk-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/capture_stuffit.py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .arg("--reference")
                                        .arg(source)
                                        .output()
                                        .unwrap();
    assert!(result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let capture: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["reference_included"], true);
    assert_eq!(capture["original_before"], capture["original_after"]);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(serial.contains("STUFFIT_PASS"), "{serial}");
    assert_eq!(serial.matches("STUFFIT_REFERENCE_USERSPACE_PASS forks=9").count(), 2, "{serial}");
    for failure in ["STUFFIT_FAIL", "MODULE_REJECTED", "SYSTEM_PANIC:", "SYSTEM_TRAP:"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
}
