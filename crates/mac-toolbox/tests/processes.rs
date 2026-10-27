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
    // Actual public account bytes must equal the real physical leases in the
    // scheduler trace, not a copied configured stack/CPU size in inspection.
    let field = |line: &str, name: &str| -> u64 {
        line.split_whitespace()
            .find_map(|part| part.strip_prefix(name))
            .unwrap_or_else(|| panic!("missing {name}: {line}"))
            .parse()
            .unwrap()
    };
    let accounts: Vec<_> =
        serial.lines().filter(|line| line.starts_with("PROCESS_EXECUTION_ACCOUNT ")).collect();
    assert_eq!(accounts.iter().filter(|line| line.contains(" actor=worker ")).count(),
               4,
               "ordinary workers must all read their own real account: {serial}");
    assert_eq!(accounts.iter().filter(|line| line.contains(" actor=observed-worker ")).count(), 2);
    for row in accounts
    {
        assert!(row.ends_with("coverage=stack-cpu-only"), "account overclaimed coverage: {row}");
        let owner = field(row, "app=");
        let backing =
            serial.lines()
                  .find(|line| line.starts_with(&format!("PROCESS_THREAD app={owner} ")))
                  .unwrap_or_else(|| panic!("account has no actual native thread backing: {row}"));
        assert_eq!(field(row, "application="),
                   field(backing, "stack_backing_bytes="),
                   "stack lease and public account disagree: {row}; {backing}");
        assert_eq!(field(row, "kernel="),
                   field(backing, "cpu_backing_bytes="),
                   "CPU lease and public account disagree: {row}; {backing}");
    }
    let retired_accounts: Vec<_> =
        serial.lines()
              .filter(|line| line.starts_with("PROCESS_EXECUTION_ACCOUNT_RETIRED "))
              .collect();
    assert_eq!(retired_accounts.len(),
               2,
               "both observed child accounts must retire after their physical backing is \
                reclaimed: {serial}");
    for row in retired_accounts
    {
        let owner = field(row, "app=");
        let reclaimed = serial.find(&format!("PROCESS_THREAD_RECLAIMED app={owner} "))
                              .expect("account retired before backing reclamation");
        let retired = serial.find(row).unwrap();
        assert!(reclaimed < retired, "account vanished before its backing was reclaimed: {row}");
    }
    let finder = serial.lines()
                       .find_map(|line| line.strip_prefix("BOOT_FINDER_DOMAIN app="))
                       .expect("bootstrap did not provision Finder before its image");
    assert_ne!(finder, "0", "Finder bootstrap needs a genuine lifetime domain");
    let provisioned = serial.find(&format!("BOOT_FINDER_DOMAIN app={finder}\n")).unwrap();
    let scheduled = serial.find(&format!("PROCESS_THREAD app={finder} ")).unwrap();
    let registered =
        serial.find(&format!("PROCESS_REGISTERED app={finder} name=Finder\n")).unwrap();
    let resources_released =
        serial.find("BOOT_RESOURCE_SCOPE_RELEASED\n")
              .expect("bootstrap source selection remained on the abandoned boot stack");
    let entry = serial.find("DESKTOP_READY: public Process Manager acceptance\n").unwrap();
    assert!(provisioned < scheduled
            && scheduled < registered
            && registered < resources_released
            && resources_released < entry,
            "Finder bootstrap ownership/scope order is invalid: {serial}");
    let command = report["launch"].as_array().unwrap();
    assert!(command.windows(2).any(|args| args[0] == "-smp" && args[1] == "1"),
            "preemption must be demonstrated on one hart");
    for marker in
        ["TRAPS_READY: resident ecall dispatcher ABI=1",
         "PROCESS_INVALID_LAUNCH_PASS",
         "PROCESS_IMAGE_LIFETIMES_PASS distinct-files=16 full-width=passed foreign=passed \
          stale=passed",
         "PROCESS_PROVISIONAL_CLEANUP_PASS cases=32 resource-context=restored retry=passed",
         "FORMAT_ERROR_STATUS_PASS budget=no-memory corrupt=corrupt-data missing=not-found \
          translation=translation-required retry=passed",
         "PROCESS_PASCAL_PASS cases=5",
         "PROCESS_PREEMPTION_PASS",
         "SYSTEM_STACK_ZERO_SP_PASS refusal=not-supported original-stack=restored",
         "RVA23_CONTEXT_PASS actor=observer fp=32 vector=32 csr=5 bitmanip=3 ecalls=passed",
         "RVA23_CONTEXT_PASS actor=spinner fp=32 vector=32 csr=5 bitmanip=3 preemption=passed",
         "PROCESS_INCARNATIONS_PASS native=passed legacy=passed foreign=passed relaunch=passed",
         "PROCESS_FILE_IDENTITIES_PASS copies=2 concurrent=passed statics=private rename=passed \
          move=passed foreground=passed exit=passed",
         "PROCESS_DOCUMENT_LIFETIMES_PASS capacity=20 handoff=passed foreign=passed stale=passed \
          exit=passed",
         "PROCESS_USERSPACE_PASS preemption=passed launch=passed invalid=5 relaunch=passed \
          windows=passed resources=passed memory=passed events=passed focus=passed hide=passed \
          appmenu=passed"]
    {
        assert!(serial.contains(marker), "missing {marker}:\n{serial}");
    }
    let aborted: Vec<_> = serial.lines()
                                .filter_map(|line| {
                                    let tail = line.strip_prefix("PROCESS_LAUNCH_ABORT app=")?;
                                    tail.strip_suffix(" resource_scope=system-only")
                                })
                                .collect();
    assert_eq!(aborted.len(),
               32,
               "each native loader refusal must enter the common walk: {serial}");
    for app in aborted
    {
        assert!(!serial.lines()
                       .any(|line| line.starts_with(&format!("PROCESS_THREAD app={app} "))),
                "refused image admission published a thread: {serial}");
        let stopped =
            serial.lines()
                  .position(|line| {
                      line == format!("PROCESS_LAUNCH_ABORT app={app} resource_scope=system-only")
                  })
                  .unwrap();
        let retired = serial.lines()
                            .position(|line| line == format!("PROCESS_EXIT app={app}"))
                            .unwrap_or_else(|| {
                                panic!("provisional domain {app} did not pass its ledger \
                                        retirement barrier: {serial}")
                            });
        assert!(stopped < retired, "provisional domain retired before cleanup: {serial}");
    }
    assert_eq!(serial.lines().filter(|line| *line == "IMAGE_PROBE_PASS").count(),
               16,
               "all distinct-file image probes must execute: {serial}");
    assert_eq!(serial.lines().filter(|line| *line == "IMAGE_PROBE_DEFINITION_PASS").count(),
               16,
               "all probes must draw through their application-owned CDEF: {serial}");
    let definitions: Vec<_> = serial.lines()
                                    .filter(|line| line.starts_with("CODE_LOADED CDEF-32759 "))
                                    .filter_map(|line| {
                                        let (_, tail) = line.split_once(" origin=")?;
                                        let (origin, tail) = tail.split_once(" linked=")?;
                                        let origin = origin.parse::<u64>().ok()?;
                                        let (_, base) = tail.split_once(" actual=")?;
                                        (origin != 0).then_some((origin, base))
                                    })
                                    .collect();
    assert_eq!(definitions.len(), 16, "definition source was lost or substituted: {serial}");
    assert!(definitions.iter().all(|(origin, _)| *origin > u64::from(u32::MAX)));
    assert_eq!(definitions.iter()
                          .map(|(origin, _)| origin)
                          .collect::<std::collections::BTreeSet<_>>()
                          .len(),
               16,
               "definition binding retargeted a previous source");
    assert_eq!(definitions.iter()
                          .map(|(_, base)| base)
                          .collect::<std::collections::BTreeSet<_>>()
                          .len(),
               1,
               "retired application definitions did not release their physical reservation");
    let probes: Vec<_> = serial.lines()
                               .filter_map(|line| {
                                   let tail = line.strip_prefix("PROCESS_LAUNCHED app=")?;
                                   let (app, tail) = tail.split_once(" thread=")?;
                                   let (_, name) = tail.split_once(" name=")?;
                                   name.starts_with("ImageProbe ").then_some(app)
                               })
                               .collect();
    assert_eq!(probes.len(), 16);
    for app in probes
    {
        let stack =
            serial.lines()
                  .position(|line| {
                      line.starts_with(&format!("PROCESS_THREAD_RECLAIMED app={app} "))
                  })
                  .unwrap_or_else(|| panic!("image probe {app} retained its stack: {serial}"));
        let retired = serial.lines()
                            .position(|line| line == format!("PROCESS_EXIT app={app}"))
                            .unwrap_or_else(|| {
                                panic!("image probe {app} left image aliases/backing pending: \
                                        {serial}")
                            });
        assert!(stack < retired, "image source retired before its execution stack: {serial}");
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
        let stopped =
            serial.lines()
                  .position(|line| line.starts_with(&format!("PROCESS_STOPPED app={app} ")))
                  .expect("worker admission and execution were not stopped");
        let walked = serial.lines()
                           .position(|line| line.starts_with(&format!("OWNER_WALK app={app} ")))
                           .expect("worker never entered the common owner walk");
        let reclaimed =
            serial.lines()
                  .position(|line| {
                      line.starts_with(&format!("PROCESS_THREAD_RECLAIMED app={app} "))
                  })
                  .expect("worker stack/context never reached the different-stack checkpoint");
        let retired = serial.lines()
                            .position(|line| line == format!("PROCESS_EXIT app={app}"))
                            .expect("worker domain never retired");
        assert!(stopped < walked && walked < reclaimed && reclaimed < retired,
                "{app} must retain saved state until another stack reclaims its thread, then \
                 pass the ledger barrier: {serial}");
        for (kind, live) in [("THRD", 1),
                             ("THST", 1),
                             ("IMAG", 1),
                             ("IMST", 1),
                             ("WIND", 3),
                             ("WNST", 1),
                             ("CTRL", 3),
                             ("CSTA", 1),
                             ("SBOX", 1),
                             ("SBST", 1),
                             ("SFDL", 0),
                             ("SFST", 1),
                             ("GWLD", 1),
                             ("GWST", 1),
                             ("MENU", 1),
                             ("MNST", 1),
                             ("MMBL", 1),
                             ("MMST", 1),
                             ("TEDT", 1),
                             ("TESV", 1),
                             ("ALRT", 1),
                             ("ALST", 1),
                             ("DRAG", 1),
                             ("DRST", 1),
                             ("DRCV", 3),
                             ("DRVS", 1),
                             ("FWCH", 1),
                             ("FWST", 1),
                             ("FFRK", 1),
                             ("FFST", 1),
                             ("FDWR", 1),
                             ("FDST", 1),
                             ("FIGR", 1),
                             ("FIGS", 1),
                             ("FPIN", 1),
                             ("FPST", 1)]
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
                                  == format!("OWNER_WALK app={app} invalidated=33 released=30 \
                                              quarantined=0 live=0 pending=3 deferred=3 \
                                              completed=41")
                              }),
                "registered owner walk did not complete all ordinary worker resources while \
                 retaining its thread, backing and image through stack reclamation for \
                 {app}:\n{serial}");
    }
    assert!(!serial.contains("OWNER_CLEANUP_QUARANTINED"),
            "unexpected native cleanup refusal:\n{serial}");
    let document_app =
        serial.lines()
              .find_map(|line| {
                  line.strip_prefix("PROCESS_DOCUMENT_SCOPE_EXIT app=")
                      .and_then(|tail| tail.split_once(" batch="))
                      .map(|(app, _)| app)
              })
              .expect("document receiver did not leave its real batch for System cleanup");
    let walked =
        serial.lines()
              .position(|line| {
                  line.starts_with(&format!("OWNER_WALK app={document_app} "))
                  && line.contains("quarantined=0 live=0 pending=7 ")
              })
              .expect("document root, three pin aliases, thread/backing and execution image must \
                       retain their real pending death barrier");
    let reclaimed = serial.lines()
                          .position(|line| {
                              line.starts_with(&format!("PROCESS_THREAD_RECLAIMED \
                                                         app={document_app} "))
                          })
                          .expect("document receiver thread was not reclaimed elsewhere");
    let retired = serial.lines()
                        .position(|line| line == format!("PROCESS_EXIT app={document_app}"))
                        .expect("document batch or alias left its process stuck after cleanup");
    assert!(walked < reclaimed && reclaimed < retired,
            "document process bypassed stack or scoped-I/O retirement barrier: {serial}");
    for line in serial.lines().filter(|line| line.starts_with("PROCESS_RETIRE_RETAINED"))
    {
        assert!(line.starts_with(&format!("PROCESS_RETIRE_RETAINED app={document_app} ")),
                "ordinary worker stranded saved state: {line}");
    }
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
    let acceptance = serde_json::json!({"status":"passed","acceptance":"mac-toolbox/tests/process_cases.rs plus process_pascal_cases.rs and conformance/application_menu.rs","single_hart_timer_preemption":true,"worker_launches":4,"concurrent_copies":2,"identical_copy_filenames":"passed","relocated_statics_and_pointers":"passed","file_identity_rename_and_move":"passed","invalid_launches":5,"typed_format_status_cases":4,"boot_unique_process_incarnations":"passed","stale_native_and_legacy_ids_after_relaunch":"passed","pascal_conversion_cases":5,"caller_posted_foreign_window_events":12,"atomic_activation_cases":10,"background_creation_cases":4,"background_selection_cases":4,"background_desktop_cases":6,"multiwindow_exit_cases":4,"resource_and_memory_partitions":"passed","image_sources":{"distinct_files":16,"full_width":true,"stale_after_reuse":true,"foreign_kind_rejected":true,"saved_view_after_exit":"not-found","stack_before_source_retirement":true},"window_ownership":"passed","private_and_physical_events":"passed","hide_focus_and_exit":"passed","document_batch_capacity":20,"document_handoff":"new receiver identity, stale sender and foreign disposal rejected","document_scope_exit":"document root, three pin aliases and image drain before actual process retirement","capture":report,"limits":["validated RV64 relocation types only","one OS thread per app","shared address space; no MMU process isolation claim","bounded worker window/handle exit cleanup exercised four times","native UTF8 process services, no 68k Process Manager adapter"]});
    fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
    fs::write(root.join("results/system7/process-checks.json"),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
}
