//! A native failed full panic paint cannot publish partial content or strand updates.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/display.rs"]
mod display;
use std::{fs, path::PathBuf, process::Command};
#[test]
#[ignore = "requires current private image and Linux QEMU"]
fn failed_full_crash_paint_preserves_pixels_and_releases_the_update_for_retry()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join(format!("results/system7/crash-paint-refusal-{}", std::process::id()));
    let capture = Command::new("python3").arg(root.join("System7/scripts/\
                                                         capture_system7_crash_paint_refusal.py"))
                                         .arg("--output")
                                         .arg(&output)
                                         .current_dir(&root)
                                         .output()
                                         .unwrap();
    assert!(capture.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    assert_eq!(report["owned_vm_exited"], true);
    assert_eq!(report["mutation"]["resource_type"], "STR#");
    assert_eq!(report["mutation"]["resource_id"], -16400);
    assert_eq!(report["mutation"]["index"], 9);
    assert_eq!(report["mutation"]["original_warning"], "Report truncated");
    assert_eq!(report["mutation"]["replacement_macroman_bytes"], 255);
    assert_eq!(report["mutation"]["replacement_utf8_bytes"], 510);
    let unchanged = report["mutation"]["other_resources_unchanged"].as_u64().unwrap();
    assert!(unchanged > 0);
    assert_eq!(unchanged + 1, report["mutation"]["system_resource_count"].as_u64().unwrap());
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    for error in ["CRASH_PAINT_FAIL", "SYSTEM_ALLOCATION_FAILED", "SYSTEM_PANIC:", "SYSTEM_TRAP:"]
    {
        assert!(!serial.contains(error), "{serial}");
    }
    assert!(serial.contains("CRASH_PAINT_USERSPACE_PASS full_cancel=passed masked_retry=passed \
                             reset_retry=passed\n"),
            "{serial}");
    assert!(serial.contains(&format!("CRASH_PAINT_REFUSED status={} report=retained \
                                      window=live\n",
                                     mac_abi::NO_MEMORY)),
            "{serial}");
    let bounds: Vec<usize> = serial.lines()
                                   .find(|line| line.starts_with("CRASH_PAINT_CONTENT "))
                                   .expect(&serial)
                                   .split_whitespace()
                                   .skip(1)
                                   .map(|word| word.parse().unwrap())
                                   .collect();
    assert_eq!(bounds.len(), 4);
    let [left, top, right, bottom]: [usize; 4] = bounds.try_into().unwrap();
    assert!(left < right
            && top < bottom
            && right <= display::width()
            && bottom <= display::height());
    let stages = report["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 4);
    let frame = |name| {
        let stage = stages.iter().find(|stage| stage["stage"] == name).unwrap();
        let pixels = fs::read(output.join(stage["frame"].as_str().unwrap())).unwrap();
        assert!(pixels.starts_with(&display::header()));
        assert_eq!(pixels.len(),
                   display::header().len() + display::width() * display::height() * 3);
        pixels
    };
    let baseline = frame("baseline");
    let refused = frame("refused");
    let after_mask = frame("after-mask");
    let retry = frame("retry");
    let mut comparisons = 0;
    let mut ink = 0;
    for y in top..bottom
    {
        for x in left..right
        {
            let at = display::header().len() + (y * display::width() + x) * 3;
            assert_eq!(&baseline[at..at + 3],
                       &refused[at..at + 3],
                       "failed full paint leaked at {x},{y}");
            assert_eq!(&baseline[at..at + 3],
                       &after_mask[at..at + 3],
                       "cancelled retry mask leaked at {x},{y}");
            comparisons += 2;
            if retry[at..at + 3] != [255; 3]
            {
                ink += 1;
            }
        }
    }
    assert!(ink > 1000, "retry must draw a live report, not an empty window");
    fs::write(output.join("acceptance.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "status":"passed", "pixel_comparisons":comparisons,
        "failure":"oversized MacRoman warning -> native NoMemory",
        "full_update_cancelled":true,"same_window_mask_retry":true,"new_report_retry":true
    })).unwrap()).unwrap();
}
