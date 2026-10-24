//! Real close-box input, public Window SDK observations and retained pixel checks.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::{Path, PathBuf},
          process::Command};

use serde_json::Value;
fn content(path: &Path) -> Vec<u8>
{
    let bytes = fs::read(path).unwrap();
    let header = b"P6\n640 480\n255\n";
    assert!(bytes.starts_with(header));
    let rgb = &bytes[header.len()..];
    assert_eq!(rgb.len(), 640 * 480 * 3);
    (100..400).flat_map(|y| rgb[(y * 640 + 100) * 3..(y * 640 + 540) * 3].iter().copied()).collect()
}
#[test]
#[ignore = "requires freshly built System image and private graphical QEMU"]
fn deferred_close_acceptance_preserves_window_until_explicit_application_close()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join(format!("results/system7/deferred-close-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/\
                                                        capture_system7_deferred_close.py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .current_dir(&root)
                                        .env("CARGO_INCREMENTAL", "0")
                                        .output()
                                        .unwrap();
    assert!(result.status.success(),
            "capture transport failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["disk_sha256_before"], report["disk_sha256_after"]);
    let observations = report["observations"].as_array().unwrap();
    let get = |name: &str| observations.iter().find(|row| row["name"] == name).unwrap();
    let baseline = &get("baseline")["state"];
    let deferred_id = baseline["front"].as_u64().unwrap();
    let automatic_id = get("explicit-close")["state"]["front"].as_u64().unwrap();
    assert_ne!(deferred_id, automatic_id);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(serial.contains("DEFERRED_WINDOW_POOL_USERSPACE_PASS reuse=passed stale=passed"),
            "guest must prove storage reuse and stale rejection through the public ledger");
    for name in ["baseline", "cancelled", "accepted", "accepted-again"]
    {
        let state = &get(name)["state"];
        assert_eq!(state["deferred"], 1, "{name} lost the live window");
        assert_eq!(state["front"], deferred_id);
        assert_eq!(state["updates"], baseline["updates"],
                   "{name} emitted an unnecessary application Update");
        assert_eq!(content(&output.join(get(name)["frame"].as_str().unwrap())),
                   content(&output.join(get("baseline")["frame"].as_str().unwrap())),
                   "{name} changed retained content");
    }
    assert_eq!(get("cancelled")["state"]["requests"], 0);
    assert_eq!(get("accepted")["state"]["requests"], 1);
    assert_eq!(get("accepted-again")["state"]["requests"], 1);
    assert_eq!(report["requests"].as_array().unwrap().len(), 1);
    assert_eq!(report["requests"][0], format!("DEFERRED_REQUEST id={deferred_id} alive=1 count=1"));
    assert_eq!(report["disposals"][0], format!("DEFERRED_DISPOSE id={deferred_id} alive=0"));
    assert_eq!(get("explicit-close")["state"]["deferred"], 0);
    assert_eq!(get("explicit-close")["state"]["automatic"], 1);
    assert_eq!(get("automatic-close")["state"]["automatic"], 0);
    assert_eq!(get("automatic-close")["state"]["requests"], 1);
    assert_eq!(get("automatic-close")["state"]["notices"], 1);
    assert_eq!(report["automatic_notices"][0],
               format!("AUTOMATIC_NOTICE id={automatic_id} count=1"));
    assert!(!output.join("vm/system7.qcow2").exists());
    let summary = serde_json::json!({"passed":true,"requests":1,"cancelled_requests":0,"retained_content_pixels":440*300,"unnecessary_updates":0,"explicit_dispose":true,"default_automatic_close":true});
    fs::write(root.join("results/system7/deferred-close-checks.json"),
              serde_json::to_vec_pretty(&summary).unwrap()).unwrap();
}
