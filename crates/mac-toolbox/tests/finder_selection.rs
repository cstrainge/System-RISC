//! Canonical end-to-end Finder set/geometry/performance acceptance.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/display.rs"]
mod display;
use std::{fs, path::PathBuf, process::Command};
#[test]
#[ignore = "requires current private image and Linux QEMU"]
fn scrollbars_follow_complete_labels_reset_and_repaint_only_the_viewport()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join(format!("results/system7/finder-scroll-{}", std::process::id()));
    let capture = Command::new("python3").arg(root.join("System7/scripts/\
                                                         capture_system7_finder_selection.py"))
                                         .arg("--output")
                                         .arg(&output)
                                         .arg("--scroll-only")
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
    assert!(serial.contains("FINDER_SCROLL_USERSPACE_PASS label_overflow=1 bounded=true \
                             reset=true\n"),
            "{serial}");
    for failure in
        ["FINDER_SELECTION_FAIL", "SYSTEM_ALLOCATION_FAILED", "SYSTEM_PANIC:", "SYSTEM_TRAP:"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
}
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
    assert!(serial.contains("FINDER_SELECTION_USERSPACE_PASS stages=30 outline_steps=4000\n"),
            "{serial}");
    assert!(serial.contains("EVENT_COORDINATES_USERSPACE_PASS\n"), "{serial}");
    assert!(serial.contains("FINDER_DEFERRED_POINTER_USERSPACE_PASS ordered=passed \
                             modifiers=passed\n"),
            "{serial}");
    let stages = report["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 30);
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
        assert!(bytes.starts_with(&display::header()));
        bytes[display::header().len()..].to_vec()
    };
    assert_eq!(frame("shift-add"), frame("selected-noop"), "no-change click changed pixels");
    assert_eq!(frame("select-all"),
               frame("outline-stress"),
               "transient outline left damage after 2000 moves");
    let gap = serial.lines().find(|line| line.starts_with("FINDER_SOURCE_GAP_BOUNDS ")).unwrap();
    let coords: Vec<i16> = gap.split_whitespace().skip(1).map(|s| s.parse().unwrap()).collect();
    let before = frame("placement-auto-added");
    let after = frame("placement-source-gap");
    for y in 0..display::height() as i16
    {
        for x in 0..display::width() as i16
        {
            let departed = x >= coords[4] && x < coords[6] && y >= coords[5] && y < coords[7];
            let header = x >= coords[0] && x < coords[2] && y >= coords[1] && y < coords[1] + 20;
            let controls = x >= coords[0]
                           && x <= coords[2]
                           && y >= coords[1]
                           && y <= coords[3]
                           && (x >= coords[2] - 15 || y >= coords[3] - 15);
            if !departed && !header && !controls
            {
                let at = (y as usize * display::width() + x as usize) * 3;
                assert_eq!(&before[at..at + 3],
                           &after[at..at + 3],
                           "source refresh changed unrelated pixel {x},{y}");
            }
        }
    }
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
        for y in 0..display::height() as i16
        {
            for x in 0..display::width() as i16
            {
                let offset = (usize::try_from(y).unwrap() * display::width()
                              + usize::try_from(x).unwrap())
                             * 3;
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
    for y in 0..display::height() as i16
    {
        for x in 0..display::width() as i16
        {
            let offset = (y as usize * display::width() + x as usize) * 3;
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
    assert_eq!(frame("desktop-clear"), frame("desktop-band-cancel"), "desktop Escape left pixels");
    assert_eq!(frame("desktop-all"), frame("desktop-outline"), "desktop outline left pixels");
    assert_eq!(frame("desktop-single"), frame("desktop-collapse"));
    let desktop_all = stages.iter().find(|s| s["name"] == "desktop-all").unwrap();
    let desktop_outline = stages.iter().find(|s| s["name"] == "desktop-outline").unwrap();
    assert_eq!(desktop_all["state"]["compositions"], desktop_outline["state"]["compositions"]);
    assert_eq!(desktop_all["state"]["menu_paints"], desktop_outline["state"]["menu_paints"]);
    let trash: Vec<i16> = serial.lines()
                                .find(|line| line.starts_with("FINDER_DESKTOP_TRASH_BOUNDS "))
                                .unwrap()
                                .split_whitespace()
                                .skip(1)
                                .map(|s| s.parse().unwrap())
                                .collect();
    let before = frame("desktop-single");
    let after = frame("desktop-shift");
    for y in 0..display::height() as i16
    {
        for x in 0..display::width() as i16
        {
            let at = (y as usize * display::width() + x as usize) * 3;
            if before[at..at + 3] != after[at..at + 3]
            {
                assert!(x >= trash[0] && y >= trash[1] && x < trash[2] && y < trash[3],
                        "desktop selection escaped at {x},{y}");
            }
        }
    }
    let actions: Vec<_> =
        serial.lines().filter(|line| line.starts_with("FINDER_SELECTION_ACTION ")).collect();
    assert!(actions.len() >= 13, "{serial}");
    assert!(actions.iter().all(|line| line.contains("files=0 widths=0")), "{serial}");
    fs::write(output.join("acceptance.json"),serde_json::to_vec_pretty(&serde_json::json!({"status":"passed","pixel_comparisons":comparisons,"outline_steps":4000,"pointer_file_calls":0,"pointer_text_measurements":0,"selection_actions":actions})).unwrap()).unwrap();
}

#[test]
#[ignore = "requires current event-trace private image and physical QMP input"]
fn physical_marquee_motion_uses_cached_items_and_frontbuffer_feedback()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join(format!("results/system7/finder-marquee-{}", std::process::id()));
    let mut command = Command::new("python3");
    command.arg(root.join("System7/scripts/capture_finder_marquee.py"))
           .arg("--output")
           .arg(&output);
    if std::env::var_os("SYSTEM7_FINDER_MARQUEE_WINDOWS").is_some()
    {
        command.arg("--windows-headless");
    }
    let capture = command.output().unwrap();
    assert!(capture.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["owned_vm_exited"], true);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    for failure in ["SYSTEM_PANIC:", "SYSTEM_TRAP:", "SYSTEM_ALLOCATION_FAILED:", "FINDER_ERROR"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
    let mut profiles = Vec::new();
    for stage in report["stages"].as_array().unwrap()
    {
        let empty = stage["name"] == "empty";
        let trace = &stage["trace"];
        assert_eq!(trace["overwritten"], 0);
        let app = stage["state_before"]["front"].as_str().unwrap();
        let frame_line =
            serial.lines()
                  .find(|line| line.starts_with(&format!("FINDER_FOLDER_LAYOUT window={app} ")))
                  .unwrap();
        let frame: Vec<i16> = frame_line.split(" frame=")
                                        .nth(1)
                                        .unwrap()
                                        .split(' ')
                                        .next()
                                        .unwrap()
                                        .split(',')
                                        .map(|n| n.parse().unwrap())
                                        .collect();
        let anchor = (400 - frame[0], 220 - frame[1]);
        let mut inputs: Vec<(u64, u64)> = Vec::new();
        let mut published = 0;
        let mut pending = None;
        let mut latency = Vec::new();
        let mut render_count = 0;
        let mut icon_slow = Vec::new();
        for row in trace["records"].as_array().unwrap()
        {
            let row: Vec<u64> =
                row.as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
            match row[2]
            {
                1 if row[4] == 1 =>
                {
                    let point = (row[5] as i16 - frame[0], row[6] as i16 - frame[1]);
                    let edges = [point.0.min(anchor.0),
                                 point.1.min(anchor.1),
                                 point.0.max(anchor.0),
                                 point.1.max(anchor.1)];
                    let packed =
                        edges.into_iter()
                             .enumerate()
                             .fold(0u64, |p, (i, edge)| p | (u64::from(edge as u16) << (16 * i)));
                    inputs.push((row[1], packed));
                }
                5 if row[4] == u64::MAX - 1 && row[7] == 1 =>
                {
                    let index =
                        (published..inputs.len()).rev()
                                                 .find(|&i| inputs[i].1 == row[6])
                                                 .expect("content outline did not correspond to \
                                                          decoded physical input");
                    pending = Some(index);
                }
                5 if row[4] == u64::MAX =>
                {
                    if let Some(index) = pending.take()
                    {
                        latency.push((row[1] - inputs[index].0) * 100);
                        published = index + 1;
                    }
                    assert!(row[5] <= 12 * (display::width() + display::height()) as u64,
                            "marquee exceeded its perimeter budget");
                }
                7 => render_count += 1,
                4 => panic!("marquee generated a synthetic window update"),
                15 if row[4] & 0xffff == 17 => icon_slow.push(row[5] * 100),
                _ => ()
            }
        }
        assert_eq!(inputs.len(), 200);
        assert!(latency.len() >= 100, "physical marquee stopped making sustained progress");
        assert_eq!(published, inputs.len(), "final decoded marquee input was not painted");
        for key in ["menu_paints", "updates"]
        {
            assert_eq!(stage["state_before"][key], stage["state_after"][key],
                       "marquee changed {key}");
        }
        for services in trace["services"].as_object().unwrap().values()
        {
            assert!(services.get("file").is_none(), "marquee enumerated storage");
        }
        if empty
        {
            assert_eq!(render_count, 0, "empty marquee replayed the scene");
            assert_eq!(fs::read(output.join(stage["back_before"].as_str().unwrap())).unwrap(),
                       fs::read(output.join(stage["back_after"].as_str().unwrap())).unwrap(),
                       "outline-only motion modified the retained backbuffer");
        }
        else
        {
            assert!(render_count > 0 && render_count < 100,
                    "selection repaint was missing or unbounded");
        }
        latency.sort_unstable();
        let p = |n: usize| latency[(latency.len() * n).div_ceil(100) - 1];
        profiles.push(serde_json::json!({"name":stage["name"],"decoded":inputs.len(),
            "published":latency.len(),"coalesced":inputs.len()-latency.len(),
            "p50_ns":p(50),"p99_ns":p(99),"max_ns":latency.last(),
            "renders":render_count,"icon_calls_at_least_500us":icon_slow,
            "limits":"Physical decode to matching outline XOR, includes first move. Excludes pre-decode host latency and scanout."}));
        if std::env::var_os("SYSTEM7_STRICT_EVENT_LATENCY").is_some()
        {
            assert!(latency.iter().all(|&ns| ns < 1_000_000),
                    "physical marquee exceeded 1ms: {profiles:?}");
        }
    }
    fs::write(output.join("marquee-profile.json"), serde_json::to_vec_pretty(&profiles).unwrap()).unwrap();
}
