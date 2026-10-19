//! Public-SDK frame atomicity while two actual native app threads poll events.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};

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
    assert!(serial.contains("RETAINED_UPDATE_USERSPACE_PASS separate_threads=2 stages=7\n"),
            "{serial}");
    let stages = report["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 7);
    let header = b"P6\n640 480\n255\n";
    let mut checked = 0;
    for stage in stages
    {
        let name = stage["stage"].as_str().unwrap();
        let frame = fs::read(output.join(stage["frame"].as_str().unwrap())).unwrap();
        assert!(frame.starts_with(header));
        assert_eq!(frame.len(), header.len() + 640 * 480 * 3);
        // Exclude the standard definition's reserved scrollbar/grow strips.
        for y in 360..430
        {
            for x in 360..610
            {
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
    }
    let acceptance = serde_json::json!({"status":"passed","stages":7,"native_app_threads":2,
        "pixels":checked,"pending_frames_retain_last_commit":true,
        "end_update_publishes_complete_scene":true,"capture":report});
    fs::write(output.join("rust-acceptance.json"),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
    fs::write(root.join("results/system7/retained-update-checks.json"),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
}
