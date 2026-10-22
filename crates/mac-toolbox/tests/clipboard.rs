//! Original Finder viewer pixels, using the same Finder source over real ECALLs.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::{Path, PathBuf},
          process::Command};

use serde_json::Value;

fn frame(path: &Path) -> Vec<u8>
{
    let bytes = fs::read(path).unwrap();
    let header = b"P6\n640 480\n255\n";
    assert!(bytes.starts_with(header));
    assert_eq!(bytes.len(), header.len() + 640 * 480 * 3);
    bytes[header.len()..].to_vec()
}

#[test]
#[ignore = "requires cargo xtask image and original Finder clipboard captures"]
fn resource_based_finder_clipboard_matches_original_empty_and_text_pixels()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join(format!("results/system7/clipboard-{}", std::process::id()));
    let captured = Command::new("python3").arg(root.join("System7/scripts/\
                                                          capture_system7_clipboard.py"))
                                          .arg("--output")
                                          .arg(&output)
                                          .current_dir(&root)
                                          .output()
                                          .unwrap();
    assert!(captured.status.success(),
            "capture failed:\n{}\n{}",
            String::from_utf8_lossy(&captured.stdout),
            String::from_utf8_lossy(&captured.stderr));
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"], "base OS artifacts changed");
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(serial.contains("TRAPS_READY: resident ecall dispatcher ABI=8"));
    assert!(!serial.contains("CLIPBOARD_PROBE_FAIL"), "{serial}");
    for definition in ["CODE_LOADED WDEF0", "CODE_LOADED CDEF1"]
    {
        assert!(serial.contains(definition), "definition absent: {definition}\n{serial}");
    }
    for marker in ["CLIPBOARD_UNRELATED_ACTIVATE_PASS commits=0",
                   "CLIPBOARD_SCROLL_STATIONARY_PASS nulls=100 commits=0",
                   "CLIPBOARD_SCROLL_MOVED_PASS", "CLIPBOARD_SCROLL_RELEASE_PASS commits=0"]
    {
        assert!(serial.contains(marker),
                "missing canonical input/render assertion {marker}:\n{serial}");
    }
    let finder_serial = fs::read_to_string(output.join("finder-vm/serial.log")).unwrap();
    assert!(finder_serial.contains("FINDER_MENU menu=1253 item=9 command=7368636c"),
            "Show Clipboard resource command was not dispatched:\n{finder_serial}");
    let observations = report["snapshots"].as_array().unwrap();
    let image = |name: &str| {
        let observation = observations.iter().find(|v| v["name"] == name).unwrap();
        frame(&output.join(observation["frame"].as_str().unwrap()))
    };
    let originals: Value =
        serde_json::from_str(include_str!("data/clipboard/original.json")).unwrap();
    let mut pixels = 0;
    for (name, original_case, expected) in
        [("finder-empty", "empty", include_bytes!("data/clipboard/empty.rgb").as_slice()),
         ("empty", "empty", include_bytes!("data/clipboard/empty.rgb").as_slice()),
         ("text", "text-abc", include_bytes!("data/clipboard/text-abc.rgb").as_slice())]
    {
        let original =
            originals.as_array().unwrap().iter().find(|v| v["case"] == original_case).unwrap();
        let crop = &original["crop"];
        let (left, top, right, bottom) = (crop["left"].as_u64().unwrap() as usize,
                                          crop["top"].as_u64().unwrap() as usize,
                                          crop["right"].as_u64().unwrap() as usize,
                                          crop["bottom"].as_u64().unwrap() as usize);
        assert_eq!(expected.len(), (right - left) * (bottom - top) * 3);
        let actual = image(name);
        let mut differences = Vec::new();
        for y in top..bottom
        {
            for x in left..right
            {
                let at = (y * 640 + x) * 3;
                let reference = ((y - top) * (right - left) + x - left) * 3;
                if actual[at..at + 3] != expected[reference..reference + 3]
                {
                    differences.push((x,
                                      y,
                                      actual[at..at + 3].to_vec(),
                                      expected[reference..reference + 3].to_vec()));
                }
                pixels += 1;
            }
        }
        assert!(differences.is_empty(),
                "{name}: {} differing original pixels; first {:?}; capture {}",
                differences.len(),
                &differences[..differences.len().min(20)],
                output.display());
    }
    assert_eq!(image("empty"), image("empty-again"), "clearing text left stale drawing");
    let acceptance = serde_json::json!({"status":"passed", "transport":"same Finder source and real public SDK ECALL",
        "original_pixels":pixels,"pixel_differences":0,"clear_roundtrip":"passed","stationary_thumb_nulls":100,"idle_policy_file_calls":0,"idle_policy_menu_calls":0,"stationary_and_release_commits":0,"unrelated_activation_commits":0,"scroll_movement":"passed","capture":report,"output":output});
    fs::write(output.join("rust-acceptance.json"), serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
    fs::write(root.join("results/system7/clipboard-checks.json"),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
}
