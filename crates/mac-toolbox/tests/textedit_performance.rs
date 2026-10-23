//! Actual Text Edit paths; transport captures counters and trace, Rust asserts.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};

use serde_json::Value;
#[test]
#[ignore = "requires optimized event-trace image and QEMU"]
fn caret_xor_is_bounded_and_typing_rewraps_only_the_affected_run()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join(format!("results/system7/textedit-performance-after-{}", std::process::id()));
    let capture = Command::new("python3").arg(root.join("System7/scripts/\
                                                         capture_text_edit_performance.py"))
                                         .arg("--output")
                                         .arg(&output)
                                         .output()
                                         .unwrap();
    assert!(capture.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    let mut wraps = Vec::new();
    for phase in report["stages"].as_array().unwrap()
    {
        let name = phase["name"].as_str().unwrap();
        assert_eq!(phase["overwritten"], 0, "lost evidence in {name}");
        let records = phase["records"].as_array().unwrap();
        let text_records: Vec<_> = records.iter().filter(|r| r[2] == 0).collect();
        if name.ends_with("typing")
        {
            assert_eq!(phase["logs"], 0, "typing must not write diagnostic logs");
            let work: Vec<_> =
                text_records.iter().filter(|r| r[4] == 0).map(|r| r[5].as_u64().unwrap()).collect();
            assert_eq!(work.len(), 20, "{name}");
            assert!(work.iter().all(|&bytes| bytes <= 128),
                    "invisible suffix rewrapped in {name}: {work:?}");
            wraps.push(work.iter().sum::<u64>());
        }
        if name.ends_with("blink")
        {
            assert_eq!(phase["updates"], 0, "blink entered app drawing in {name}");
            assert_eq!(phase["scrollbar_configures"], 0);
            assert!(text_records.iter().all(|r| r[4] == 3),
                    "blink allocated a snapshot, rewrapped or emitted text: {text_records:?}");
            assert_eq!(text_records.len(), 20);
            assert!(text_records.iter().all(|r| r[5] == 1 && r[6].as_u64().unwrap() <= 20),
                    "caret damage escaped its1px column");
            assert!(records.iter().all(|r| r[2] != 7), "blink recomposited retained text");
            let copies: Vec<_> = records.iter().filter(|r| r[2] == 14).collect();
            assert_eq!(copies.len(), 20, "{name}");
            assert!(copies.iter().all(|r| r[5].as_u64().unwrap() <= 20),
                    "framebuffer present exceeded the caret in {name}");
        }
    }
    assert_eq!(wraps[0], wraps[1], "typing layout work depends on the untouched document suffix");
    fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&serde_json::json!({"status":"passed","capture":report})).unwrap()).unwrap();
}
