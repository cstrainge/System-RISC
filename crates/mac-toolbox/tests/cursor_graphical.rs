//! Real Finder launch, native cursor compositing and automatic editor hover.
#![cfg(not(target_arch = "riscv64"))]
use std::{collections::BTreeSet,
          fs,
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

fn sprite(base: &[u8], shown: &[u8], point: [usize; 2], bytes: &[u8]) -> bool
{
    assert_eq!(bytes.len(), 68);
    let word = |at| u16::from_be_bytes([bytes[at], bytes[at + 1]]);
    let hotspot = [word(66) as usize, word(64) as usize];
    for y in point[1] - 16..point[1] + 16
    {
        for x in point[0] - 16..point[0] + 16
        {
            let at = (y * 640 + x) * 3;
            let column = x as isize - point[0] as isize + hotspot[0] as isize;
            let row = y as isize - point[1] as isize + hotspot[1] as isize;
            let mut expected: [u8; 3] = base[at..at + 3].try_into().unwrap();
            if (0..16).contains(&column) && (0..16).contains(&row)
            {
                let bit = 0x8000 >> column;
                let image = word(row as usize * 2) & bit != 0;
                let mask = word(32 + row as usize * 2) & bit != 0;
                if mask
                {
                    expected.fill(if image { 0 } else { 255 });
                }
                else if image
                {
                    expected.iter_mut().for_each(|v| *v ^= 255);
                }
            }
            if shown[at..at + 3] != expected
            {
                return false;
            }
        }
    }
    true
}

fn clear(base: &[u8], shown: &[u8], [x, y]: [usize; 2]) -> bool
{
    (y - 16..y + 16).all(|row| {
                        let at = (row * 640 + x - 16) * 3;
                        base[at..at + 32 * 3] == shown[at..at + 32 * 3]
                    })
}

#[test]
#[ignore = "requires current OS image and private graphical QEMU"]
fn installed_cursor_app_and_automatic_text_regions_render_original_sprites()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let windows = std::env::var_os("SYSTEM7_CURSOR_WINDOWS").is_some();
    let output = root.join(format!("results/system7/cursors-{}-{}",
                                   if windows { "windows" } else { "linux" },
                                   std::process::id()));
    let mut command = Command::new("python3");
    command.arg(root.join("System7/scripts/capture_system7_cursors.py"))
           .arg("--output")
           .arg(&output)
           .current_dir(&root);
    if windows
    {
        command.arg("--windows-headless");
    }
    let transport = command.output().unwrap();
    assert!(transport.status.success(),
            "cursor capture failed:\n{}\n{}",
            String::from_utf8_lossy(&transport.stdout),
            String::from_utf8_lossy(&transport.stderr));
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(serial.contains("CURSOR_TEST_READY\n"));
    assert!(!serial.contains("CURSOR_TEST_ERROR"), "{serial}");
    assert!(!serial.contains("SYSTEM_PANIC:"), "{serial}");
    let load = |name: &str| {
        let entry = report["frames"].as_array()
                                    .unwrap()
                                    .iter()
                                    .find(|entry| entry["name"] == name)
                                    .expect(name);
        frame(&output.join(entry["frame"].as_str().unwrap()))
    };
    let asset = |name: &str| fs::read(root.join("System7/system/assets").join(name)).unwrap();
    let arrow = asset("cursors/arrow.curs");
    let ibeam = asset("visual/43555253-1.bin");
    let pointer = &report["pointer_present"];
    let scene_before = fs::read(output.join(pointer["before"].as_str().unwrap())).unwrap();
    let scene_after = fs::read(output.join(pointer["after"].as_str().unwrap())).unwrap();
    assert_eq!(scene_before.len(), 640 * 480 * 4);
    assert_eq!(scene_before, scene_after, "pure pointer motion wrote the backbuffer");
    for field in ["scene_rev", "compositions", "updates", "menu_paints"]
    {
        assert_eq!(pointer["state_before"][field], pointer["state_after"][field],
                   "pure pointer motion did scene work: {field}");
    }
    let clean_scene: Vec<u8> = scene_after.chunks_exact(4)
                                          .flat_map(|pixel| {
                                              let pixel =
                                                  u32::from_le_bytes(pixel.try_into().unwrap());
                                              [(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8]
                                          })
                                          .collect();
    let moved = load("pointer-moved");
    assert!(sprite(&clean_scene, &moved, [143, 126], &arrow),
            "present-time sprite differs from the original arrow over the actual scene");
    let previous = load("pointer-before");
    for y in 0..480
    {
        for x in 0..640
        {
            // Both original and new 16x16 cursor boxes, hotspot (1,1).
            if !(141..158).contains(&x) || !(125..141).contains(&y)
            {
                let at = (y * 640 + x) * 3;
                assert_eq!(previous[at..at + 3],
                           moved[at..at + 3],
                           "pointer movement changed unrelated pixel {x},{y}");
            }
        }
    }
    let mut original_pixels = 0;
    for (name, point, data) in [("arrow", [142, 126], arrow.clone()),
                                ("ibeam", [271, 126], ibeam.clone()),
                                ("crosshair", [400, 126], asset("visual/43555253-2.bin")),
                                ("plus", [529, 126], asset("visual/43555253-3.bin")),
                                ("watch", [142, 200], asset("visual/43555253-4.bin")),
                                ("ring", [400, 200], asset("visual/43555253--5760.bin")),
                                ("ram-disk", [529, 200], asset("visual/43555253--20161.bin"))]
    {
        assert!(sprite(&load(&format!("{name}-base")),
                       &load(&format!("{name}-shown")),
                       point,
                       &data),
                "{name} differs from original cursor asset; {}",
                output.display());
        original_pixels += 32 * 32;
    }
    let watch_frames = [4,-6078,-6077,-6076,-6075,-6074,-6073,-6072]
        .map(|id| asset(&format!("visual/43555253-{id}.bin")));
    let mut observed_frames = BTreeSet::new();
    for index in 0..10
    {
        let shown = load(&format!("animation-{index}"));
        let matching =
            watch_frames.iter()
                        .position(|data| sprite(&load("animation-base"), &shown, [271, 200], data));
        observed_frames.insert(matching.expect("animated watch is not an original frame"));
        original_pixels += 32 * 32;
    }
    // QMP screenshot cadence can alias the animation period, particularly
    // across the Windows transport. Every frame must be valid and a stationary
    // pointer must show progression; exact sequence/wrapping has separate tests.
    assert!(observed_frames.len() >= 2, "timer animation did not progress: {observed_frames:?}");
    let base = load("editor-base");
    for (name, point) in [("editor-ibeam", [350, 350]),
                          ("editor-moved", [351, 350]),
                          ("editor-editable-again", [351, 350]),
                          ("editor-returned", [351, 350]),
                          ("menu-released-editor", [350, 350])]
    {
        assert!(sprite(&base, &load(name), point, &ibeam),
                "{name}: automatic I-beam missing; {}",
                output.display());
    }
    for (name, point) in
        [("editor-typed-hidden", [350, 350]), ("editor-obscured-after-show", [351, 350])]
    {
        assert!(clear(&base, &load(name), point), "{name}: obscured cursor is still drawn");
    }
    for (name, point) in [("editor-read-only", [351, 350]), ("menu-over-editor", [350, 350])]
    {
        assert!(sprite(&base, &load(name), point, &arrow), "{name}: arrow policy missing");
    }
    assert!(sprite(&load("title-bar-hidden"), &load("title-bar-base"), [350, 52], &arrow),
            "automatic I-beam leaked into title bar");
    let acceptance = serde_json::json!({"status":"passed", "original_sprite_pixels":original_pixels,
        "pointer_backbuffer_identity_bytes":scene_after.len(), "pointer_scene_recompositions":0,
        "observed_watch_frames":observed_frames, "automatic_hover_cases":10,
        "capture":report, "output":output});
    fs::write(output.join("rust-acceptance.json"), serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
    fs::write(root.join("results/system7/cursor-checks.json"),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
}
