//! Real Finder behavior against ordinary host entries and a broken container.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};

fn capture(corrupt: bool) -> (PathBuf, String)
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join(format!("results/system7/transfer-incident-{}-{corrupt}", std::process::id()));
    let mut command = Command::new("python3");
    command.arg(root.join("System7/scripts/capture_transfer_incident.py"))
           .arg("--output")
           .arg(&output);
    if corrupt
    {
        command.arg("--corrupt");
    }
    let result = command.output().unwrap();
    assert!(result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    for error in ["FINDER_ERROR", "SYSTEM_PANIC:", "SYSTEM_TRAP:"]
    {
        assert!(!serial.contains(error), "{serial}");
    }
    (output, serial)
}

#[test]
#[ignore = "requires current private image, Linux QEMU and host companion"]
fn ordinary_host_entries_boot_browse_and_allow_finder_folder_creation()
{
    let (output, serial) = capture(false);
    assert!(serial.contains("DESKTOP_READY"), "{serial}");
    assert!(serial.contains("FINDER_FOLDER_OPEN ") && serial.contains("name=Transfer"), "{serial}");
    assert!(serial.contains("FINDER_NEW_FOLDER "), "{serial}");
    assert!(!serial.contains("FINDER_ACTION_FAILED"), "{serial}");
    assert!(serial.contains("FINDER_FOLDER_CHANGED "), "{serial}");
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["rapid_closed_windows"].as_array().unwrap().len(), 3);
    assert_eq!(report["state"]["windows"], "2");
    assert!(serial.contains("FINDER_CLOSE_COMMITTED"), "{serial}");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let arrow = fs::read(root.join("System7/system/assets/cursors/arrow.curs")).unwrap();
    assert_arrow(&fs::read(output.join("rapid-close.ppm")).unwrap(), &arrow, [350, 350]);
    let export = output.join("export");
    assert_eq!(fs::read(export.join("Notes.txt")).unwrap(), b"Ordinary host file\n");
    assert!(export.join("Test").is_dir());
    assert!(fs::read_dir(&export).unwrap().filter_map(Result::ok).any(|entry| entry.file_name().to_string_lossy().starts_with("d-") && entry.path().join(".system-risc-directory").is_file()));
}

#[test]
#[ignore = "requires current private image, Linux QEMU and host companion"]
fn provider_error_names_operation_subject_and_cause_and_alert_stays_responsive()
{
    let (output, serial) = capture(true);
    let alert = serial.lines().find(|line| line.starts_with("FINDER_ALERT_TEXT ")).expect(&serial);
    assert!(alert.contains("Transfer") && alert.contains("CorruptData") && alert.contains("-18"),
            "{alert}");
    assert!(serial.contains("FINDER_ACTION_FAILED status=-18"), "{serial}");
    assert!(serial.contains("FINDER_ALERT_DISMISSED"), "{serial}");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let arrow = fs::read(root.join("System7/system/assets/cursors/arrow.curs")).unwrap();
    for name in ["error.ppm", "dismissed.ppm"]
    {
        let frame = fs::read(output.join(name)).unwrap();
        assert_arrow(&frame, &arrow, [350, 350]);
    }
}

fn assert_arrow(frame: &[u8], cursor: &[u8], point: [usize; 2])
{
    let header = b"P6\n640 480\n255\n";
    assert!(frame.starts_with(header));
    let pixels = &frame[header.len()..];
    let word = |at| u16::from_be_bytes([cursor[at], cursor[at + 1]]);
    // Check the opaque sprite pixels. The background is allowed to be a folder
    // window or desktop, but a stalled animated watch cannot match this arrow.
    for row in 0..16usize
    {
        for column in 0..16usize
        {
            let bit = 0x8000 >> column;
            if word(32 + row * 2) & bit == 0
            {
                continue;
            }
            let x = point[0] + column - word(66) as usize;
            let y = point[1] + row - word(64) as usize;
            let expected = if word(row * 2) & bit != 0 { 0 } else { 255 };
            let at = (y * 640 + x) * 3;
            assert_eq!(&pixels[at..at + 3],
                       &[expected; 3],
                       "cursor is not responsive arrow at {x},{y}");
        }
    }
}
