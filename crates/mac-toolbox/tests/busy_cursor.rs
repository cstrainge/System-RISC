//! Foreground stalls observed through real SDK applications and original sprites.
#![cfg(not(target_arch = "riscv64"))]
use std::{collections::BTreeSet, fs, path::PathBuf, process::Command};

fn matches(frame: &[u8], cursor: &[u8], point: [usize; 2], checker: bool) -> bool
{
    let header = b"P6\n640 480\n255\n";
    assert!(frame.starts_with(header));
    let pixels = &frame[header.len()..];
    let word = |at| u16::from_be_bytes([cursor[at], cursor[at + 1]]);
    for y in point[1] - 16..point[1] + 16
    {
        for x in point[0] - 16..point[0] + 16
        {
            let mut rgb =
                [if checker { if (x + y) & 1 == 0 { 0x87 } else { 0xc0 } } else { 255 }; 3];
            let column = x as isize - point[0] as isize + word(66) as isize;
            let row = y as isize - point[1] as isize + word(64) as isize;
            if (0..16).contains(&column) && (0..16).contains(&row)
            {
                let bit = 0x8000 >> column;
                let image = word(row as usize * 2) & bit != 0;
                let mask = word(32 + row as usize * 2) & bit != 0;
                if mask
                {
                    rgb.fill(if image { 0 } else { 255 });
                }
                else if image
                {
                    for channel in &mut rgb
                    {
                        *channel ^= 255;
                    }
                }
            }
            if pixels[(y * 640 + x) * 3..(y * 640 + x) * 3 + 3] != rgb
            {
                return false;
            }
        }
    }
    true
}

#[test]
#[ignore = "requires current OS image and private graphical QEMU"]
fn foreground_stalls_animate_without_app_cooperation_or_scene_repaints()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join(format!("results/system7/busy-cursor-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/\
                                                        capture_system7_busy_cursor.py"))
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
    assert_eq!(report["owned_vm_exited"], true);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(serial.contains("BUSY_CURSOR_USERSPACE_PASS\n"));
    assert!(!serial.contains("BUSY_CURSOR_FAIL"));
    let frames = report["frames"].as_array().unwrap();
    let entry = |name: &str| frames.iter().find(|f| f["name"] == name).unwrap();
    let frame = |name: &str| fs::read(output.join(entry(name)["frame"].as_str().unwrap())).unwrap();
    let sprite = |id| {
        fs::read(root.join(format!("System7/system/assets/visual/43555253-{id}.bin"))).unwrap()
    };
    let start = serial.lines()
                      .find_map(|line| line.strip_prefix("BUSY_CLOCK start="))
                      .expect("missing guest work-start counter")
                      .parse::<u64>()
                      .unwrap();
    let short = entry("short-work");
    let before = short["clock_before"].as_u64().unwrap();
    let after = short["clock_after"].as_u64().unwrap();
    assert!(start <= before && before <= after && after - start < 4_000_000,
            "short-work capture was late: start={start}, before={before}, after={after}; capture \
             must finish within400ms of guest work, before the500ms stall threshold");
    let cross = sprite(2);
    for name in ["idle", "short-work", "recovered", "peeking", "trap-recovered"]
    {
        assert!(matches(&frame(name), &cross, [350, 300], false),
                "{name}: app cursor was not preserved/restored");
    }
    let watch = [4, -6078, -6077, -6076, -6075, -6074, -6073, -6072].map(sprite);
    let mut seen = BTreeSet::new();
    let baseline = frame("idle");
    let switch_base = frame("switch-base");
    for f in frames
    {
        let name = f["name"].as_str().unwrap();
        if name.starts_with("spin-")
           || name.starts_with("traps-")
           || name.starts_with("hidden-") && name != "hidden-recovered"
           || name == "switch-busy"
        {
            let pixels = frame(name);
            let reference = if name == "switch-busy" { &switch_base } else { &baseline };
            let index = watch.iter()
                             .position(|cursor| matches(&pixels, cursor, [350, 300], false))
                             .unwrap_or_else(|| {
                                 panic!("{name}: System did not display an original animated \
                                         watch frame")
                             });
            if name.starts_with("spin-")
            {
                seen.insert(index);
                assert_eq!(f["state"]["compositions"],
                           entry("spin-0")["state"]["compositions"],
                           "watch animation recomposed the scene");
                assert_eq!(f["state"]["menu_paints"], entry("spin-0")["state"]["menu_paints"]);
            }
            let header = b"P6\n640 480\n255\n".len();
            for y in 0..480
            {
                for x in 0..640
                {
                    if (334..366).contains(&x) && (284..316).contains(&y)
                    {
                        continue;
                    }
                    let at = header + (y * 640 + x) * 3;
                    assert_eq!(&pixels[at..at + 3],
                               &reference[at..at + 3],
                               "{name}: cursor damaged unrelated pixels at {x},{y}");
                }
            }
        }
    }
    assert!(seen.len() >= 2, "CPU-bound app prevented the watch from animating");
    let blank = [0u8; 68];
    assert!(matches(&frame("hidden-recovered"), &blank, [350, 300], false),
            "hidden app cursor was made visible on recovery");
    let arrow = fs::read(root.join("System7/system/assets/cursors/arrow.curs")).unwrap();
    for name in ["other-app", "background-recovered"]
    {
        assert!(matches(&frame(name), &arrow, [16, 444], true),
                "{name}: background stall leaked into responsive foreground");
    }
    fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&serde_json::json!({"status":"passed","original_watch_frames":seen.len(),"public_sdk_visibility_preserved":true,"cpu_and_trap_bound_work":true,"sleeping_app_exempt":true,"cursor_only_damage":true,"background_stall_ignored":true})).unwrap()).unwrap();
}
