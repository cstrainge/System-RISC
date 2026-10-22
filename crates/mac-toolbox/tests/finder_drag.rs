//! Real physical Finder drags and persisted placement through the public SDK.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/process_trace.rs"]
mod process_trace;
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
    let result = Command::new("python3").arg(root.join("System7/scripts/capture_finder_drag.py"))
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
    for scenario in ["unsupported-copy-0", "unsupported-copy-1", "failed-drop-0", "failed-drop-1"]
    {
        for selected in ["panic", "cursor", "quickdraw"]
        {
            let frame = fs::read(output.join(format!("{scenario}-{selected}.ppm"))).unwrap();
            let pixels = frame.strip_prefix(b"P6\n640 480\n255\n").unwrap();
            for (item, x) in [("panic", 96), ("cursor", 31), ("quickdraw", 146)]
            {
                let offset = (122 * 640 + x) * 3;
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
            let pixels = frame.strip_prefix(b"P6\n640 480\n255\n").unwrap();
            for (item, x, y) in [("panic", 96, 122), ("cursor", 71, 162), ("quickdraw", 222, 216)]
            {
                let offset = ((y + dy) * 640 + x + dx) * 3;
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
                    "ResEdit" =>
                    {
                        Point { h: 192,
                                v: 0 }
                    }
                    "TextEdit Exerciser" =>
                    {
                        Point { h: 256,
                                v: 0 }
                    }
                    _ => panic!("unexpected installed app {name}")
                };
                assert_eq!(moved.saved_icon_location(), Some(point));
                let mut expected = original;
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
    assert_eq!(serial.matches("FINDER_DRAG_MOVE_COMMITTED\n").count(), 2, "{serial}");
    assert_eq!(serial.matches("FINDER_DRAG_ENDED moved\n").count(), 2, "{serial}");
    assert_eq!(serial.matches("FINDER_ACTION_FAILED status=-11").count(), 2, "{serial}");
    assert_eq!(serial.matches("FINDER_ACTION_FAILED ").count(), 2, "{serial}");
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
