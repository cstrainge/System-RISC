//! Canonical end-to-end Finder set/geometry/performance acceptance.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};
#[test]
#[ignore = "requires current private image and Linux QEMU"]
fn cached_selection_masks_and_marquee_stay_bounded_through_real_services()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join(format!("results/system7/finder-selection-{}", std::process::id()));
    let capture = Command::new("python3").arg(root.join("System7/scripts/\
                                                         capture_system7_finder_selection.py"))
                                         .arg("--output")
                                         .arg(&output)
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
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    for error in ["FINDER_SELECTION_FAIL", "SYSTEM_TRAP:", "SYSTEM_PANIC:", "FINDER_ERROR"]
    {
        assert!(!serial.contains(error), "{serial}");
    }
    assert!(serial.contains("FINDER_SELECTION_USERSPACE_PASS stages=16 outline_steps=2000\n"),
            "{serial}");
    let stages = report["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 16);
    assert!(serial.contains("FINDER_INFO_MULTIPLE_TOTALS selected=10 logical=0 \
                             allocated=Some(0) error=None"),
            "{serial}");
    let all = stages.iter().find(|stage| stage["name"] == "select-all").unwrap();
    let stress = stages.iter().find(|stage| stage["name"] == "outline-stress").unwrap();
    assert_eq!(all["state"]["compositions"], stress["state"]["compositions"],
               "outline-only motion ran the scene compositor");
    assert_eq!(all["state"]["menu_paints"], stress["state"]["menu_paints"],
               "outline motion repainted menus");
    let frame = |name: &str| -> Vec<u8> {
        let stage = stages.iter().find(|stage| stage["name"] == name).unwrap();
        let bytes = fs::read(output.join(stage["frame"].as_str().unwrap())).unwrap();
        assert!(bytes.starts_with(b"P6\n640 480\n255\n"));
        bytes[b"P6\n640 480\n255\n".len()..].to_vec()
    };
    assert_eq!(frame("shift-add"), frame("selected-noop"), "no-change click changed pixels");
    assert_eq!(frame("select-all"),
               frame("outline-stress"),
               "transient outline left damage after 2000 moves");
    let geometry =
        serial.lines().find(|line| line.starts_with("FINDER_SELECTION_BOUNDS ")).expect(&serial);
    let fields: Vec<i16> =
        geometry.split_whitespace().skip(1).map(|word| word.parse().unwrap()).collect();
    assert_eq!(fields.len(), 8);
    let first = &fields[..4];
    let second = &fields[4..];
    let mut comparisons = 0;
    for (before, after, mask) in [("baseline", "single", vec![first]),
                                  ("single", "shift-add", vec![second]),
                                  ("selected-noop", "shift-toggle", vec![first])]
    {
        let before = frame(before);
        let after = frame(after);
        let mut changed = 0;
        for y in 0..480i16
        {
            for x in 0..640i16
            {
                let offset = (usize::try_from(y).unwrap() * 640 + usize::try_from(x).unwrap()) * 3;
                if before[offset..offset + 3] != after[offset..offset + 3]
                {
                    assert!(mask.iter().any(|r| x >= r[0] && y >= r[1] && x < r[2] && y < r[3]),
                            "damage outside changed item at {x},{y}");
                    changed += 1;
                }
                comparisons += 1;
            }
        }
        assert!(changed > 0, "selection did not change pixels");
    }
    assert_eq!(frame("wrapped-selected"), frame("wrapped-noop"));
    assert_eq!(frame("wrapped-selected"), frame("wrapped-refreshed"));
    let wrapped_bounds: Vec<i16> = serial.lines()
                                         .find(|line| line.starts_with("FINDER_WRAPPED_BOUNDS "))
                                         .expect(&serial)
                                         .split_whitespace()
                                         .skip(1)
                                         .map(|s| s.parse().unwrap())
                                         .collect();
    assert_eq!(wrapped_bounds.len(), 4);
    assert!(wrapped_bounds[2] - wrapped_bounds[0] <= 100);
    assert!(wrapped_bounds[3] - wrapped_bounds[1] > 44);
    let before = frame("wrapped");
    let after = frame("wrapped-selected");
    let mut changed = 0;
    for y in 0..480i16
    {
        for x in 0..640i16
        {
            let offset = (y as usize * 640 + x as usize) * 3;
            if before[offset..offset + 3] != after[offset..offset + 3]
            {
                assert!(x >= wrapped_bounds[0]
                        && x < wrapped_bounds[2]
                        && y >= wrapped_bounds[1]
                        && y < wrapped_bounds[3],
                        "wrapped caption repaint escaped its mask at {x},{y}");
                changed += 1;
            }
        }
    }
    assert!(changed > 0);
    let actions: Vec<_> =
        serial.lines().filter(|line| line.starts_with("FINDER_SELECTION_ACTION ")).collect();
    assert!(actions.len() >= 13, "{serial}");
    assert!(actions.iter().all(|line| line.contains("files=0 widths=0")), "{serial}");
    fs::write(output.join("acceptance.json"),serde_json::to_vec_pretty(&serde_json::json!({"status":"passed","pixel_comparisons":comparisons,"outline_steps":2000,"pointer_file_calls":0,"pointer_text_measurements":0,"selection_actions":actions})).unwrap()).unwrap();
}
