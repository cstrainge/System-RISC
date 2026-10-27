//! Mapping rows travel through public SDK ECALLs, from actual owned System PTEs.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};
#[test]
#[ignore = "requires a current optimized image and private graphical QEMU"]
fn actual_mapping_inventory_matches_owned_root_heap_stack_guards_and_ram()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("memory-inspection-{}", std::process::id()));
    let capture = Command::new("python3").arg(root.join("System7/scripts/\
                                                         capture_system7_memory_inspection.py"))
                                         .arg("--output")
                                         .arg(&output)
                                         .current_dir(&root)
                                         .output()
                                         .unwrap();
    assert!(capture.status.success(),
            "native mapping capture failed:\n{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    assert_eq!(report["owned_vm_exited"], true);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(serial.contains("TOOLBOX_USERSPACE_PASS "),
            "native SDK assertions did not pass:\n{serial}");
    for failure in
        ["TOOLBOX_USERSPACE_FAIL", "SYSTEM_TRAP:", "SYSTEM_PANIC:", "SYSTEM_ALLOCATION_FAILED:"]
    {
        assert!(!serial.contains(failure), "unexpected {failure}:\n{serial}");
    }
    let field = |line: &str, name: &str| {
        line.split_whitespace()
            .find_map(|part| part.strip_prefix(&format!("{name}=")))
            .unwrap()
            .to_owned()
    };
    let mapping = serial.lines().find(|l| l.starts_with("MEMORY_SNAPSHOT ")).unwrap();
    let admitted = serial.lines().find(|l| l.starts_with("MMU_READY ")).unwrap();
    assert_eq!(field(mapping, "root"),
               field(admitted, "kernel_root"),
               "SDK must report the actually installed owned root");
    assert_eq!(field(mapping, "stack"), "4194304");
    assert_eq!(field(mapping, "guards"), "2");
    assert!(field(mapping, "rows").parse::<u64>().unwrap() > 0);
    fs::write(output.join("rust-acceptance.json"),b"{\"result\":\"pass\",\"coverage\":\"actual System PTE mapping batches, RX code, RW/NX heap, guarded stack, RAM aliases, informative refusal and retry\"}\n").unwrap();
}
