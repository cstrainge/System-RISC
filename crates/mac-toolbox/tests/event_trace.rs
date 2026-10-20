//! Acceptance for observations of real application SDK event consumption.
//! Requires the opt-in resident trace image; never targets the visible desktop.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};

use serde_json::Value;

#[test]
#[ignore = "build with SYSTEM7_EVENT_TRACE=1 cargo xtask image; private QEMU trace scenarios"]
fn physical_events_are_delivered_once_and_noop_activation_has_no_update_storm()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let existing = std::env::var_os("SYSTEM7_EVENT_TRACE_CAPTURE").map(PathBuf::from);
    let output =
        existing.as_ref()
                .map(|path| {
                    if path.is_absolute() { path.clone() } else { root.join("System7").join(path) }
                })
                .unwrap_or_else(|| {
                    root.join(format!("results/system7/event-trace-{}", std::process::id()))
                });
    if existing.is_none()
    {
        let capture = Command::new("python3").arg(root.join("System7/scripts/\
                                                             capture_system7_event_trace.py"))
                                             .arg("--output")
                                             .arg(&output)
                                             .output()
                                             .unwrap();
        assert!(capture.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&capture.stdout),
                String::from_utf8_lossy(&capture.stderr));
    }
    assert!(output.join("capture.json").exists(),
            "missing requested capture: {}",
            output.display());
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    let stages = report["stages"].as_array().unwrap();
    assert!(stages.len() >= 31);
    let stage = |name: &str| stages.iter().find(|stage| stage["name"] == name).unwrap();
    for stage in stages
    {
        assert_eq!(stage["quiescent"], true, "{} never reached a quiet gap", stage["name"]);
        assert_eq!(stage["overwritten"], 0, "bounded trace lost records: {}", stage["name"]);
        let log = stage["serial"].as_str().unwrap();
        assert!(!log.contains("WM_STATE "), "{}: input path wrote UART state logs", stage["name"]);
        for marker in ["SYSTEM_PANIC", "SYSTEM_TRAP:", "SYSTEM_ALLOCATION_FAILED", "_ERROR"]
        {
            assert!(!log.contains(marker), "{}: {log}", stage["name"]);
        }
        for dropped in log.lines().filter_map(|line| line.split("dropped=").nth(1))
        {
            assert_eq!(dropped.parse::<u64>().unwrap(), 0);
        }
    }
    for name in ["typing-burst", "typing-with-backgrounds"]
    {
        let records = stage(name)["records"].as_array().unwrap();
        for (kind, input_down) in [(3, 1), (4, 0)]
        {
            let input: Vec<_> = records.iter()
                                       .filter(|r| r[2] == 1 && r[4] == 3 && r[6] == input_down)
                                       .map(|r| r[5].as_u64().unwrap())
                                       .collect();
            let delivered: Vec<_> = records.iter()
                                           .filter(|r| r[2] == 2 && r[4] == kind)
                                           .map(|r| r[5].as_u64().unwrap())
                                           .collect();
            assert_eq!(input.len(), 20, "{name}: physical transitions");
            assert_eq!(delivered, input,
                       "{name}: every key transition delivered exactly once, in order");
        }
    }
    for name in ["same-app-finder", "same-app-quickdraw", "show-all-again"]
    {
        for record in stage(name)["records"].as_array().unwrap()
        {
            assert!(!(record[2] == 2 && record[4] == 8), "{name}: artificial activation {record}");
            assert_ne!(record[2], 4, "{name}: artificial Update {record}");
        }
    }
    let modifier = stage("modifier-only")["records"].as_array().unwrap();
    assert!(!modifier.iter().any(|r| r[2] == 2 && (r[4] == 3 || r[4] == 4)),
            "modifier state changes are not text key deliveries");
    let idle = stage("idle-finder");
    assert!(!idle["records"].as_array()
                            .unwrap()
                            .iter()
                            .any(|r| matches!(r[2].as_u64(), Some(2 | 4 | 7))),
            "idle Finder generated events or full-screen compositions");
    let motion = stage("pointer-motion")["records"].as_array().unwrap();
    assert!(!motion.iter().any(|r| r[2] == 7),
            "cursor motion must not replay window content or the desktop");
    let copies: Vec<_> = motion.iter().filter(|r| r[2] == 14).collect();
    assert!(!copies.is_empty(), "cursor movement was never presented");
    for copy in copies
    {
        assert!(copy[5].as_u64().unwrap() <= 16 * 16,
                "cursor-only presentation copied outside old/new cursor bounds: {copy}");
        let start = copy[6].as_u64().unwrap();
        let end = copy[7].as_u64().unwrap();
        assert!((end >> 32) - (start >> 32) <= 16
                && (end & 0xffff_ffff) - (start & 0xffff_ffff) <= 16,
                "cursor-only copy exceeded cursor dimensions: {copy}");
    }
    let before = &stage("pointer-motion")["state_before"];
    let after = &stage("pointer-motion")["state_after"];
    for field in ["scene_rev", "compositions", "menu_paints"]
    {
        assert_eq!(before[field], after[field], "cursor motion changed {field}");
    }
    assert_ne!(before["input_seq"], after["input_seq"], "physical input was not acknowledged");
    let drag = stage("drag-editor");
    assert_ne!(drag["state_before"]["compositions"], drag["state_after"]["compositions"]);
    assert_eq!(drag["state_before"]["menu_paints"], drag["state_after"]["menu_paints"],
               "unchanged menu bar was rerasterized while tracking a window");
    fs::write(output.join("rust-acceptance.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "status":"passed", "scenarios":stages.len(), "assertion_owner":"mac-toolbox/tests/event_trace.rs",
        "scope":"Real SDK application consumption; no repeated text delivery or no-op activation damage",
        "timing":"observations only; no host-dependent wall-clock threshold"
    })).unwrap()).unwrap();
}
