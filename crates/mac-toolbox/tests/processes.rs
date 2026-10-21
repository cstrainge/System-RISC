//! Real single-hart RV64 application launch, timer preemption and ownership.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};
#[test]
#[ignore = "requires fresh cargo xtask image and QEMU"]
fn independent_applications_use_timer_preemption_and_private_toolbox_state()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join("results/system7").join(format!("processes-{}", std::process::id()));
    let transport = Command::new("python3").arg(root.join("System7/scripts/\
                                                           capture_system7_processes.py"))
                                           .arg("--output")
                                           .arg(&output)
                                           .current_dir(&root)
                                           .output()
                                           .unwrap();
    assert!(transport.status.success(),
            "process transport failed:\n{}\n{}",
            String::from_utf8_lossy(&transport.stdout),
            String::from_utf8_lossy(&transport.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert_eq!(report["original_before"], report["original_after"], "base artifacts changed");
    assert_eq!(report["owned_vm_exited"], true, "private QEMU remained running");
    assert_eq!(report["termination"], "guest-outcome");
    assert!(!serial.contains("PROCESS_FAIL"), "canonical assertion failed:\n{serial}");
    let command = report["launch"].as_array().unwrap();
    assert!(command.windows(2).any(|args| args[0] == "-smp" && args[1] == "1"),
            "preemption must be demonstrated on one hart");
    for marker in ["TRAPS_READY: resident ecall dispatcher ABI=7",
                   "PROCESS_INVALID_LAUNCH_PASS",
                   "PROCESS_PASCAL_PASS cases=5",
                   "PROCESS_PREEMPTION_PASS",
                   "PROCESS_FILE_IDENTITIES_PASS copies=2 concurrent=passed statics=private \
                    rename=passed move=passed foreground=passed exit=passed",
                   "PROCESS_USERSPACE_PASS preemption=passed launch=passed invalid=3 \
                    relaunch=passed windows=passed resources=passed memory=passed events=passed \
                    focus=passed hide=passed appmenu=passed"]
    {
        assert!(serial.contains(marker), "missing {marker}:\n{serial}");
    }
    let start = serial.find("PROCESS_SPIN_STARTED\n").expect("spinner never started");
    let progress =
        serial.find("PROCESS_OBSERVER_PROGRESS_DURING_SPIN\n").expect("observer made no progress");
    let finish = serial.find("PROCESS_SPIN_FINISHED\n").expect("spinner never returned");
    assert!(start < progress && progress < finish,
            "observer must run during child's no-trap/no-yield interval:\n{serial}");
    assert_eq!(serial.matches("PROCESS_WORKER_INPUT_PASS\n").count(),
               4,
               "all launches must receive routed physical input:\n{serial}");
    for actor in ["observer", "worker"]
    {
        assert_eq!(serial.matches(&format!("PROCESS_SYNTHETIC_EVENTS_PASS actor={actor} \
                                            cases=2\n"))
                         .count(),
                   if actor == "worker" { 4 } else { 2 },
                   "all {actor} lifecycles must retain and flush foreign-window synthetic \
                    events:\n{serial}");
    }
    assert_eq!(serial.matches("PROCESS_ATOMIC_ACTIVATION_PASS cases=5\n").count(),
               2,
               "both lifecycles must preserve atomic multiwindow activation and no-op visibility");
    for marker in
        ["PROCESS_BACKGROUND_CREATE_PASS cases=2\n", "PROCESS_MULTIWINDOW_EXIT_PASS cases=2\n"]
    {
        assert_eq!(serial.matches(marker).count(), 2, "missing multiwindow scenario {marker}");
    }
    let acceptance = serde_json::json!({"status":"passed","acceptance":"mac-toolbox/tests/process_cases.rs plus process_pascal_cases.rs and conformance/application_menu.rs","single_hart_timer_preemption":true,"worker_launches":4,"concurrent_copies":2,"identical_copy_filenames":"passed","relocated_statics_and_pointers":"passed","file_identity_rename_and_move":"passed","invalid_launches":3,"pascal_conversion_cases":5,"caller_posted_foreign_window_events":12,"atomic_activation_cases":10,"background_creation_cases":4,"background_selection_cases":4,"background_desktop_cases":6,"multiwindow_exit_cases":4,"resource_and_memory_partitions":"passed","window_ownership":"passed","private_and_physical_events":"passed","hide_focus_and_exit":"passed","capture":report,"limits":["validated RV64 relocation types only","one OS thread per app","shared address space; no MMU process isolation claim","bounded worker window/handle exit cleanup exercised four times","native UTF8 process services, no 68k Process Manager adapter"]});
    fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
    fs::write(root.join("results/system7/process-checks.json"),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
}
