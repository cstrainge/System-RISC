//! Real single-hart RV64 application launch, timer preemption and ownership.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};
#[test]
#[ignore = "requires fresh cargo xtask image and QEMU"]
fn independent_applications_use_timer_preemption_and_private_toolbox_state()
{
    run(false);
}
#[test]
#[ignore = "requires fresh cargo xtask image and QEMU"]
fn malformed_system_open_layout_is_rolled_back_and_repeated_requests_remain_usable()
{
    run(true);
}
fn run(malformed: bool)
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let suffix = if malformed { "-malformed-standard-file" } else { "" };
    let output =
        root.join("results/system7").join(format!("processes-{}{suffix}", std::process::id()));
    let mut command = Command::new("python3");
    command.arg(root.join("System7/scripts/capture_system7_processes.py"))
           .arg("--output")
           .arg(&output)
           .current_dir(&root);
    if malformed
    {
        command.arg("--malformed-standard-file");
    }
    let transport = command.output().unwrap();
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
    assert_eq!(report["malformed_standard_file"], malformed);
    let marker = if malformed
    {
        "PROCESS_STANDARD_FILE_RAII_PASS mode=invalid-resource retry=passed"
    }
    else
    {
        "PROCESS_STANDARD_FILE_RAII_PASS mode=normal reserved=passed"
    };
    assert_eq!(serial.lines().filter(|line| *line == marker).count(),
               4,
               "all worker incarnations must verify the System-owned dialog lifetime: {serial}");
    if malformed
    {
        assert_eq!(serial.lines()
                         .filter(|line| {
                             line.starts_with("PROCESS_STANDARD_FILE_REFUSAL DLOG -6042 error=")
                         })
                         .count(),
                   8);
    }
    assert!(!serial.contains("PROCESS_FAIL"), "canonical assertion failed:\n{serial}");
    let command = report["launch"].as_array().unwrap();
    assert!(command.windows(2).any(|args| args[0] == "-smp" && args[1] == "1"),
            "preemption must be demonstrated on one hart");
    for marker in
        ["TRAPS_READY: resident ecall dispatcher ABI=1",
         "PROCESS_INVALID_LAUNCH_PASS",
         "FORMAT_ERROR_STATUS_PASS budget=no-memory corrupt=corrupt-data missing=not-found \
          translation=translation-required retry=passed",
         "PROCESS_PASCAL_PASS cases=5",
         "PROCESS_PREEMPTION_PASS",
         "RVA23_CONTEXT_PASS actor=observer fp=32 vector=32 csr=5 bitmanip=3 ecalls=passed",
         "RVA23_CONTEXT_PASS actor=spinner fp=32 vector=32 csr=5 bitmanip=3 preemption=passed",
         "PROCESS_INCARNATIONS_PASS native=passed legacy=passed foreign=passed relaunch=passed",
         "PROCESS_FILE_IDENTITIES_PASS copies=2 concurrent=passed statics=private rename=passed \
          move=passed foreground=passed exit=passed",
         "PROCESS_USERSPACE_PASS preemption=passed launch=passed invalid=5 relaunch=passed \
          windows=passed resources=passed memory=passed events=passed focus=passed hide=passed \
          appmenu=passed"]
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
    let workers: Vec<_> = serial.lines()
                                .filter_map(|line| {
                                    line.strip_prefix("PROCESS_WORKER_LEAVING_CONTROL app=")
                                        .and_then(|tail| tail.split_once(" control="))
                                        .map(|(app, _)| app)
                                })
                                .collect();
    assert_eq!(workers.len(),
               4,
               "all workers must leave a live registered control for System cleanup");
    for app in workers
    {
        for (kind, live) in [("WIND", 2),
                             ("WNST", 1),
                             ("CTRL", 1),
                             ("CSTA", 1),
                             ("SBOX", 1),
                             ("SBST", 1),
                             ("SFDL", 0),
                             ("SFST", 1),
                             ("GWLD", 1),
                             ("GWST", 1)]
        {
            assert!(serial.lines().any(|line| {
                                      line
                                      == format!("PROCESS_WORKER_POOL app={app} kind={kind} \
                                                  live={live} pending=0")
                                  }),
                    "worker {app} must leave its actual {kind} pool for generic \
                     cleanup:\n{serial}");
        }
        assert!(serial.lines().any(|line| {
                                  line
                                  == format!("OWNER_WALK app={app} invalidated=8 released=8 \
                                              quarantined=0 live=0 pending=0 deferred=0 \
                                              completed=10")
                              }),
                "registered owner walk did not complete all ten leaked worker resources for \
                 {app}:\n{serial}");
    }
    assert!(!serial.contains("OWNER_CLEANUP_QUARANTINED"),
            "unexpected native cleanup refusal:\n{serial}");
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
    let acceptance = serde_json::json!({"status":"passed","acceptance":"mac-toolbox/tests/process_cases.rs plus process_pascal_cases.rs and conformance/application_menu.rs","single_hart_timer_preemption":true,"worker_launches":4,"concurrent_copies":2,"identical_copy_filenames":"passed","relocated_statics_and_pointers":"passed","file_identity_rename_and_move":"passed","invalid_launches":5,"typed_format_status_cases":4,"boot_unique_process_incarnations":"passed","stale_native_and_legacy_ids_after_relaunch":"passed","pascal_conversion_cases":5,"caller_posted_foreign_window_events":12,"atomic_activation_cases":10,"background_creation_cases":4,"background_selection_cases":4,"background_desktop_cases":6,"multiwindow_exit_cases":4,"resource_and_memory_partitions":"passed","window_ownership":"passed","private_and_physical_events":"passed","hide_focus_and_exit":"passed","capture":report,"limits":["validated RV64 relocation types only","one OS thread per app","shared address space; no MMU process isolation claim","bounded worker window/handle exit cleanup exercised four times","native UTF8 process services, no 68k Process Manager adapter"]});
    fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
    fs::write(root.join("results/system7/process-checks.json"),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
}
