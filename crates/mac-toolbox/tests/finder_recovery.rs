//! Actual Finder failure/replacement, not a mock reset or a guest reboot.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};

use serde_json::Value;
#[path = "support/display.rs"]
mod display;

#[test]
#[ignore = "requires the current optimized image and private graphical QEMU"]
fn finder_restarts_with_fresh_state_and_retains_report_on_allocation_refusal()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let mut results = Vec::new();
    for pressure in [false, true]
    {
        let output = root.join(format!("results/system7/finder-recovery-{}-{}",
                                       if pressure { "pressure" } else { "repeat" },
                                       std::process::id()));
        let mut command = Command::new("python3");
        command.arg(root.join("System7/scripts/capture_system7_finder_recovery.py"))
               .arg("--output")
               .arg(&output)
               .current_dir(&root);
        if pressure
        {
            command.arg("--pressure");
        }
        let transport = command.output().unwrap();
        assert!(transport.status.success(),
                "Finder recovery transport failed: {}\n{}\n{}",
                output.display(),
                String::from_utf8_lossy(&transport.stdout),
                String::from_utf8_lossy(&transport.stderr));
        let capture: Value =
            serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
        assert_eq!(capture["original_before"], capture["original_after"]);
        assert_eq!(capture["owned_vm_exited"], true);
        let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
        for snapshot in capture["snapshots"].as_array().unwrap().iter().filter(|entry|
            entry["name"].as_str().is_some_and(|name| name.ends_with("replacement")))
        { assert_eq!(snapshot["state"]["mouse"], "16,550", "fresh saved context restored default pointer state"); }

        for failure in ["SYSTEM_PANIC:", "SYSTEM_TRAP:", "SYSTEM_ALLOCATION_FAILED:",
                        "FINDER_RECOVERY_FAIL", "APPLICATION_MENU_FAILED",
                        "APPLICATION_PANIC_RECURSIVE"]
        {
            assert!(!serial.contains(failure), "{failure} in {}\n{serial}", output.display());
        }
        assert_eq!(serial.matches("System RISC Boot ROM v1 ").count(),
                   1,
                   "recovery rebooted the kernel");
        let count = if pressure { 1 } else { 2 };
        assert_eq!(serial.matches("FINDER_RECOVERED ").count(), count);
        assert_eq!(serial.matches("FINDER_RECOVERY_PASS ").count(), count);
        assert_eq!(serial.matches("FINDER_RECOVERY_INPUT_ACK\n").count(), count);
        for line in serial.lines().filter(|line| line.starts_with("FINDER_RECOVERED "))
        {
            let fields: Vec<_> = line.split_whitespace().collect();
            let old = fields[1].strip_prefix("old=").unwrap();
            let new = fields[2].strip_prefix("new=").unwrap();
            assert_ne!(old, new);
            let retired = format!("PROCESS_EXIT app={old}\n");
            assert!(serial.find(&retired).unwrap() < serial.find(line).unwrap(),
                    "old owner did not cross its real stack and resource death barrier");
        }
        assert!(serial.contains("retained report café λ"), "original UTF-8 diagnostics lost");
        if pressure
        {
            let image = |name: &str| {
                let snapshot = capture["snapshots"].as_array()
                                                   .unwrap()
                                                   .iter()
                                                   .find(|entry| entry["name"] == name)
                                                   .unwrap();
                let data = fs::read(output.join(snapshot["frame"].as_str().unwrap())).unwrap();
                let header = display::header();
                assert!(data.starts_with(&header));
                data[header.len()..].to_vec()
            };
            let before = image("original-report");
            let refused = image("admission-refused");
            for y in 140..396
            {
                let from = (y * display::width() + 44) * 3;
                let to = (y * display::width() + 596) * 3;
                assert_eq!(&before[from..to],
                           &refused[from..to],
                           "restart warning repainted or changed diagnostic body");
            }
            assert!(serial.contains("FINDER_RESTART_FAILED "));
            assert!(serial.contains("status=-5; report retained, retry available"),
                    "allocation refusal lacks retry/report context: {serial}");
            assert!(serial.contains("CRASH_REPORT_COPIED "),
                    "refused restart broke report commands");
            let refused = serial.find("FINDER_RESTART_FAILED ").unwrap();
            let handoff = serial.find("FINDER_HANDOFF ").unwrap();
            let freed = serial.find("FINDER_RECOVERY_WORKER_EXIT\n").unwrap();
            assert!(refused < freed && freed < handoff, "failed preflight destroyed the old shell");
        }
        else
        {
            assert!(serial.contains("FINDER_RECOVERY_WORKER_INPUT_ACK\n"));
        }
        results.push(serde_json::json!({"pressure":pressure,"output":output,"capture":capture}));
    }
    fs::create_dir_all(root.join("results/system7")).unwrap();
    fs::write(root.join("results/system7/finder-recovery-checks.json"),
        serde_json::to_vec_pretty(&serde_json::json!({"status":"passed","guests":results})).unwrap()).unwrap();
}
