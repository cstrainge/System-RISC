//! Public SDK acceptance for dynamic live process/thread capacity.
#![cfg(not(target_arch = "riscv64"))]
use std::{collections::BTreeSet, fs, path::PathBuf, process::Command};
#[test]
#[ignore = "requires an optimized private image and QEMU"]
fn more_than_eight_apps_receive_input_and_reuse_reclaimed_stacks()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("thread-capacity-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/\
                                                        capture_system7_thread_capacity.py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .current_dir(&root)
                                        .output()
                                        .unwrap();
    assert!(result.status.success(),
            "capacity transport failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    assert_eq!(report["owned_vm_exited"], true);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(!serial.contains("THREAD_CAPACITY_FAIL"), "{serial}");
    assert!(!serial.contains("CRASH_REPORT_READY"), "{serial}");
    for phase in 0..2
    {
        assert!(serial.contains(&format!("THREAD_CAPACITY_LIVE phase={phase} apps=10\n")),
                "{serial}");
    }
    let rows: Vec<_> =
        serial.lines().filter(|line| line.starts_with("THREAD_CAPACITY_WORKER ")).collect();
    assert_eq!(rows.len(), 18, "{serial}");
    for field in ["app", "thread", "image", "window"]
    {
        let values: BTreeSet<_> =
            rows.iter()
                .map(|line| {
                    line.split_whitespace()
                        .find_map(|part| part.strip_prefix(&format!("{field}=")))
                        .unwrap()
                })
                .collect();
        assert_eq!(values.len(), 18, "reused or foreign {field} identity: {serial}");
    }
    // Within each concurrently running cohort, mutable globals reside at
    // different actual addresses. A second incarnation may reuse real backing.
    for cohort in rows.chunks_exact(9)
    {
        let addresses: BTreeSet<_> =
            cohort.iter()
                  .map(|line| {
                      line.split_whitespace().find_map(|part| part.strip_prefix("global=")).unwrap()
                  })
                  .collect();
        assert_eq!(addresses.len(), 9, "writable app images alias: {serial}");
    }
    assert_eq!(serial.lines().filter(|line| line.starts_with("THREAD_CAPACITY_ACK ")).count(),
               18,
               "{serial}");
    assert_eq!(report["input"].as_array().unwrap().len(), 18);
    assert!(serial.contains("THREAD_CAPACITY_PASS apps=10 incarnations=18 native-ids=passed \
                             private-globals=passed input=passed reuse=passed"),
            "{serial}");
}
