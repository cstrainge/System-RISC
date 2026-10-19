//! Native PNG extension, tested through Finder -> public Toolbox -> resident
//! retained drawing, real disk boot, and actual framebuffer pixels.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};
#[test]
#[ignore = "requires cargo xtask image and real QEMU graphical guest"]
fn finder_png_resource_composites_and_scales_through_the_resident_service()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join(format!("results/system7/png-finder-{}", std::process::id()));
    let capture = Command::new("python3").arg(root.join("System7/scripts/\
                                                         capture_system7_png_demo.py"))
                                         .args(["--output"])
                                         .arg(&output)
                                         .current_dir(&root)
                                         .output()
                                         .unwrap();
    assert!(capture.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["base_before"], report["base_after"]);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(serial.contains("QD_DEMO_READY page=10 png"));
    assert!(!serial.contains("FINDER_ACTION_FAILED"));
    let frame = fs::read(output.join("finder-png.ppm")).unwrap();
    let header = b"P6\n640 480\n255\n";
    assert!(frame.starts_with(header));
    let rgb = &frame[header.len()..];
    assert_eq!(rgb.len(), 640 * 480 * 3);
    let pixel = |x: usize, y: usize| -> u32 {
        let i = (y * 640 + x) * 3;
        (rgb[i] as u32) << 16 | (rgb[i + 1] as u32) << 8 | rgb[i + 2] as u32
    };
    // Independent source sample formula used to encode the bundled PNG. This
    // checks all natural, scaled, and clipped pixels without decoding it in tests.
    let expected = |x: usize, y: usize, sx: usize, sy: usize| -> u32 {
        let source = [sx * 255 / 95, sy * 255 / 63, 255 - sx * 255 / 95];
        let alpha = ((sx + sy) * 3).min(255);
        let bg = if ((x - 16) / 16 + (y - 60) / 16).is_multiple_of(2) { 0xd0 } else { 255 };
        let channel = |v| ((v * alpha + bg * (255 - alpha) + 127) / 255) as u32;
        channel(source[0]) << 16 | channel(source[1]) << 8 | channel(source[2])
    };
    let mut checked = 0;
    for (left, top, width, height, clip) in [(24, 76, 96, 64, (24, 76, 120, 140)),
                                             (160, 76, 288, 192, (160, 76, 448, 268)),
                                             (440, 76, 96, 192, (460, 100, 520, 240))]
    {
        for y in clip.1..clip.3
        {
            for x in clip.0..clip.2
            {
                let (sx, sy) = ((x - left) * 96 / width, (y - top) * 64 / height);
                assert_eq!(pixel(40 + x, 75 + y),
                           expected(x, y, sx, sy),
                           "Finder PNG local{x},{y}, source{sx},{sy}");
                checked += 1;
            }
        }
    }
    fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&serde_json::json!({"status":"passed","checked_pixels":checked,"pixel_differences":0,"transport":"Finder/resource/publicSDK/realECALL/retainedRGBframebuffer","capture":report})).unwrap()).unwrap();
    fs::write(root.join("results/system7/png-finder-checks.json"),
              fs::read(output.join("rust-acceptance.json")).unwrap()).unwrap();
}
