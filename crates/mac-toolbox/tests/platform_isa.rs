//! Platform architecture acceptance, with Python only transporting real QEMU
//! boot/SDK-app evidence. Register assertions execute in Rust userspace.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};
fn root() -> PathBuf
{
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap()
}
#[test]
#[ignore = "requires fresh RVA23 release image and Linux QEMU"]
fn weaker_cpu_is_refused_before_rust_with_an_informative_floor_diagnostic()
{
    let root = root();
    for (index, cpu) in ["rv64,v=false", "rv64,c=false"].into_iter().enumerate()
    {
        let output =
            root.join(format!("results/system7/isa-refusal-{index}-{}", std::process::id()));
        let capture = Command::new("python3").arg(root.join("System7/scripts/\
                                                             capture_system7_isa_refusal.py"))
                                             .args(["--cpu", cpu, "--output"])
                                             .arg(&output)
                                             .output()
                                             .unwrap();
        assert!(capture.status.success(), "{}", String::from_utf8_lossy(&capture.stderr));
        let serial = fs::read_to_string(output.join("serial.log")).unwrap();
        assert!(serial.contains("BOOT_UNSUPPORTED_ISA: System RISC requires RVA23S64")
                && serial.contains("select the configured CPU and rebuild the whole system"),
                "missing actionable floor refusal:\n{serial}");
        assert!(!serial.contains("System RISC Boot ROM v1"),
                "Rust ran before unsupported-CPU refusal");
        assert!(!serial.contains("BOOT_TRAP:"),
                "unsupported CPU caused an unhandled trap instead of admission refusal:\n{serial}");
    }
}
fn context_capture(frontend: &str, bits: u32)
{
    let root = root();
    let output =
        root.join(format!("results/system7/isa-context-{frontend}-{bits}-{}", std::process::id()));
    let capture = Command::new("python3").arg(root.join("System7/scripts/\
                                                         capture_system7_processes.py"))
                                         .args(["--frontend",
                                                frontend,
                                                "--vector-bits",
                                                &bits.to_string(),
                                                "--output"])
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
    assert_eq!(report["original_before"], report["original_after"], "base installation modified");
    assert_eq!(report["owned_vm_exited"], true, "private VM leaked");
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(!serial.contains("PROCESS_FAIL"), "register/SDK assertion failed:\n{serial}");
    for (actor, kind) in [("observer", "ecalls"), ("spinner", "preemption")]
    {
        let marker = format!("RVA23_CONTEXT_PASS actor={actor} fp=32 vector=32 csr=5 bitmanip=3 \
                              {kind}=passed vlenb={}",
                             bits / 8);
        assert!(serial.contains(&marker), "missing {marker}:\n{serial}");
    }
    assert!(serial.contains("PROCESS_USERSPACE_PASS"),
            "subsequent SDK calls did not recover/progress:\n{serial}");
    let start = serial.find("PROCESS_SPIN_STARTED\n").unwrap();
    let progress = serial.find("PROCESS_OBSERVER_PROGRESS_DURING_SPIN\n").unwrap();
    let finish = serial.find("PROCESS_SPIN_FINISHED\n").unwrap();
    assert!(start < progress && progress < finish,
            "observer must progress while the no-trap spinner holds all register sentinels");
}
#[test]
#[ignore = "requires fresh RVA23 release image and Linux QEMU"]
fn vlen_scaled_state_survives_preemption_and_ecalls_at_128_and_256_bits()
{
    for bits in [128, 256]
    {
        context_capture("headless", bits);
    }
}
#[test]
#[ignore = "requires native Windows QEMU and fresh RVA23 release image"]
fn native_windows_uses_the_same_floor_and_preserves_cpu_state()
{
    context_capture("native-headless", 128);
}
