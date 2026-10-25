//! Actual native driver/module SDK acceptance. Mounted public File Manager APIs
//! require their separate gate; this checks their native transport prerequisite.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};

#[test]
#[ignore = "requires optimized private image, QEMU and disconnectable host companion"]
fn disconnecting_either_transfer_endpoint_preserves_unrelated_native_storage()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    for endpoint in ["source", "destination"]
    {
        let output = root.join("results/system7")
                         .join(format!("transfer-disconnect-{endpoint}-{}", std::process::id()));
        let result = Command::new("python3").arg(root.join("System7/scripts/\
                                                            capture_host_filesystem.py"))
                                            .arg("--output")
                                            .arg(&output)
                                            .arg("--disconnect")
                                            .arg(endpoint)
                                            .output()
                                            .unwrap();
        assert!(result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr));
        let capture: serde_json::Value =
            serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
        assert_eq!(capture["original_before"], capture["original_after"]);
        assert_eq!(capture["termination"], "guest-outcome", "{capture}");
        let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
        assert!(serial.contains("TRANSFER_DISCONNECT_READY\n"), "{serial}");
        assert!(serial.contains("TRANSFER_DISCONNECT_FAILURE status=")
                && serial.lines().any(|line| {
                                     line.starts_with("TRANSFER_DISCONNECT_FAILURE ")
                                     && line.contains(" detail=")
                                 }),
                "{serial}");
        assert!(serial.contains("TRANSFER_DISCONNECT_PASS cleanup=owned native-storage=usable \
                                 retry=passed\n")
                && serial.contains("HOST_FILESYSTEM_PASS\n"),
                "{serial}");
        for failure in
            ["HOST_FILESYSTEM_FAIL", "SYSTEM_PANIC:", "SYSTEM_TRAP:", "SYSTEM_ALLOCATION_FAILED:"]
        {
            assert!(!serial.contains(failure), "{serial}");
        }
    }
}

#[test]
#[ignore = "requires optimized private image, QEMU and real host exports"]
fn finder_double_click_launches_and_relaunches_an_external_application()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("external-app-ui-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/capture_host_desktop.py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .arg("--application")
                                        .output()
                                        .unwrap();
    assert!(result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let capture: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["original_before"], capture["original_after"]);
    assert_eq!(capture["guest_running_after_quit"], false);
    assert_eq!(capture["companion_running_after_quit"], false);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert_eq!(serial.matches("name=QuickDraw Test\n").count(), 2, "{serial}");
    assert_eq!(serial.matches("QUICKDRAW_TEST_READY\n").count(), 2, "{serial}");
    for failure in ["FINDER_ERROR", "SYSTEM_TRAP:", "SYSTEM_PANIC:", "SYSTEM_ALLOCATION_FAILED:",
                    "QUICKDRAW_TEST_ERROR", "PROCESS_ACTION_FAILED"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
    for frame in ["external-application.ppm", "external-relaunched.ppm"]
    {
        assert!(fs::metadata(output.join(frame)).unwrap().len() > 640 * 480 * 3);
    }
}

#[test]
#[ignore = "requires fresh private image, QEMU and the default Transfer export"]
fn default_transfer_is_initialized_and_mounted_in_the_private_profile()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join("results/system7")
                     .join(format!("native-default-transfer-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/capture_host_desktop.py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .arg("--default-transfer")
                                        .output()
                                        .unwrap();
    assert!(result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let capture: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["original_before"], capture["original_after"]);
    assert_eq!(capture["guest_running_after_quit"], false);
    assert_eq!(capture["companion_running_after_quit"], false);
    let selected = mac_build::paths::build_dir(&root).join("transfer");
    assert_eq!(capture["exports"]["exports"],
               serde_json::json!([format!("Transfer={}", selected.display())]));
    assert_ne!(selected,
               root.join("System7/transfer"),
               "acceptance must not touch the normal export");
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(serial.contains("HOST_VOLUME_MOUNTED index=0 "), "{serial}");
    assert!(serial.lines()
                  .any(|line| {
                      line.starts_with("FINDER_FOLDER_OPEN ") && line.ends_with("name=Transfer")
                  }),
            "{serial}");
    assert!(!serial.contains("FINDER_ERROR") && !serial.contains("HOST_VOLUME_MOUNT_FAILED"),
            "{serial}");
}

#[test]
#[ignore = "requires fresh private image, QEMU and real host exports"]
fn normal_launcher_automounts_exports_before_finder_and_releases_its_companion()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("native-host-desktop-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/capture_host_desktop.py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .output()
                                        .unwrap();
    assert!(result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let capture: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["original_before"], capture["original_after"]);
    assert_eq!(capture["guest_running_after_quit"], false);
    assert_eq!(capture["companion_running_after_quit"], false);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    let mounted = serial.find("HOST_VOLUME_MOUNTED index=1 ").expect(&serial);
    assert!(mounted < serial.find("SYSTEM_FINDER_LAUNCH").expect(&serial), "{serial}");
    for name in ["Desktop Host", "Desktop Read Only"]
    {
        assert!(serial.lines().any(|line| {
                                  line.starts_with("FINDER_VOLUME ")
                                  && line.ends_with(&format!("name={name}"))
                              }),
                "{serial}");
    }
    assert!(serial.lines()
                  .any(|line| {
                      line.starts_with("FINDER_FOLDER_OPEN ") && line.ends_with("name=Desktop Host")
                  }),
            "{serial}");
    for failure in ["HOST_VOLUME_MOUNT_FAILED", "HOST_VOLUME_DISCOVERY_FAILED", "FINDER_ERROR",
                    "SYSTEM_TRAP:", "SYSTEM_PANIC:"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
}

#[test]
#[ignore = "requires fresh private cargo xtask image, QEMU and Linux host companion"]
fn native_userspace_mounts_reuses_and_unmounts_host_volumes_through_public_files()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join("results/system7")
                     .join(format!("native-host-filesystem-sdk-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/capture_host_filesystem.\
                                                        py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .arg("--sdk")
                                        .output()
                                        .unwrap();
    assert!(result.status.success(),
            "capture failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let capture: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["original_before"], capture["original_after"]);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert_eq!(capture["termination"], "guest-outcome", "{serial}");
    assert!(serial.contains("HOST_FS_NATIVE_WRITE_GROWTH_PASS count=20 width=64 reuse=passed \
                             cleanup=passed\n"),
            "{serial}");
    assert!(serial.contains("HOST_FILESYSTEM_PASS"), "{serial}");
    assert_eq!(serial.matches("HOST_FS_WATCH_PASS\n").count(), 2, "{serial}");
    assert_eq!(serial.matches("HOST_FS_SNAPSHOT_PASS\n").count(), 2, "{serial}");
    assert_eq!(serial.matches("HOST_FS_SNAPSHOT_STALE status=-16 error=ScanStale \
                               detail=directory scan invalidated by a catalog change")
                     .count(),
               2,
               "{serial}");
    assert!(serial.contains("HOST_FS_EXIT_WORKER_READY"), "{serial}");
    assert!(serial.contains("HOST_FS_METADATA_PASS native=passed legacy=passed protected=passed \
                             retry=passed"),
            "{serial}");
    assert!(serial.contains("HOST_FS_METADATA_PROTECTED status=-12 error=AccessDenied \
                             detail=item is protected or locked"),
            "{serial}");
    assert!(serial.contains("FILE_TRANSFER_PASS forks=passed recursive=passed directions=passed \
                             identity=passed readonly=passed cancellation=passed stale=passed"),
            "{serial}");
    assert!(serial.contains("FILE_TRANSFER_DROP_PASS cleanup=passed retry=passed"), "{serial}");
    assert!(serial.contains("FILE_TRANSFER_EXIT_PASS cleanup=passed retry=passed"), "{serial}");
    assert!(serial.contains("HOST_FS_TRANSFER_LEDGER_PASS"), "{serial}");
    assert!(serial.contains("HOST_FS_TRANSFER_SCOPES_PASS owner=app fork=owned writer=owned"),
            "{serial}");
    let after_transfer = serial.split("HOST_FS_TRANSFER_LEDGER_PASS").nth(1).unwrap();
    let stopped =
        after_transfer.lines().find(|line| line.starts_with("PROCESS_STOPPED app=")).unwrap();
    let owner = stopped.split("app=").nth(1).unwrap().split_whitespace().next().unwrap();
    let prefix = format!("OWNER_WALK app={owner} ");
    let walked = after_transfer.lines().find(|line| line.starts_with(&prefix)).unwrap();
    let field = |name: &str| {
        walked.split_whitespace()
              .find_map(|part| part.strip_prefix(name))
              .and_then(|value| value.parse::<usize>().ok())
              .unwrap()
    };
    assert!(field("live=") == 0 && field("pending=") >= 3 && field("deferred=") >= 3,
            "transfer exit must retain its coordinator and real scoped fork/writer until I/O \
             completes: {walked}\n{serial}");
    let walk_at = after_transfer.find(&prefix).unwrap();
    let reclaimed_at =
        after_transfer.find(&format!("PROCESS_THREAD_RECLAIMED app={owner} ")).unwrap();
    let exited_at = after_transfer.find(&format!("PROCESS_EXIT app={owner}\n")).unwrap();
    assert!(walk_at < reclaimed_at && reclaimed_at < exited_at,
            "process must retain its saved state through another-stack thread reclaim and I/O \
             retirement: {serial}");
    assert!(serial.contains("FILE_TRANSFER_TRASH_PASS recursive=passed put-away=passed \
                             empty=passed retry=passed"),
            "{serial}");
    let reboot = &capture["reboot"];
    assert_eq!(reboot["termination"], "guest-outcome");
    assert_ne!(reboot["first_pid"], reboot["second_pid"]);
    let restored = fs::read_to_string(output.join("restore-vm/serial.log")).unwrap();
    assert!(restored.contains("HOST_METADATA_RESTORE_PASS"), "{restored}");
    assert!(!restored.contains("HOST_FILESYSTEM_FAIL")
            && !restored.contains("SYSTEM_PANIC:")
            && !restored.contains("SYSTEM_TRAP:"),
            "{restored}");
    let snapshot = |text: &str| {
        text.lines()
            .find(|line| line.starts_with("HOST_FS_METADATA_SNAPSHOT "))
            .unwrap_or_else(|| panic!("missing native metadata snapshot: {text}"))
            .split_whitespace()
            .skip(1)
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(snapshot(&serial),
               snapshot(&restored),
               "cold boot changed catalog identities or dates");
    assert_eq!(serial.matches("FILE_PROPERTIES_PASS atomic=passed no-change=passed \
                               locked=passed retry=passed")
                     .count(),
               2,
               "{serial}");
    assert!(serial.contains("FILE_PROPERTIES_LOCKED status=-12 error=AccessDenied detail=item \
                             is protected or locked"),
            "{serial}");
    assert!(serial.contains("FILE_PROPERTIES_INVALID status=-2 error=BadArgument detail=invalid \
                             argument"),
            "{serial}");
    assert!(serial.contains("HOST_FS_REMOVE_PASS native=passed legacy=passed readonly=passed \
                             retry=passed"),
            "{serial}");
    assert!(serial.contains("HOST_FS_REMOVE_NONEMPTY status=-15 error=DirectoryNotEmpty \
                             detail=directory is not empty"),
            "{serial}");
    assert!(serial.contains("HOST_FS_REMOVE_READ_ONLY status=-10 error=ReadOnly detail=volume \
                             is read-only"),
            "{serial}");
    assert!(serial.contains("HOST_FS_TRASH_CREATE_PASS native=passed legacy=passed \
                             readonly=passed"),
            "{serial}");
    assert!(serial.contains("HOST_FS_TRASH_READ_ONLY status=-10 error=ReadOnly"), "{serial}");
    assert_eq!(serial.matches("HOST_FS_EXTERNAL_APPLICATION_PASS\n").count(), 3, "{serial}");
    for marker in ["HOST_FS_EXTERNAL_APPLICATION_READY_RW",
                   "HOST_FS_EXTERNAL_APPLICATION_READY_RO",
                   "HOST_FS_EXTERNAL_APPLICATION_EXIT_RO",
                   "HOST_FS_EXTERNAL_APPLICATION_PASCAL_PASS",
                   "HOST_FS_EXTERNAL_APPLICATION_REJECTED status=-18 error=CorruptData \
                    detail=file or catalog data is damaged or malformed"]
    {
        assert!(serial.contains(marker), "{marker}: {serial}");
    }

    assert!(serial.contains("PROCESS_LAUNCHED app=") && serial.contains("name=Host Exit Worker"),
            "{serial}");
    for failure in ["HOST_FILESYSTEM_FAIL", "HOST_FS_EXIT_WORKER_FAIL", "MODULE_REJECTED",
                    "SYSTEM_TRAP:", "SYSTEM_PANIC:"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
    let report = serde_json::json!({"status":"passed", "public_file_apis":"host export count; mount; volume metadata; catalog; direct-child and incremental cursor enumeration; unknown child count and allocation as Rust options; parent; nested folders and Unicode names; relative/absolute/dot/parent/Unicode path lookup and bounded path output; both-fork open/read/seek/tell/close; EOF and independent marks; Drop; busy unmount; stale/foreign file and volume IDs; cursor binding after remount; read-only mutation rejection; repeated mount; invalid source cleanup; unmount; real second-app exit with two forgotten provider forks followed by acknowledged cleanup/unmount",
        "public_writes":"DataWriter Unicode creation, multi-batch data/readback, zero-length files, duplicate/wrong-ID refusal, partial staging and Drop, existing-reader contention, replacement preserving the resource fork and Finder metadata, stale/foreign write IDs, catalog cursor invalidation; legacy Pascal-name creation/append/commit/readback through the same resident services; real second-app exit with forgotten RAM-only writer staging",
        "resource_inspection":"every resource type, entry, metadata/name/attributes and payload of a host-mounted ResEdit matches its block-backed startup copy through the public SDK; payload reads cross the 64 KiB class boundary; independent positioned inspection preserves fork position; empty/malformed forks, EOF, invalid offsets, warmed metadata, busy unmount, closed/stale/foreign resource-fork IDs and slot reuse; original Pascal resource trap reaches the same provider service",
        "plain_entries":"raw host files with empty resource forks, raw directories, independent extension classification, directory creation inside a raw host directory and nested file publication, case-collision refusal, read-only directory-creation rejection",
        "watches":"coalesced nonblocking directory watches; actual host-created arrivals on writable and read-only exports; readback after automatic cache invalidation; owner exit with a forgotten watch; local ownership and forced-slot-reuse checks in file_ownership",
        "external_applications":"writable and read-only provider native launches, private application resources, same-file activation without a duplicate process, damaged fork rejection with useful text, immediate valid launch after failure and acknowledged cleanup on exit",
        "transfer_and_mutation_acceptance":"native and provider property edits; empty/nonempty/readonly removal; recursive two-fork host-to-native move and native-to-host copy; identity-preserving moves; forced same-folder copy names; cycles/collisions/readonly refusal; partial-copy cancellation with exact date restoration; RAII Drop and actual app-exit cleanup followed by retry; recursive Trash/Put Away/Empty; cold-boot identity and metadata restoration",
        "not_covered":"classic notification trap compatibility, file-specific watches, exhaustive failure injection at every provider phase and sustained-I/O latency; disconnect and actual Finder gestures have separate native gates", "capture":capture});
    fs::write(output.join("rust-acceptance.json"), serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    fs::write(root.join("results/system7/host-filesystem-mount-checks.json"),
              serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}

#[test]
#[ignore = "requires fresh private cargo xtask image, Linux QEMU and host companion"]
fn native_filesystem_callback_reaches_the_real_host_through_relocated_modules()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("native-host-filesystem-{}", std::process::id()));
    let result = Command::new("python3").arg(root.join("System7/scripts/capture_host_filesystem.\
                                                        py"))
                                        .arg("--output")
                                        .arg(&output)
                                        .output()
                                        .unwrap();
    assert!(result.status.success(),
            "capture failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    let capture: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["original_before"], capture["original_after"]);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    for name in ["Host Transport", "Host Filesystem", "Host Filesystem Probe"]
    {
        assert!(serial.contains(&format!("MODULE_PUBLISHED name={name} id=")),
                "{name} failed:\n{serial}");
    }
    let transport = serial.find("MODULE_PUBLISHED name=Host Transport id=").unwrap();
    let filesystem = serial.find("MODULE_ENTER name=Host Filesystem id=").unwrap();
    let probe = serial.find("MODULE_ENTER name=Host Filesystem Probe id=").unwrap();
    assert!(transport < filesystem && filesystem < probe, "dependency order: {serial}");
    for failure in ["MODULE_REJECTED", "SYSTEM_TRAP:", "SYSTEM_PANIC:"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
    assert!(serial.contains("DESKTOP_READY"), "{serial}");
    let report = serde_json::json!({"status":"passed", "native_module_callbacks":"FSYS Exports through HDIR/virtio and real Linux host companion",
        "mounted_public_file_api":"not covered", "capture":capture});
    fs::write(output.join("rust-acceptance.json"), serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    fs::write(root.join("results/system7/host-filesystem-module-checks.json"),
              serde_json::to_vec_pretty(&report).unwrap()).unwrap();
}
