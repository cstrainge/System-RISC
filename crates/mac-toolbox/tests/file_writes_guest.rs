//! Real public ECALL/virtio persistence; Python only prepares and boots private disks.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};
#[test]
#[ignore = "requires fresh cargo xtask image and QEMU"]
fn durable_file_manager_operations_survive_three_actual_disk_boots()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("file-write-guest-{}", std::process::id()));
    let command = Command::new("python3").arg(root.join("System7/scripts/capture_file_writes.py"))
                                         .arg("--output")
                                         .arg(&output)
                                         .output()
                                         .unwrap();
    assert!(command.status.success(),
            "transport failed:\n{}\n{}",
            String::from_utf8_lossy(&command.stdout),
            String::from_utf8_lossy(&command.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"], "base images changed");
    for boot in 1..=3
    {
        let serial = fs::read_to_string(output.join(format!("boot-{boot}.log"))).unwrap();
        assert_eq!(report["boots"][boot - 1]["termination"],
                   "guest-outcome",
                   "boot {boot}: {serial}");
        assert!(!serial.contains("FILE_WRITE_FAIL"), "boot {boot}: {serial}");
        assert!(serial.contains(&format!("FILE_WRITE_STAGE {boot}")),
                "incorrect stage after remount {boot}: {serial}");
        let comment_marker = match boot
        {
            1 => "FILE_COMMENTS_COMMITTED",
            2 => "FILE_COMMENTS_PERSISTED",
            _ => "FILE_COMMENTS_UPDATED_PERSISTED"
        };
        assert!(serial.contains(comment_marker),
                "comment create/update/remount evidence missing: {serial}");
        if boot <= 2
        {
            let marker =
                if boot == 1 { "FILE_INFO_EDITS_COMMITTED" } else { "FILE_INFO_EDITS_PERSISTED" };
            assert!(serial.contains(marker), "metadata edit/remount evidence missing: {serial}");
        }
        assert!(serial.contains("TRAPS_READY: resident ecall dispatcher ABI=1"),
                "actual gate missing: {serial}");
    }
    let acceptance = serde_json::json!({"status":"passed","boots":3,"rename_file_and_folder":"passed","stationery_and_lock_persistence":"passed","exact_physical_fork_sizes":"passed","desktop_comments_persistence":"passed","acceptance":"mac-toolbox/tests/file_write_cases.rs","capture":report,"limits":["read-only volumes rejected","bounded existing catalog/overflow capacity","no power-loss atomicity without journal","Trash name conflicts return ALREADY_EXISTS rather than renaming older original Finder item","locked-item Option override not implemented"]});
    fs::write(output.join("rust-acceptance.json"), serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
    fs::write(root.join("results/system7/filesystem-write-checks.json"),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
}
