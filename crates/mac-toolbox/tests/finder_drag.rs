//! Real physical Finder drags and persisted placement through the public SDK.
#![cfg(not(target_arch = "riscv64"))]
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
fn same_folder_single_and_group_drags_persist_and_escape_does_not_move_items()
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
    for key in ["compositions", "menu_paints", "updates", "scene_rev"]
    {
        assert_eq!(report["tracking_before"][key], report["tracking_after"][key],
                   "tracking changed {key}");
    }
    for service in ["file", "font"]
    {
        assert!(report["tracking_trace"]["services"]["1"][service].is_null(),
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
            _ => assert_eq!(*moved, original, "unrelated item changed: {name}")
        }
    }
    let serial = fs::read_to_string(output.join("boot-1.log")).unwrap();
    assert_eq!(serial.matches("FINDER_DRAG_MOVE_COMMITTED\n").count(), 2, "{serial}");
    assert_eq!(serial.matches("FINDER_DRAG_ENDED moved\n").count(), 2, "{serial}");
    for failure in ["FINDER_ACTION_FAILED", "SYSTEM_PANIC:", "SYSTEM_TRAP:"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
    // After cancellation the exact saved icon and caption pixels remain.
    let a = fs::read(output.join("group-moved.ppm")).unwrap();
    let b = fs::read(output.join("cancelled.ppm")).unwrap();
    assert_eq!(a, b, "Escape left feedback or changed selection/content");
}
