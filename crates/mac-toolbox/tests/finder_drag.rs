//! Real physical Finder drags and persisted placement through the public SDK.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/process_trace.rs"]
mod process_trace;
#[path = "support/display.rs"]
mod display;
use std::{collections::BTreeMap,
          fs,
          path::{Path, PathBuf},
          process::Command};

use mac_toolbox::{Point, System,
                  files::{CatalogInfo, ROOT_ID}};

struct Media(PathBuf);
impl Drop for Media
{
    fn drop(&mut self)
    {
        for name in ["before.raw", "after.raw"]
        {
            let _ = fs::remove_file(self.0.join(name));
        }
    }
}
#[test]
#[ignore = "requires current private image, QEMU and real host exports"]
fn finder_transfers_external_applications_and_documents_with_drop_time_policy()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("filesystem-workflows-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/\
                                                        capture_filesystem_workflows.py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .output()
                                        .unwrap();
    assert!(result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    assert_eq!(fs::read(output.join("native-cursor.rsrc")).unwrap(),
               fs::read(output.join("original-cursor.rsrc")).unwrap(),
               "both-fork application copy/move changed resources");
    assert_eq!(fs::read(output.join("exported/children/Note.txt copy/data")).unwrap(),
               b"External document\n");
    assert_eq!(fs::read(output.join("exported/children/Trash/children/Note.txt/data")).unwrap(),
               b"External document\n");
    assert!(!output.join("Writable/Note.txt").exists());
    assert!(!output.join("Writable/Cursor Test").exists());
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert_eq!(serial.matches("FINDER_DRAG_TRANSFER_COMMITTED\n").count(), 5, "{serial}");
    assert_eq!(serial.matches("FINDER_DRAG_ENDED copied\n").count(), 3, "{serial}");
    assert_eq!(serial.matches("FINDER_DRAG_ENDED moved\n").count(), 4, "{serial}");
    assert_eq!(serial.matches("FINDER_DRAG_ENDED rejected\n").count(), 3, "{serial}");
    assert_eq!(serial.matches("FINDER_DRAG_VOLUME_UNMOUNTED\n").count(), 2, "{serial}");
    assert_eq!(serial.matches("TEXT_EDIT_DOCUMENT_OPEN\n").count(), 1, "{serial}");
    assert!(serial.contains("CURSOR_TEST_READY\n"), "{serial}");
    for failure in ["SYSTEM_PANIC:", "SYSTEM_TRAP:", "SYSTEM_ALLOCATION_FAILED:",
                    "FINDER_ACTION_FAILED", "FINDER_ERROR", "TEXT_EDIT_ERROR"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
}
fn catalog(path: &Path) -> BTreeMap<String, CatalogInfo>
{
    mac_system::testing::with_file_disk(path, true, Default::default(), |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let files = system.files();
        let volume = files.volume_at(0).unwrap().id;
        let apps = files.lookup(volume, ROOT_ID, "Applications").unwrap();
        (0..apps.child_count.unwrap()).map(|index| {
                                          let item = files.child(volume, apps.id, index).unwrap();
                                          (item.name().to_owned(), item)
                                      })
                                      .collect()
    }).unwrap()
      .0
}
#[test]
#[ignore = "requires current event-trace private image and Linux QEMU"]
fn failed_drop_alerts_release_sessions_and_successful_group_drags_persist()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join("results/system7").join(format!("finder-drag-{}", std::process::id()));
    let _media = Media(output.clone());
    let mut command = Command::new("python3");
    command.arg(root.join("System7/scripts/capture_finder_drag.py")).arg("--output").arg(&output);
    if std::env::var_os("SYSTEM7_FINDER_DRAG_WINDOWS").is_some()
    {
        command.arg("--windows-headless");
    }
    let result = command.output().unwrap();
    assert!(result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    for scenario in ["incompatible-app-0", "incompatible-app-1", "failed-drop-0", "failed-drop-1"]
    {
        for selected in ["panic", "cursor", "quickdraw"]
        {
            let frame = fs::read(output.join(format!("{scenario}-{selected}.ppm"))).unwrap();
            let pixels = frame.strip_prefix(display::header().as_slice()).unwrap();
            for (item, x) in [("panic", 96), ("cursor", 31), ("quickdraw", 146)]
            {
                let offset = (122 * display::width() + x) * 3;
                let expected = if item == selected { [0; 3] } else { [255; 3] };
                assert_eq!(&pixels[offset..offset + 3],
                           &expected,
                           "selection did not change after {scenario}: selected {selected}, \
                            checking {item}");
            }
        }
    }
    for (scenario, dx, dy) in [("cancelled-group", 0, 0),
                               ("outside-rejected", 0, 0),
                               ("moved-window", 40, 40),
                               ("cancelled-window-drag", 40, 40)]
    {
        for selected in ["cursor", "panic", "quickdraw"]
        {
            let frame = fs::read(output.join(format!("{scenario}-{selected}.ppm"))).unwrap();
            let pixels = frame.strip_prefix(display::header().as_slice()).unwrap();
            for (item, x, y) in [("panic", 96, 122), ("cursor", 71, 162), ("quickdraw", 222, 216)]
            {
                let offset = ((y + dy) * display::width() + x + dx) * 3;
                let expected = if item == selected { [0; 3] } else { [255; 3] };
                assert_eq!(&pixels[offset..offset + 3],
                           &expected,
                           "selection did not change after {scenario}: selected {selected}, \
                            checking {item}");
            }
        }
    }
    assert!(report["moved_window_updates_before"].as_u64().unwrap() > 0,
            "missing folder update instrumentation");
    assert_eq!(report["moved_window_updates_before"], report["moved_window_updates_after"],
               "moving retained window content forced a full folder repaint");
    for key in ["compositions", "menu_paints", "updates", "scene_rev"]
    {
        assert_eq!(report["tracking_before"][key], report["tracking_after"][key],
                   "tracking changed {key}");
    }
    let before = fs::read(output.join(report["tracking_back_before"].as_str().unwrap())).unwrap();
    assert_eq!(before.len(), display::width() * display::height() * 4);
    assert_eq!(before,
               fs::read(output.join(report["tracking_back_after"].as_str().unwrap())).unwrap(),
               "file drag feedback modified the retained scene");
    assert_eq!(report["tracking_trace"]["overwritten"], 0, "drag trace records were lost");
    let rows = report["tracking_trace"]["records"].as_array().unwrap();
    let mut pending = None;
    let mut latency = Vec::new();
    let mut coalesced = 0;
    let mut decoded = 0;
    let mut xors = 0;
    let mut xor_ns = Vec::new();
    for row in rows
    {
        assert_ne!(row[2], 7, "file drag replayed a retained scene: {row}");
        assert_ne!(row[2], 4, "held file drag generated update work: {row}");
        if row[2] == 1 && row[4] == 1
        {
            decoded += 1;
            if pending.is_some()
            {
                coalesced += 1;
            }
            pending = Some((row[1].as_u64().unwrap(),
                            row[5].as_u64().unwrap(),
                            row[6].as_u64().unwrap(),
                            false));
        }
        else if row[2] == 5 && row[4].as_u64() == Some(u64::MAX)
        {
            xors += 1;
            assert!(row[5].as_u64().unwrap()
                    <= 12 * (display::width() + display::height()) as u64 as u64,
                    "file drag exceeded its perimeter budget: {row}");
            xor_ns.push(row[6].as_u64().unwrap() * 100);
            if let Some(pending) = &mut pending
            {
                pending.3 = true;
            }
        }
        else if row[2] == 14
        {
            assert!(row[5].as_u64().unwrap() <= 256, "drag copied more than cursor pixels: {row}");
            if let Some((tick, x, y, xored)) = pending
            {
                let from = row[6].as_u64().unwrap();
                let to = row[7].as_u64().unwrap();
                if xored
                   && from >> 32 <= x
                   && from & 0xffff_ffff <= y
                   && to >> 32 > x
                   && to & 0xffff_ffff > y
                {
                    latency.push((row[1].as_u64().unwrap() - tick) * 100);
                    pending = None;
                }
            }
        }
    }
    assert!(pending.is_none(), "last file drag input never published its feedback");
    assert!(xors >= 100 && latency.len() >= 100, "file outline did not make sustained progress");
    assert_eq!(latency.len() + coalesced, decoded);
    let distribution = |values: &[u64]| {
        let mut v = values.to_vec();
        v.sort_unstable();
        let p = |n: usize| v[(v.len() * n).div_ceil(100) - 1];
        serde_json::json!({"samples":v.len(),"p50_ns":p(50),"p95_ns":p(95),"p99_ns":p(99),"max_ns":v.last()})
    };
    let profile = serde_json::json!({"decoded":decoded,"coalesced":coalesced,"xor_updates":xors,
        "decode_to_feedback_and_cursor":distribution(&latency),"xor":distribution(&xor_ns),
        "limits":"Includes first decoded move; coalescing is counted. Excludes pre-decode host/polling delay and scanout. Paused snapshots are outside the stream.",
        "strict":std::env::var_os("SYSTEM7_STRICT_EVENT_LATENCY").is_some()});
    fs::write(output.join("drag-feedback-profile.json"),
              serde_json::to_vec_pretty(&profile).unwrap()).unwrap();
    if std::env::var_os("SYSTEM7_STRICT_EVENT_LATENCY").is_some()
    {
        assert!(latency.iter().all(|&ns| ns < 1_000_000),
                "strict file-drag latency exceeded 1ms: {profile}");
    }
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    let finder = process_trace::finder(&serial).to_string();
    for service in ["file", "font"]
    {
        assert!(report["tracking_trace"]["services"][&finder][service].is_null(),
                "pointer motion called {service}: {}",
                report["tracking_trace"]);
    }
    let before = catalog(&output.join("before.raw"));
    let after = catalog(&output.join("after.raw"));
    assert_eq!(before.len(), after.len());
    for (name, original) in before
    {
        let moved = after.get(&name).unwrap();
        assert_eq!((moved.id,
                    moved.parent,
                    moved.data_length,
                    moved.resource_length,
                    moved.created,
                    moved.modified,
                    moved.backup),
                   (original.id,
                    original.parent,
                    original.data_length,
                    original.resource_length,
                    original.created,
                    original.modified,
                    original.backup),
                   "{name}");
        match name.as_str()
        {
            "QuickDraw Test" =>
            {
                assert_eq!(moved.saved_icon_location(),
                           Some(Point { h: 204,
                                        v: 94 }))
            }
            "Cursor Test" =>
            {
                assert_eq!(moved.saved_icon_location(),
                           Some(Point { h: 40,
                                        v: 40 }))
            }
            _ =>
            {
                if name == "Z Conflict"
                {
                    assert!(moved.saved_icon_location().is_some());
                    let mut expected = original;
                    expected.volume = moved.volume; // A remount issues a fresh capability.
                    expected.finder_info = moved.finder_info;
                    assert_eq!(*moved, expected, "collision fixture changed beyond placement");
                    continue;
                }
                let point = match name.as_str()
                {
                    "Panic Test" =>
                    {
                        Point { h: 64,
                                v: 0 }
                    }
                    "Res Edit" =>
                    {
                        Point { h: 320,
                                v: 0 }
                    }
                    "Text Edit" =>
                    {
                        Point { h: 0,
                                v: 52 }
                    }
                    "Reactor Room" =>
                    {
                        Point { h: 256,
                                v: 0 }
                    }
                    _ => panic!("unexpected installed app {name}")
                };
                assert_eq!(moved.saved_icon_location(), Some(point));
                let mut expected = original;
                expected.volume = moved.volume; // Compare persisted metadata, not mount IDs.
                expected.finder_info[8..10].copy_from_slice(&(original.finder_flags().bits()
                                                              | 0x100)
                                                                      .to_be_bytes());
                expected.finder_info[10..12].copy_from_slice(&point.v.to_be_bytes());
                expected.finder_info[12..14].copy_from_slice(&point.h.to_be_bytes());
                assert_eq!(*moved, expected,
                           "unrelated item changed beyond initial placement: {name}");
            }
        }
    }
    let serial = fs::read_to_string(output.join("boot-1.log")).unwrap();
    assert_eq!(serial.matches("FINDER_DRAG_TRANSFER_COMMITTED\n").count(), 2, "{serial}");
    assert_eq!(serial.matches("FINDER_DRAG_ENDED moved\n").count(), 2, "{serial}");
    assert_eq!(serial.matches("AlreadyExists (-11)").count(), 2, "{serial}");
    assert_eq!(serial.matches("Could not transfer \"QuickDraw Test\": an item with that name \
                               already exists")
                     .count(),
               2,
               "{serial}");
    assert_eq!(serial.matches("FINDER_DRAG_ENDED rejected\n").count(), 5, "{serial}");
    assert_eq!(serial.matches("FINDER_ALERT_DISMISSED\n").count(), 2, "{serial}");
    for failure in ["SYSTEM_PANIC:", "SYSTEM_TRAP:"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
    // After cancellation the exact saved icon and caption pixels remain.
    let a = fs::read(output.join("group-moved.ppm")).unwrap();
    let b = fs::read(output.join("cancelled.ppm")).unwrap();
    assert_eq!(a, b, "Escape left feedback or changed selection/content");
}
