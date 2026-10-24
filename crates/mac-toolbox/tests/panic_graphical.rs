//! Deliberate app panics, original bomb pixels, complete reports and recovery.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::{Path, PathBuf},
          process::Command};

use serde_json::Value;
#[path = "support/process_trace.rs"]
mod process_trace;

fn frame(path: &Path) -> Vec<u8>
{
    let bytes = fs::read(path).unwrap();
    let header = b"P6\n640 480\n255\n";
    assert!(bytes.starts_with(header));
    assert_eq!(bytes.len(), header.len() + 640 * 480 * 3);
    bytes[header.len()..].to_vec()
}

fn region(bytes: &[u8], left: usize, top: usize, right: usize, bottom: usize) -> Vec<u8>
{
    (top..bottom).flat_map(|y| bytes[(y * 640 + left) * 3..(y * 640 + right) * 3].iter().copied())
                 .collect()
}

#[test]
#[ignore = "requires current OS image and private graphical QEMU"]
fn application_panics_show_complete_diagnostics_and_leave_the_desktop_usable()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let windows = std::env::var_os("SYSTEM7_PANIC_WINDOWS").is_some();
    let output = root.join(format!("results/system7/panics-{}-{}",
                                   if windows { "windows" } else { "linux" },
                                   std::process::id()));
    let mut command = Command::new("python3");
    command.arg(root.join("System7/scripts/capture_system7_panics.py"))
           .arg("--output")
           .arg(&output)
           .current_dir(&root);
    if windows
    {
        command.arg("--windows-headless");
    }
    let transport = command.output().unwrap();
    assert!(transport.status.success(),
            "panic capture failed:\n{}\n{}",
            String::from_utf8_lossy(&transport.stdout),
            String::from_utf8_lossy(&transport.stderr));
    let capture: Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["original_before"], capture["original_after"]);
    assert_eq!(capture["owned_vm_exited"], true);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    for failure in ["SYSTEM_PANIC:", "SYSTEM_TRAP:", "SYSTEM_ALLOCATION_FAILED:",
                    "PANIC_TEST_ERROR", "TEXT_EDIT_ERROR", "PROCESS_ACTION_FAILED"]
    {
        assert!(!serial.contains(failure), "unexpected {failure}:\n{serial}");
    }
    for expected in ["PANIC_TEST_TRIGGER kind=short\n", "PANIC_TEST_TRIGGER kind=long\n",
                     "value=42", "formatted diagnostic", "café λ", "apps/panic-test/src/main.rs",
                     "END_OF_LONG_PANIC"]
    {
        assert!(serial.contains(expected), "missing {expected:?}; {}", output.display());
    }
    let reports = capture["reports"].as_array().unwrap();
    assert_eq!(reports.len(), 2);
    let short_app = reports[0]["app"].as_u64().unwrap();
    let long_app = reports[1]["app"].as_u64().unwrap();
    assert_ne!(short_app, long_app);
    // Process IDs are opaque, boot-unique incarnations, not packed slot IDs.
    // Verify completed retirement before relaunch from actual lifecycle records.
    let retired = serial.find(&format!("PROCESS_EXIT app={short_app}\n")).unwrap();
    let relaunched = serial.find(&format!("PROCESS_LAUNCHED app={long_app} ")).unwrap();
    assert!(retired < relaunched, "panic application relaunched before prior retirement");
    assert!(serial.contains(&format!("CRASH_REPORT_COPIED app={long_app} ")));
    for id in [short_app, long_app]
    {
        assert!(serial.contains(&format!("CRASH_REPORT_DISMISSED app={id}\n")));
        assert!(serial.contains(&format!("PROCESS_EXIT app={id}\n")));
    }
    let editor = process_trace::launched(&serial, "Text Edit").0;
    assert!(serial.contains(&format!("PROCESS_EXIT app={editor}\n")));
    for report in reports
    {
        assert_eq!(report["flags"], 0, "report was truncated or formatting failed");
    }
    let long_bytes = reports[1]["bytes"].as_u64().unwrap();
    let fixture = fs::read(root.join("System7/apps/panic-test/assets/long-panic.txt")).unwrap();
    assert!(fixture.len() > 20 * 1024);
    assert!(long_bytes > fixture.len() as u64, "panic context or message was truncated");
    assert!(reports[1]["lines"].as_u64().unwrap() > reports[1]["page_lines"].as_u64().unwrap());
    assert!(serial.contains(&format!("FINDER_TEXTEDIT_STATE length={long_bytes} \
                                      selection={long_bytes}..{long_bytes}\n")),
            "Copy Report did not preserve every UTF-8 byte for a separate application");
    let snapshots = capture["snapshots"].as_array().unwrap();
    let image = |name: &str| {
        let entry = snapshots.iter().find(|entry| entry["name"] == name).unwrap();
        frame(&output.join(entry["frame"].as_str().unwrap()))
    };
    for name in ["panic-button-held", "panic-button-left", "panic-button-reentered",
                 "panic-button-cancelled"]
    {
        let entry = snapshots.iter().find(|entry| entry["name"] == name).unwrap();
        let prefix = &serial[..entry["serial_length"].as_u64().unwrap() as usize];
        assert!(!prefix.contains("PANIC_TEST_TRIGGER"),
                "{name}: action fired before accepted release");
    }
    for name in ["panic-button-held", "panic-button-reentered"]
    {
        assert!(region(&image(name), 124, 244, 136, 256).iter().all(|&byte| byte == 0),
                "{name}: standard button did not highlight");
    }
    for name in ["panic-button-left", "panic-button-cancelled"]
    {
        assert_eq!(region(&image(name), 110, 235, 235, 265),
                   region(&image("panic-app-ready"), 110, 235, 235, 265),
                   "{name}: button did not restore its normal CDEF pixels");
    }
    assert_eq!(serial.matches("PANIC_TEST_TRIGGER kind=short\n").count(), 1);
    let bomb = fs::read(root.join("System7/system/assets/crashes/classic-bomb.icon")).unwrap();
    assert_eq!(bomb.len(), 128);
    let original_bomb: Vec<u8> =
        (0..32).flat_map(|y| {
                   let bomb = &bomb;
                   (0..32).flat_map(move |x| {
                              let color = if bomb[y * 4 + x / 8] & (0x80 >> (x % 8)) != 0
                              {
                                  0
                              }
                              else
                              {
                                  255
                              };
                              [color; 3]
                          })
               })
               .collect();
    for name in ["short-report", "long-first-page", "long-last-page"]
    {
        assert_eq!(region(&image(name), 48, 63, 80, 95),
                   original_bomb,
                   "{name}: bomb differs from the original System DSAT bitmap");
    }
    let first = region(&image("long-first-page"), 44, 140, 596, 396);
    assert_ne!(first,
               region(&image("long-last-page"), 44, 140, 596, 396),
               "End did not reveal later diagnostic lines");
    assert_eq!(first,
               region(&image("long-first-page-again"), 44, 140, 596, 396),
               "Home did not restore the complete original first page");
    let acceptance = serde_json::json!({"status":"passed", "original_bomb_pixels":3 * 32 * 32,
        "complete_report_bytes":long_bytes, "cases":["formatted location and values", "Unicode",
        "long report", "Home and End scrolling", "lossless cross-app clipboard", "app-only exit",
        "normal application after panic", "button press leave reenter cancel and release"], "capture":capture, "output":output});
    let bytes = serde_json::to_vec_pretty(&acceptance).unwrap();
    fs::write(output.join("rust-acceptance.json"), &bytes).unwrap();
    fs::write(root.join("results/system7/panic-checks.json"), bytes).unwrap();
}
