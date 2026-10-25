//! Reactor Room is the first visible customer of public lifetime inspection.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::{Path, PathBuf},
          process::Command};

fn frame(path: &Path) -> Vec<u8>
{
    let bytes = fs::read(path).unwrap();
    let header = b"P6\n640 480\n255\n";
    assert!(bytes.starts_with(header));
    assert_eq!(bytes.len(), header.len() + 640 * 480 * 3);
    bytes[header.len()..].to_vec()
}
#[test]
#[ignore = "requires an optimized installation and a private graphical QEMU"]
fn reactor_uses_public_policy_grants_and_managed_views_without_selection_repaints()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join("results/system7").join(format!("reactor-ui-{}", std::process::id()));
    let mut command = Command::new("python3");
    command.arg(root.join("System7/scripts/capture_system7_reactor.py"))
           .arg("--output")
           .arg(&output)
           .current_dir(&root);
    if std::env::var_os("SYSTEM7_REACTOR_WINDOWS").is_some()
    {
        command.args(["--frontend", "native-headless"]);
    }
    let capture = command.output().unwrap();
    assert!(capture.status.success(),
            "Reactor Room capture failed:\n{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"],
               "capture changed installation artifacts");
    assert_eq!(report["owned_vm_exited"], true);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(serial.contains("name=Reactor Room\n"));
    assert!(serial.contains("REACTOR_READY coverage=process-and-module-domains,partial-ledgers \
                             authority=system-policy\n"));
    for failure in ["REACTOR_ERROR", "REACTOR_INSPECTION_ERROR", "SYSTEM_PANIC:", "SYSTEM_TRAP:",
                    "SYSTEM_ALLOCATION_FAILED:"]
    {
        assert!(!serial.contains(failure), "unexpected {failure}:\n{serial}");
    }
    let first = frame(&output.join("selected-first.ppm"));
    let again = frame(&output.join("selected-again.ppm"));
    assert_eq!(first, again, "reselecting the same domain changed pixels");
    let second = frame(&output.join("selected-second.ppm"));
    assert_ne!(first, second, "selecting another domain did not update its managed list/details");
    for y in 0..480
    {
        for x in 0..640
        {
            if !(24..616).contains(&x) || !(88..391).contains(&y)
            {
                let at = (y * 640 + x) * 3;
                assert_eq!(first[at..at + 3],
                           second[at..at + 3],
                           "domain selection damaged unrelated pixel {x},{y}");
            }
        }
    }
    assert_eq!(second,
               frame(&output.join("refreshed.ppm")),
               "refresh did not preserve the selected incarnation");
    assert_ne!(second, frame(&output.join("resized.ppm")), "managed panes did not resize");
    let scrolled = frame(&output.join("scrolled-second.ppm"));
    assert_ne!(scrolled, second, "detail scrollbar did not expose another part of the ledger");
    assert!(scrolled == frame(&output.join("scrolled-second-again.ppm")),
            "switching domains while scrolled reused another owner's cached rows");
    assert!(scrolled == frame(&output.join("scrolled-refreshed.ppm")),
            "refreshing a scrolled domain changed its authoritative detail rows");

    let module = frame(&output.join("selected-module.ppm"));
    assert_ne!(module, second,
               "selecting a loaded module did not change the managed domain/details panes");
    assert_eq!(module,
               frame(&output.join("module-refreshed.ppm")),
               "refresh did not preserve the selected module incarnation");

    // The native All-domain read above is admitted by this exact resource
    // content, not its filename/creator. Shared codec tests cover tampering.
    let dist = mac_build::paths::dist_dir(&root);
    let system =
        mac_formats::resource::decode(&fs::read(dist.join("System.rsrc")).unwrap()).unwrap();
    let application =
        mac_formats::resource::decode(&fs::read(dist.join("Reactor Room.rsrc")).unwrap()).unwrap();
    let policy = system.iter()
                       .find(|r| r.kind == mac_formats::authority::RESOURCE_KIND && r.id == 0)
                       .unwrap();
    assert_eq!(mac_formats::authority::granted(&policy.data,mac_formats::authority::image_digest(&application).unwrap()).unwrap(),mac_formats::authority::INSPECT_DOMAINS);
    fs::write(output.join("rust-acceptance.json"),b"{\"result\":\"pass\",\"coverage\":\"process/module-domain view, policy grant, selection damage, resize and quit\"}\n").unwrap();
}
