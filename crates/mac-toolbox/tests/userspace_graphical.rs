//! Boot the real graphical System with a private userspace conformance Finder.
//! Python only packages/boots/captures; canonical Rust guest assertions decide PASS.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};
#[test]
#[ignore = "requires fresh cargo xtask image, qemu-system-riscv64 and original captured fixtures"]
fn disk_booted_aggregate_userspace_conformance_uses_real_ecalls()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("userspace-conformance-{}", std::process::id()));
    let capture = Command::new("python3").arg(root.join("System7/scripts/\
                                                         capture_system7_conformance.py"))
                                         .arg("--output")
                                         .arg(&output)
                                         .current_dir(&root)
                                         .output()
                                         .expect("start isolated graphical conformance guest");
    assert!(capture.status.success(),
            "transport failed:\n{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert_eq!(report["original_before"], report["original_after"], "base OS artifacts changed");
    assert_eq!(report["termination"], "guest-outcome", "guest did not report outcome:\n{serial}");
    assert!(!serial.contains("TOOLBOX_USERSPACE_FAIL"),
            "canonical userspace assertion failed:\n{serial}");
    assert!(serial.contains("TOOLBOX_USERSPACE_PASS geometry=20 ovals=267 handles=48 \
                             controls=86 controlpixels=269600 controlhits=8424 strings=passed \
                             dates=passed"),
            "missing complete canonical success marker:\n{serial}");
    assert!(serial.contains("TEXTEDIT_USERSPACE_PASS original=356 native=20 legacy=6 \
                             navigation=49"),
            "missing TextEdit userspace contracts:\n{serial}");
    assert!(serial.contains("SCRAP_USERSPACE_PASS original=18 native=17 legacy=12"),
            "missing Scrap Manager userspace contracts:\n{serial}");
    assert!(serial.contains("TRAPS_READY: resident ecall dispatcher ABI=6"),
            "actual resident ECALL gate absent:\n{serial}");
    assert!(serial.contains("EVENTS_USERSPACE_PASS original_queue=passed native_queue=passed"),
            "Event Manager public ECALL conformance absent:\n{serial}");
    assert!(serial.contains("OWNERSHIP_USERSPACE_PASS scope=40 early_returns=16 stale=passed \
                             transfer=passed"),
            "owned window/handle lifecycle acceptance absent:\n{serial}");
    assert!(serial.contains("HANDLE_REUSE_USERSPACE_PASS windows=40 native=9 legacy=3 \
                             retirement=passed"),
            "shared handle validation through native and legacy traps absent:\n{serial}");
    assert!(serial.contains("SCROLL_BOX_USERSPACE_PASS parent_cleanup=8 stale=passed \
                             legacy=passed rows=passed"),
            "resident scroll-box lifecycle acceptance absent:\n{serial}");
    for definition in ["CODE_LOADED CDEF0", "CODE_LOADED CDEF1"]
    {
        assert!(serial.contains(definition),
                "loaded definition evidence {definition} absent:\n{serial}");
    }
    let acceptance = serde_json::json!({"status":"passed","acceptance":"mac-toolbox/tests/conformance::run_all plus strings::shared/graphical","transport":"real graphical disk boot and RV64 ECALL","controls":{"cases":86,"pixels":269600,"pixel_differences":0,"hit_records":8424,"distinct_hit_tuples":1296,"hit_differences":0},"strings":"passed","dates":"passed","textedit":{"original":356,"native":20,"legacy":6,"navigation":49},"scrap":{"original":18,"native":17,"legacy":12},"capture":report,"output":output});
    fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
    fs::write(root.join("results/system7/userspace-conformance-checks.json"),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
}
