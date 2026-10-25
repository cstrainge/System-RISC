//! Real managed Alert controls: bounded pixels, no-change work and cancellation.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/display.rs"]
mod display;
use std::{fs, path::PathBuf, process::Command};

#[test]
#[ignore = "requires optimized event-trace image and QEMU"]
fn alert_tracking_repaints_only_its_control_and_early_outs_when_unchanged()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join(format!("results/system7/alert-tracking-{}", std::process::id()));
    let capture = Command::new("python3").arg(root.join("System7/scripts/capture_alert_tracking.\
                                                         py"))
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
    let stages = report["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 6);
    let frame = |name: &str| {
        let bytes = fs::read(output.join(format!("{name}.ppm"))).unwrap();
        let header = display::header();
        assert!(bytes.starts_with(&header));
        assert_eq!(bytes.len(), header.len() + display::width() * display::height() * 3);
        bytes[header.len()..].to_vec()
    };
    let base = frame("base");
    let pressed = frame("pressed");
    assert_ne!(base, pressed, "button did not highlight");
    assert_eq!(frame("held"), pressed, "unchanged held pointer changed pixels");
    assert_eq!(frame("outside"), base, "drag-out did not unhighlight");
    assert_eq!(frame("released"), base, "cancelled release changed pixels");
    assert_eq!(frame("selected"), base, "selected button did not restore its pixels");
    let bounds: Vec<i64> =
        stages[0]["bounds"].as_array().unwrap().iter().map(|n| n.as_i64().unwrap()).collect();
    let contains =
        |x: i64, y: i64| bounds[0] <= x && x < bounds[2] && bounds[1] <= y && y < bounds[3];
    for y in 0..display::height()
    {
        for x in 0..display::width()
        {
            let at = (y * display::width() + x) * 3;
            if base[at..at + 3] != pressed[at..at + 3]
            {
                assert!(contains(x as i64, y as i64), "button repainted unrelated pixel {x},{y}");
            }
        }
    }
    let first_updates = &stages[0]["state"]["updates"];
    for stage in &stages[1..]
    {
        let name = stage["name"].as_str().unwrap();
        assert_eq!(&stage["state"]["updates"], first_updates, "{name} entered full-window drawing");
        assert_eq!(stage["delta"]["overwritten"], 0, "{name}: trace lost evidence");
        let mouse: Vec<i64> = stage["state"]["mouse"].as_str()
                                                     .unwrap()
                                                     .split(',')
                                                     .map(|n| n.parse().unwrap())
                                                     .collect();
        for row in stage["delta"]["records"].as_array().unwrap().iter().filter(|r| r[2] == 14)
        {
            let first = row[6].as_u64().unwrap();
            let last = row[7].as_u64().unwrap();
            let (l, t, r, b) = ((first >> 32) as i64,
                                (first & 0xffffffff) as i64,
                                (last >> 32) as i64,
                                (last & 0xffffffff) as i64);
            let control = bounds[0] <= l && bounds[1] <= t && r <= bounds[2] && b <= bounds[3];
            // The independent cursor overlay may also republish its own box.
            let cursor = mouse[0] - 16 <= l
                         && mouse[1] - 16 <= t
                         && r <= mouse[0] + 16
                         && b <= mouse[1] + 16;
            assert!(control || cursor, "{name}: unrelated framebuffer copy {l},{t}..{r},{b}");
        }
    }
    for (idle, previous) in [("held", "pressed"), ("released", "outside")]
    {
        let stage = stages.iter().find(|s| s["name"] == idle).unwrap();
        let prior = stages.iter().find(|s| s["name"] == previous).unwrap();
        assert_eq!(stage["state"]["compositions"], prior["state"]["compositions"],
                   "{idle}: no-change recomposition");
        assert_eq!(stage["state"]["menu_paints"], prior["state"]["menu_paints"],
                   "{idle}: menu repaint");
    }
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(serial.contains("ALERT_TRACKING_USERSPACE_PASS cancel=passed release=passed \
                             cleanup=passed"));
    for bad in ["TOOLBOX_USERSPACE_FAIL", "SYSTEM_PANIC:", "SYSTEM_ALLOCATION_FAILED:"]
    {
        assert!(!serial.contains(bad), "{bad}: {serial}");
    }
}
