//! Public-SDK frame atomicity while two actual native app threads poll events.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};

fn cursor_on_white(frame: &[u8], point: [usize; 2], cursor: &[u8], stage: &str)
{
    assert_eq!(cursor.len(), 68);
    let word = |at| u16::from_be_bytes([cursor[at], cursor[at + 1]]);
    let hotspot = [word(66) as isize, word(64) as isize];
    let header = b"P6\n640 480\n255\n";
    assert!(frame.starts_with(header));
    assert_eq!(frame.len(), header.len() + 640 * 480 * 3);
    for y in point[1] - 16..point[1] + 16
    {
        for x in point[0] - 16..point[0] + 16
        {
            let column = x as isize - point[0] as isize + hotspot[0];
            let row = y as isize - point[1] as isize + hotspot[1];
            let mut expected = [255; 3];
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
                    expected.fill(0);
                }
            }
            let offset = header.len() + (y * 640 + x) * 3;
            assert_eq!(&frame[offset..offset + 3],
                       expected,
                       "{stage}: incorrect committed cursor at ({x},{y}), hover {point:?}");
        }
    }
}

#[test]
#[ignore = "requires fresh cargo xtask image and QEMU"]
fn staged_window_drawing_stays_private_until_end_update()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join(format!("results/system7/retained-update-{}", std::process::id()));
    let capture = Command::new("python3").arg(root.join("System7/scripts/\
                                                         capture_system7_retained_update.py"))
                                         .arg("--output")
                                         .arg(&output)
                                         .current_dir(&root)
                                         .output()
                                         .unwrap();
    assert!(capture.status.success(),
            "transport failed:\n{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"], "base images changed");
    assert_eq!(report["owned_vm_exited"], true);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(!serial.contains("RETAINED_UPDATE_FAIL"), "{serial}");
    assert!(serial.contains("RETAINED_UPDATE_USERSPACE_PASS separate_threads=2 stages=42\n"),
            "{serial}");
    let stages = report["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 42);
    let header = b"P6\n640 480\n255\n";
    let mut checked = 0;
    let mut cursor_pixels = 0;
    for stage in stages
    {
        let name = stage["stage"].as_str().unwrap();
        if name.starts_with("cursor-")
        {
            let cursor_frames = stage["cursor_frames"].as_array().unwrap();
            assert_eq!(cursor_frames.len(), 2);
            for (index, expected_point) in [[380, 400], [550, 400]].into_iter().enumerate()
            {
                let entry = &cursor_frames[index];
                assert_eq!(entry["point"], serde_json::json!(expected_point));
                let ibeam = match name
                {
                    "cursor-left"
                    | "cursor-pending-right"
                    | "cursor-outside-patch"
                    | "cursor-restored-patch" => index == 0,
                    "cursor-right" => index == 1,
                    "cursor-omitted"
                    | "cursor-erased-patch"
                    | "cursor-pending-patch"
                    | "cursor-cancelled-patch" => false,
                    _ => panic!("unexpected cursor checkpoint {name}")
                };
                let cursor: &[u8] = if ibeam
                {
                    include_bytes!("../../../system/assets/visual/43555253-1.bin")
                }
                else
                {
                    include_bytes!("../../../system/assets/cursors/arrow.curs")
                };
                let frame = fs::read(output.join(entry["frame"].as_str().unwrap())).unwrap();
                cursor_on_white(&frame, expected_point, cursor, name);
                cursor_pixels += 32 * 32;
            }
            continue;
        }
        let frame = fs::read(output.join(stage["frame"].as_str().unwrap())).unwrap();
        assert!(frame.starts_with(header));
        assert_eq!(frame.len(), header.len() + 640 * 480 * 3);
        if name.starts_with("desktop-patch-")
        {
            let reference = |name: &str| {
                let row = stages.iter().find(|s| s["stage"] == name).unwrap();
                fs::read(output.join(row["frame"].as_str().unwrap())).unwrap()
            };
            if matches!(name, "desktop-patch-pending" | "desktop-patch-cancelled")
            {
                assert_eq!(frame,
                           reference("desktop-patch-covered"),
                           "{name}: exposed private drawing");
            }
            else if name == "desktop-patch-committed"
            {
                let before = reference("desktop-patch-covered");
                assert_eq!(stage["state"]["menu_paints"],
                           stages.iter().find(|s| s["stage"] == "desktop-patch-covered").unwrap()
                               ["state"]["menu_paints"]);
                let mut changes = 0;
                for y in 0..480
                {
                    for x in 0..640
                    {
                        let at = header.len() + (y * 640 + x) * 3;
                        if frame[at..at + 3] != before[at..at + 3]
                        {
                            assert!((80..300).contains(&x) && (180..280).contains(&y),
                                    "desktop mask escaped at {x},{y}");
                            assert!(!(90..290).contains(&x) || !(195..275).contains(&y),
                                    "desktop damaged covering window at {x},{y}");
                            assert_eq!(&frame[at..at + 3], &[0, 255, 0]);
                            changes += 1;
                        }
                    }
                }
                assert!(changes > 0);
            }
            else if matches!(name, "desktop-patch-revealed" | "desktop-patch-transparent")
            {
                let before = reference("desktop-patch-baseline");
                for y in 0..480
                {
                    for x in 0..640
                    {
                        let at = header.len() + (y * 640 + x) * 3;
                        let expected: &[u8] = if (80..300).contains(&x) && (180..280).contains(&y)
                        {
                            if name == "desktop-patch-transparent"
                            {
                                if (x + y) & 1 == 0
                                {
                                    &[0x87, 0x87, 0x87]
                                }
                                else
                                {
                                    &[0xc0, 0xc0, 0xc0]
                                }
                            }
                            else
                            {
                                &[0, 255, 0]
                            }
                        }
                        else
                        {
                            &before[at..at + 3]
                        };
                        assert_eq!(&frame[at..at + 3],
                                   expected,
                                   "desktop exposure differs at {x},{y}");
                    }
                }
            }
            else if name == "desktop-patch-outline"
            {
                assert_eq!(frame,
                           reference("desktop-patch-inactive"),
                           "desktop outline left damage");
                let baseline =
                    stages.iter().find(|s| s["stage"] == "desktop-patch-inactive").unwrap();
                assert_eq!(stage["state"]["compositions"], baseline["state"]["compositions"],
                           "desktop outline recomposed scene");
                assert_eq!(stage["state"]["menu_paints"], baseline["state"]["menu_paints"]);
            }
            continue;
        }
        if name.starts_with("mask-")
        {
            let baseline = stages.iter().find(|s| s["stage"] == "desktop-revealed").unwrap();
            let original = fs::read(output.join(baseline["frame"].as_str().unwrap())).unwrap();
            assert_eq!(stage["state"]["menu_paints"], baseline["state"]["menu_paints"],
                       "mask update repainted menus");
            for y in 0..480
            {
                for x in 0..640
                {
                    let at = header.len() + (y * 640 + x) * 3;
                    let expected: &[u8] = if name != "mask-pending-cyan"
                                             && (470..530).contains(&x)
                                             && (365..415).contains(&y)
                    {
                        &[0, 255, 255]
                    }
                    else
                    {
                        &original[at..at + 3]
                    };
                    assert_eq!(&frame[at..at + 3], expected, "{name}: mask damaged {x},{y}");
                    checked += 1;
                }
            }
            continue;
        }
        if name.starts_with("scroll-")
        {
            for row in 0..5
            {
                let expected = if name == "scroll-row-committed" && row == 0
                {
                    [255, 255, 255]
                }
                else
                {
                    [255, 0, 0]
                };
                for y in 360 + row * 16..370 + row * 16
                {
                    for x in 365..585
                    {
                        let offset = header.len() + (y * 640 + x) * 3;
                        assert_eq!(&frame[offset..offset + 3],
                                   expected,
                                   "{name}: shared row snapshot changed at {x},{y}");
                        checked += 1;
                    }
                }
            }
            continue;
        }
        // Exclude the standard definition's reserved scrollbar/grow strips.
        for y in 360..430
        {
            for x in 360..610
            {
                // Exclude the temporary cover window including its definition
                // chrome; the following stage checks the complete exposed area.
                if name == "pattern-occluded" && (385..550).contains(&x)
                {
                    continue;
                }
                let expected = match name
                {
                    "committed-red" | "pending-empty" | "pending-half" | "pending-full" =>
                    {
                        [255, 0, 0]
                    }
                    "committed-split" | "pending-yellow" =>
                    {
                        if x < 490 { [0, 255, 0] } else { [0, 0, 255] }
                    }
                    "committed-yellow" => [255, 255, 0],
                    "committed-pattern" | "appended-black" | "pattern-occluded"
                    | "pattern-revealed" | "desktop-cyan" | "desktop-pending"
                    | "desktop-cancelled" | "desktop-magenta" | "desktop-covered"
                    | "desktop-revealed" =>
                    {
                        if name != "committed-pattern"
                           && (390..450).contains(&x)
                           && (370..410).contains(&y)
                        {
                            [0, 0, 0]
                        }
                        else if (x - 350) & 7 == (y - 350) & 7
                        {
                            [0x12, 0x34, 0x56]
                        }
                        else
                        {
                            [0xfe, 0xdc, 0xba]
                        }
                    }
                    _ => panic!("unexpected checkpoint {name}")
                };
                let offset = header.len() + (y * 640 + x) * 3;
                assert_eq!(&frame[offset..offset + 3],
                           expected,
                           "{name}: pending drawing exposed or committed scene missing at \
                            ({x},{y}); capture {}",
                           output.display());
                checked += 1;
            }
        }
        if name.starts_with("desktop-")
        {
            let expected = match name
            {
                "desktop-cyan" | "desktop-pending" | "desktop-cancelled" => [0, 255, 255],
                "desktop-magenta" | "desktop-revealed" => [255, 0, 255],
                "desktop-covered" => [0x12, 0x34, 0x56],
                _ => unreachable!()
            };
            for y in 205..250
            {
                for x in 100..270
                {
                    let offset = header.len() + (y * 640 + x) * 3;
                    assert_eq!(&frame[offset..offset + 3],
                               expected,
                               "{name}: incorrect desktop commit/cache/exposure at ({x},{y})");
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(cursor_pixels, 18 * 32 * 32);
    let acceptance = serde_json::json!({"status":"passed","stages":42,"native_app_threads":2,
        "masked_window_updates_preserve_all_other_pixels_and_menu_cache":true,
        "scroll_rows_commit_atomically_without_mutating_other_rows":true,
        "pixels":checked,"pending_frames_retain_last_commit":true,
        "cursor_binding_pixels":cursor_pixels,"cursor_bindings_commit_atomically":true,
        "omitted_editor_removes_cursor_binding":true,
        "end_update_publishes_complete_scene":true,
        "pattern_phase_append_and_exposure":true,
        "desktop_commit_pending_cancel_and_exposure":true,
        "desktop_masks_preserve_occlusion_transparency_and_unrelated_pixels":true,
        "desktop_outline_steps_without_compositions":2000,"capture":report});
    fs::write(output.join("rust-acceptance.json"),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
    fs::write(root.join("results/system7/retained-update-checks.json"),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
}
