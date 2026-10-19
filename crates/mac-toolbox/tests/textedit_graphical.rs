//! Original 1-bit GWorld TextEdit pixels compared against actual resident SDK calls.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};

use serde_json::Value;

#[test]
#[ignore = "requires cargo xtask image and original System 7 TextEdit captures"]
fn resident_textedit_matches_original_text_selection_clipping_caret_and_scroll()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join(format!("results/system7/textedit-{}", std::process::id()));
    let capture = Command::new("python3").arg(root.join("System7/scripts/\
                                                         capture_system7_textedit.py"))
                                         .arg("--output")
                                         .arg(&output)
                                         .current_dir(&root)
                                         .output()
                                         .unwrap();
    assert!(capture.status.success(),
            "capture failed:\n{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"], "base OS changed");
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(serial.contains("TRAPS_READY: resident ecall dispatcher ABI=5"));
    assert!(!serial.contains("TEXTEDIT_PROBE_FAIL"), "{serial}");
    let finder_serial = fs::read_to_string(output.join("finder-vm/serial.log")).unwrap();
    assert!(finder_serial.contains("FINDER_MENU menu=1257 item=1 command=7465646d"),
            "resource-based TextEdit command missing: {finder_serial}");
    assert!(!finder_serial.contains("FINDER_ERROR"), "{finder_serial}");
    let steps = report["finder_steps"].as_array().unwrap();
    for (name, expected) in [("type-hello", "length=5 selection=5..5"),
                             ("select-hello", "length=5 selection=0..5"),
                             ("copy", "length=5 selection=0..5"),
                             ("cut", "length=0 selection=0..0"),
                             ("paste", "length=5 selection=5..5"),
                             ("paste-again", "length=10 selection=10..10"),
                             ("select-ten", "length=10 selection=0..10"),
                             ("copy-ten", "length=10 selection=0..10")]
    {
        let step = steps.iter().find(|step| step["name"] == name).unwrap();
        assert!(step["log"].as_str()
                           .unwrap()
                           .contains(&format!("FINDER_TEXTEDIT_STATE {expected}\n")),
                "{name}: actual Finder resident editor state differs: {step}");
    }
    let resize = steps.iter().find(|step| step["name"] == "resize").unwrap();
    assert!(resize["log"].as_str()
                         .unwrap()
                         .contains("FINDER_TEXTEDIT_LAYOUT width=420 height=260\n"),
            "resizable Finder editor did not relayout through TextEdit: {resize}");
    let original: Value =
        serde_json::from_str(include_str!("data/textedit/original.json")).unwrap();
    let mut pixels = 0;
    let mut cases = 0;
    for snapshot in report["snapshots"].as_array().unwrap()
    {
        let name = snapshot["name"].as_str().unwrap();
        let reference = original.as_array()
                                .unwrap()
                                .iter()
                                .find(|r| r["case"] == "pixels" && r["operation"] == name)
                                .unwrap();
        let frame = fs::read(output.join(snapshot["frame"].as_str().unwrap())).unwrap();
        let header = b"P6\n640 480\n255\n";
        assert!(frame.starts_with(header));
        assert_eq!(frame.len(), header.len() + 640 * 480 * 3);
        let mut differences = Vec::new();
        for (y, row) in reference["rows"].as_array().unwrap().iter().enumerate()
        {
            let row = row.as_str().unwrap();
            assert_eq!(row.len(), 32);
            for x in 0..128
            {
                let byte = u8::from_str_radix(&row[x / 8 * 2..x / 8 * 2 + 2], 16).unwrap();
                let expected = if byte & (0x80 >> (x % 8)) == 0 { 255 } else { 0 };
                let at = header.len() + ((100 + y) * 640 + 100 + x) * 3;
                if frame[at..at + 3] != [expected; 3]
                {
                    differences.push((x, y, frame[at..at + 3].to_vec(), expected));
                }
                pixels += 1;
            }
        }
        assert!(differences.is_empty(),
                "{name}: {} differing original pixels, first {:?}; capture {}",
                differences.len(),
                &differences[..differences.len().min(24)],
                output.display());
        cases += 1;
    }
    assert_eq!(cases, 7);
    assert_eq!(pixels, 7 * 128 * 96);
    let acceptance = serde_json::json!({"status":"passed","cases":cases,"original_pixels":pixels,"pixel_differences":0,"capture":report,"output":output});
    fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
    fs::write(root.join("results/system7/textedit-graphical-checks.json"),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
}
