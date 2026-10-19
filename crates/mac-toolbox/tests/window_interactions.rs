//! Real userspace Window Manager acceptance. The capture transport only drives
//! virtio input and records returned state/framebuffers; assertions live here.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::{Path, PathBuf},
          process::Command};

use serde_json::Value;

struct Frame
{
    width:  usize,
    height: usize,
    rgb:    Vec<u8>
}
impl Frame
{
    fn read(path: &Path) -> Self
    {
        let bytes = fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let mut at = 0;
        fn token<'a>(bytes: &'a [u8], at: &mut usize) -> &'a [u8]
        {
            loop
            {
                while bytes.get(*at).is_some_and(u8::is_ascii_whitespace)
                {
                    *at += 1;
                }
                if bytes.get(*at) != Some(&b'#')
                {
                    break;
                }
                while bytes.get(*at).is_some_and(|b| *b != b'\n')
                {
                    *at += 1;
                }
            }
            let start = *at;
            while bytes.get(*at).is_some_and(|b| !b.is_ascii_whitespace())
            {
                *at += 1;
            }
            &bytes[start..*at]
        }
        assert_eq!(token(&bytes, &mut at), b"P6");
        let width = std::str::from_utf8(token(&bytes, &mut at)).unwrap().parse::<usize>().unwrap();
        let height = std::str::from_utf8(token(&bytes, &mut at)).unwrap().parse::<usize>().unwrap();
        assert_eq!(token(&bytes, &mut at), b"255");
        assert!(bytes[at].is_ascii_whitespace());
        at += if bytes.get(at..at + 2) == Some(b"\r\n") { 2 } else { 1 };
        let rgb = bytes[at..].to_vec();
        assert_eq!(rgb.len(), width * height * 3);
        Self { width,
               height,
               rgb }
    }

    fn pixel(&self, x: usize, y: usize) -> &[u8]
    {
        &self.rgb[(y * self.width + x) * 3..(y * self.width + x + 1) * 3]
    }

    fn assert_same(&self, other: &Self, reason: &str)
    {
        assert_eq!((self.width, self.height), (other.width, other.height));
        let different = self.rgb
                            .as_chunks::<3>()
                            .0
                            .iter()
                            .zip(other.rgb.as_chunks::<3>().0.iter())
                            .filter(|(a, b)| a != b)
                            .count();
        assert_eq!(different, 0, "{reason}: differing RGB pixels");
    }
}

#[test]
#[ignore = "requires freshly built System/Finder disk image and qemu-system-riscv64; run after \
            cargo xtask image"]
fn real_userspace_window_tracking_commits_on_release_and_cancels_cleanly()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("window-tracking-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/\
                                                        capture_system7_window_tracking.py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .current_dir(&root)
                                        .output()
                                        .expect("start isolated capture");
    assert!(result.status.success(),
            "capture transport failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["disk_sha256_before"], report["disk_sha256_after"],
               "readonly fixture disk changed");
    let observations = report["observations"].as_array().unwrap();
    let get = |name: &str| {
        observations.iter()
                    .find(|o| o["name"] == name)
                    .unwrap_or_else(|| panic!("missing observation {name}"))
    };
    let frame = |name: &str| Frame::read(&output.join(get(name)["frame"].as_str().unwrap()));
    let bounds = |name: &str, expected: &str| {
        let state = &get(name)["state"];
        assert_eq!(state["first"], expected, "{name} window bounds");
        assert_eq!(state["windows"], "1", "{name} window count");
        assert_eq!(state["front"], "1", "{name} z order");
    };
    for name in ["baseline", "drag-held", "drag-outside-held", "drag-cancelled", "drag-reentered"]
    {
        bounds(name, "8,46,412,234");
    }
    assert_eq!(get("drag-held")["state"]["updates"],
               get("baseline")["state"]["updates"],
               "held drag emitted an application update");
    let baseline = frame("baseline");
    assert_eq!((baseline.width, baseline.height), (640, 480));
    baseline.assert_same(&frame("drag-cancelled"), "cancelled drag changed composed scene");
    bounds("drag-committed", "68,79,472,267");
    assert_eq!(get("drag-committed")["state"]["action"], "DRAG");
    bounds("grow-held", "68,79,472,267");
    assert_eq!(get("grow-held")["state"]["updates"],
               get("drag-committed")["state"]["updates"],
               "held grow emitted an application update");
    bounds("grow-committed", "68,79,412,237");
    assert_eq!(get("grow-committed")["state"]["action"], "RESIZE");
    bounds("roundtrip", "8,46,412,234");
    baseline.assert_same(&frame("roundtrip"), "drag/grow roundtrip left stale pixels");
    for name in ["close-held", "close-left", "close-reentered", "close-cancelled", "zoom-held",
                 "zoom-left", "zoom-reentered", "zoom-cancelled"]
    {
        bounds(name, "8,46,412,234");
    }
    frame("close-held").assert_same(&frame("close-reentered"),
                                    "close reentry failed to restore pressed pixels");
    frame("zoom-held").assert_same(&frame("zoom-reentered"),
                                   "zoom reentry failed to restore pressed pixels");
    baseline.assert_same(&frame("close-cancelled"), "close cancellation changed pixels");
    baseline.assert_same(&frame("zoom-cancelled"), "zoom cancellation changed pixels");
    bounds("zoom-committed", "1,44,638,457");
    bounds("zoom-restored", "8,46,412,234");
    baseline.assert_same(&frame("zoom-restored"), "zoom roundtrip changed pixels");
    for (name, bounds, front) in [("background-baseline", "8,46,412,234", "2"),
                                  ("command-drag-held", "8,46,412,234", "2"),
                                  ("command-drag-committed", "68,79,472,267", "2"),
                                  ("background-drag-cancelled", "68,79,472,267", "2"),
                                  ("background-drag-held", "68,79,472,267", "2"),
                                  ("background-drag-activated", "8,46,412,234", "1")]
    {
        assert_eq!(get(name)["state"]["windows"], "2", "{name} count");
        assert_eq!(get(name)["state"]["front"], front, "{name} activation/z order");
        assert_eq!(get(name)["state"]["first"], bounds, "{name} bounds");
    }
    assert_eq!(get("close-accepted")["state"]["windows"], "1");
    assert_eq!(get("close-accepted")["state"]["front"], "2");
    assert_eq!(get("close-accepted")["state"]["first"], "0,0,0,0");
    assert_eq!(get("last-close-accepted")["state"]["windows"], "0");
    assert_eq!(get("last-close-accepted")["state"]["front"], "0");
    // Menu bar and document pixels stay fixed during the held preview. The
    // separate moving outline crosses other portions of the original content.
    let held = frame("drag-held");
    for y in 0..21
    {
        for x in 0..640
        {
            assert_eq!(baseline.pixel(x, y), held.pixel(x, y), "drag touched menu at{x},{y}");
        }
    }
    for y in 100..150
    {
        for x in 200..250
        {
            assert_eq!(baseline.pixel(x, y),
                       held.pixel(x, y),
                       "held drag moved document at{x},{y}");
        }
    }
    fs::write(output.join("rust-acceptance.json"), serde_json::to_vec_pretty(&serde_json::json!({"status":"passed", "assertions":"mac-toolbox/tests/window_interactions.rs", "observations":observations.len(), "transport":"isolated real RV64 userspace / public SDK / virtio input", "animation_scope":"held outline pixel oracle in separate aggregate test; exhaustive temporal sampling pending"})).unwrap()).unwrap();
}

#[test]
#[ignore = "requires built System disk and original tracking captures; real public SDK guest \
            fixture"]
fn original_drag_and_grow_outline_pixels_match_through_public_sdk()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("window-original-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/\
                                                        capture_system7_window_tracking.py"))
                                        .args(["--probe", "--output"])
                                        .arg(&output)
                                        .current_dir(&root)
                                        .output()
                                        .expect("start private userspace probe");
    assert!(result.status.success(),
            "capture failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["disk_sha256_before"], report["disk_sha256_after"]);
    let observations = report["observations"].as_array().unwrap();
    let get = |name: &str| observations.iter().find(|o| o["name"] == name).unwrap();
    let frame = |name: &str| Frame::read(&output.join(get(name)["frame"].as_str().unwrap()));
    let original: Value =
        serde_json::from_str(include_str!("data/original-window-tracking.json")).unwrap();
    let baseline = frame("baseline");
    let mut measurements = Vec::new();
    for (name, mask) in [("drag", include_bytes!("data/original-window-drag-mask.bin").as_slice()),
                         ("grow", include_bytes!("data/original-window-grow-mask.bin").as_slice())]
    {
        let held = frame(&format!("{name}-held"));
        let case = &original["cases"][name];
        let mut changed = 0;
        let mut wrong = Vec::new();
        for y in 0..480
        {
            for x in 0..640
            {
                let index = y * 640 + x;
                let expected = mask[index / 8] & (0x80 >> (index % 8)) != 0;
                let actual = baseline.pixel(x, y) != held.pixel(x, y);
                if actual
                {
                    changed += 1;
                }
                if expected != actual && wrong.len() < 20
                {
                    wrong.push((x, y, expected, actual));
                }
            }
        }
        assert!(wrong.is_empty(), "{name} original outline mask differs: {wrong:?}");
        assert_eq!(changed, case["changed_pixels"].as_u64().unwrap() as usize);
        let mut common_source_pixels = 0;
        for item in case["observed_rgb_pairs"].as_array().unwrap()
        {
            let x = item[0].as_u64().unwrap() as usize;
            let y = item[1].as_u64().unwrap() as usize;
            let before = item[2].as_u64().unwrap() as u32;
            let after = item[3].as_u64().unwrap() as u32;
            let before = [(before >> 16) as u8, (before >> 8) as u8, before as u8];
            let after = [(after >> 16) as u8, (after >> 8) as u8, after as u8];
            // Original application has control samples; this minimal public-SDK
            // probe has white content. Compare color only for identical source
            // pixels; the complete changed-pixel geometry is checked above.
            if baseline.pixel(x, y) == before
            {
                common_source_pixels += 1;
                assert_eq!(held.pixel(x, y), after, "{name} indexed XOR at{x},{y}");
            }
        }
        assert!(common_source_pixels > changed / 2, "too little common original content");
        assert_eq!(get(&format!("{name}-held"))["state"]["first"], "100,100,540,400");
        let expected = case["committed_bounds"].as_array()
                                               .unwrap()
                                               .iter()
                                               .map(ToString::to_string)
                                               .collect::<Vec<_>>()
                                               .join(",");
        assert_eq!(get(&format!("{name}-committed"))["state"]["first"], expected);
        baseline.assert_same(&frame(&format!("{name}-restored")),
                             "original fixture roundtrip left stale pixels");
        measurements.push(serde_json::json!({"case":name,"exact_mask_pixels":640*480,"changed_pixels":changed,"common_original_rgb_pairs":common_source_pixels}));
    }
    assert_eq!(get("grow-no-motion")["state"]["first"], "100,100,540,400");
    baseline.assert_same(&frame("grow-no-motion"), "no-motion grow changed scene");
    fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&serde_json::json!({"status":"passed","source":"public SDK-only guest probe", "original":"provenance/control-tracking-reference.json", "cases":measurements})).unwrap()).unwrap();
}
