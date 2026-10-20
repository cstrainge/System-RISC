//! Actual Finder -> facade -> ECALL -> resource managers -> writable virtio HFS.
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
fn crop(frame: &[u8], left: usize, top: usize, width: usize, height: usize) -> Vec<u8>
{
    let mut result = Vec::new();
    for y in top..top + height
    {
        result.extend_from_slice(&frame[(y * 640 + left) * 3..(y * 640 + left + width) * 3]);
    }
    result
}
#[test]
#[ignore = "requires cargo xtask image and real QEMU graphical guest"]
fn finder_menus_and_trash_are_resource_based_and_persist_across_disk_boots()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join(format!("results/system7/finder-trash-{}", std::process::id()));
    let capture = Command::new("python3").arg(root.join("System7/scripts/capture_finder_trash.py"))
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
    assert_eq!(report["base_before"], report["base_after"], "generated baseline disk changed");
    let observations = report["observations"].as_array().unwrap();
    let observation = |name: &str| observations.iter().find(|v| v["name"] == name).unwrap();
    let log = |name: &str| observation(name)["log"].as_str().unwrap();
    let image = |name: &str| frame(&output.join(observation(name)["frame"].as_str().unwrap()));
    assert!(log("folder-created-menu").contains("FINDER_MENU menu=1252 item=1 command=6e657720"));
    assert_eq!(log("before-reboot").matches("FINDER_NEW_FOLDER ").count(),
               1,
               "modal Ctrl-N escaped to Finder");
    let (volume, _) =
        mac_system::testing::with_file_disk(&root.join("build/system7/dist/system7.hfsb"),
                                            true,
                                            Default::default(),
                                            |services| {
                                                let system =
                                                    unsafe {
                                                        mac_toolbox::System::from_raw(services)
                                                    }.unwrap();
                                                system.files().volume_at(0).unwrap().id
                                            }).unwrap();
    assert!(log("trashed").contains(&format!("FINDER_TRASHED volume={volume} ")),
            "drag did not move item to Trash");
    assert!(log("put-away").contains(&format!("FINDER_PUT_AWAY volume={volume} ")));
    assert_eq!(log("trashed-again").matches("FINDER_TRASHED ").count(),
               2,
               "Ctrl-Backspace did not use real Trash");
    let prompt = "FINDER_EMPTY_TEXT The Trash contains 1 item.  It uses 0K of disk space.  Are \
                  you sure you want to permanently remove it?";
    assert!(log("empty-confirm").contains(prompt),
            "original resource template/count/allocated-space formatting differs");
    assert_eq!(observation("empty-confirm")["state"]["front"],
               observation("modal-blocked")["state"]["front"]);
    assert_eq!(observation("empty-confirm")["state"]["windows"],
               observation("modal-blocked")["state"]["windows"]);
    assert!(!log("empty-cancelled").contains("FINDER_TRASH_EMPTIED"));
    assert!(log("empty-cancelled").contains("FINDER_ALERT_DISMISSED"));
    assert_eq!(image("desktop-focused-before"),
               image("desktop-focus-restored"),
               "modal dismissal stole desktop focus/selection");
    assert_eq!(crop(&image("trashed"), 576, 420, 32, 32),
               crop(&image("rebooted-full-trash"), 576, 420, 32, 32),
               "Trash contents/icon did not survive cold reboot");
    assert!(log("reboot-confirm").contains(prompt));
    assert!(log("emptied").contains("FINDER_EMPTY_CONFIRMED"));
    assert!(log("emptied").contains("FINDER_TRASH_EMPTIED removed=1"));
    assert_eq!(crop(&image("baseline"), 576, 420, 32, 32),
               crop(&image("rebooted-empty-trash"), 576, 420, 32, 32));
    assert!(image("ok-held") != image("ok-left"), "default button did not unhighlight outside");
    assert_eq!(image("ok-held"), image("ok-reentered"), "default button reentry differs");
    for (name, color) in
        [("ok-held", [0, 0, 0]), ("ok-left", [255, 255, 255]), ("ok-reentered", [0, 0, 0])]
    {
        let frame = image(name);
        let at = (173 * 640 + 440) * 3;
        assert_eq!(&frame[at..at + 3],
                   &color,
                   "{name}: actual CDEF highlight must follow pointer before release");
    }
    let original = include_bytes!("data/original-alert-6012.rgb");
    let alert = crop(&image("empty-confirm"), 128, 89, 384, 114);
    let mut differences = Vec::new();
    let mut checked = 0;
    for y in 0..114
    {
        for x in 0..384
        {
            // Original file used8K; this empty folder uses0K. Exclude only
            // that digit's glyph; retain all other body, WDEF/CDEF/icon pixels.
            if (312..320).contains(&x) && (18..30).contains(&y)
            {
                continue;
            }
            checked += 1;
            let at = (y * 384 + x) * 3;
            if alert[at..at + 3] != original[at..at + 3]
            {
                differences.push((x + 128, y + 89));
            }
        }
    }
    assert!(differences.is_empty(),
            "original alert chrome differs: {} pixels, first {:?}",
            differences.len(),
            &differences[..differences.len().min(24)]);
    for boot in ["first-boot.log", "second-boot.log", "third-boot.log"]
    {
        let serial = fs::read_to_string(output.join(boot)).unwrap();
        assert!(!serial.contains("FINDER_ACTION_FAILED"), "{boot}: {serial}");
        assert!(!serial.contains("DISK_REQUEST_FAILED")
                && !serial.contains("DISK_WRITE_FAILED")
                && !serial.contains("DISK_FLUSH_FAILED")
                && !serial.contains("DISK_READ_FAILED"),
                "{boot}: storage failed during Trash transaction: {serial}");
        assert!(!serial.contains("PANIC"));
    }
    mac_system::testing::with_file_disk(Path::new(report["private_disk"].as_str().unwrap()),true,Default::default(),|services|{
        let system=unsafe{mac_toolbox::System::from_raw(services)}.unwrap();
        let trash=system.files().trash_directory(system.files().volume_at(0).unwrap().id,false).unwrap();assert_eq!(trash.child_count,0);
        assert_eq!(system.files().lookup(system.files().volume_at(0).unwrap().id,2,"untitled folder"),Err(mac_toolbox::Error(mac_toolbox::NOT_FOUND)));
        assert!(system.files().lookup(system.files().volume_at(0).unwrap().id,2,"System Folder/System").is_ok());
        assert!(system.files().lookup(system.files().volume_at(0).unwrap().id,2,"System Folder/Finder").is_ok());
    }).unwrap();
    let accepted = serde_json::json!({"status":"passed","boots":3,"alert_original_pixels":checked,"pixel_differences":0,"scope":"real Finder menu/drag/shortcut/PutAway/modal confirmation and persistent HFS Trash","capture":report});
    let bytes = serde_json::to_vec_pretty(&accepted).unwrap();
    fs::write(output.join("rust-acceptance.json"), &bytes).unwrap();
    fs::write(root.join("results/system7/finder-trash-checks.json"), bytes).unwrap();
}
