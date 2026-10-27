//! Real INIT/System0 -> VBYT -> virtio capability exchange. This gate does not
//! qualify production worker scheduling or OS-host clipboard text sharing.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/native_capture.rs"]
mod native_capture;
use std::{fs,
          path::{Path, PathBuf}};

#[test]
#[ignore = "requires optimized private image and QEMU with builtin qemu-vdagent; no graphical host \
            peer"]
fn native_vdagent_transport_exchanges_capabilities_and_refuses_bad_endpoints()
{
    let build = native_capture::build();
    assert!(build.join("dist/system7.qcow2").is_file(),
            "build a private optimized image and select it with SYSTEM7_BUILD_DIR");
    let output = build.join("checks").join(format!("vdagent-native-{}", std::process::id()));
    let result = native_capture::command("capture_vdagent_transport.py").arg("--output")
                                                                        .arg(&output)
                                                                        .output()
                                                                        .unwrap();
    assert!(result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr));
    assert_capture(&output);
}
#[test]
#[ignore = "validates a prepared private native vdagent transport capture"]
fn prepared_vdagent_transport_capture_passes_native_acceptance()
{
    let selected=std::env::var_os("SYSTEM7_VDAGENT_TRANSPORT_CAPTURE")
        .expect("SYSTEM7_VDAGENT_TRANSPORT_CAPTURE must name a completed private capture");
    let output = PathBuf::from(selected);
    assert_capture(&output);
}
#[test]
#[ignore = "requires a prepared isolated Xvfb/GTK native transport text capture"]
fn prepared_private_x11_vdagent_text_roundtrip_passes_native_acceptance()
{
    let selected=std::env::var_os("SYSTEM7_VDAGENT_PRIVATE_X11_CAPTURE")
        .expect("SYSTEM7_VDAGENT_PRIVATE_X11_CAPTURE must name a fresh owned private Xvfb capture");
    let output = PathBuf::from(selected);
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    let case = &report["cases"]["private-x11"];
    assert_eq!(case["termination"], "desktop-ready", "{case}");
    assert_eq!(case["host_display_environment_removed"], true);
    assert_eq!(case["cleanup_errors"], serde_json::json!([]));
    let text = &case["private_text"];
    assert_eq!(text["scope"], "fresh-owned-Xvfb-only");
    assert!(text["display"].as_str().is_some_and(|display| display.starts_with("127.0.0.1:")));
    assert!(text["server_pid"].as_u64().is_some_and(|pid| pid > 0));
    assert_eq!(text["gtk_clipboard_option"], true,
               "positive GTK qualification requires the selected QEMU's declared runtime \
                clipboard facility: {case}");
    assert!(text["host_utf8_requests"].as_u64().is_some_and(|requests| requests > 0));
    assert_eq!(text["errors"], serde_json::json!([]));
    assert_eq!(text["relay_errors"], serde_json::json!([]));
    assert!(text["host_target_requests"].as_u64().is_some_and(|requests| requests > 0),
            "QEMU GTK did not observe the private selection owner; use a QEMU build declaring \
             DisplayGTK.clipboard and enable clipboard=on before claiming a text roundtrip: \
             {case}");
    let hex =
        |value: &str| value.as_bytes().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    assert_eq!(text["host_text_utf8_hex"], hex("Private host café 🦀\nsecond line"));
    assert_eq!(text["guest_text_utf8_hex"], hex("Private guest 雪 🦀\nsecond line"));
    let serial = fs::read_to_string(output.join("private-x11/vm/serial.log")).unwrap();
    assert!(serial.contains("VDAGENT_NATIVE_POLICY engine=mac-host-clipboard \
                             snapshots=private-fixture-only resident_grants=false"),
            "{serial}");
    assert!(serial.contains("VDAGENT_NATIVE_CAPABILITIES_PASS "), "{serial}");
    assert!(serial.contains("VDAGENT_NATIVE_PRIVATE_HOST_TO_GUEST_PASS "), "{serial}");
    assert!(serial.contains("utf8=passed multiline=passed emoji=passed"), "{serial}");
    assert!(serial.contains("VDAGENT_NATIVE_PRIVATE_GUEST_TEXT_SENT "), "{serial}");
    assert!(serial.contains("MODULE_PUBLISHED name=VDAgent Transport Probe "), "{serial}");
    for failure in ["VDAGENT_NATIVE_FAIL", "VDAGENT_NATIVE_TIMEOUT", "VDAGENT_NATIVE_CODEC_ERROR",
                    "VDAGENT_NATIVE_PRIVATE_TEXT_FAIL", "VDAGENT_NATIVE_CANCEL_RETAINED",
                    "SYSTEM_PANIC:", "SYSTEM_TRAP:", "DRIVER_PANIC_FATAL:"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
    for folder in [output.join("private-x11/dist"), output.join("private-x11/vm")]
    {
        for entry in fs::read_dir(folder).unwrap()
        {
            let path = entry.unwrap().path();
            assert!(!matches!(path.extension().and_then(|e| e.to_str()),
                              Some("raw" | "qcow2" | "hfs" | "hfsb" | "img")),
                    "private disk retained: {}",
                    path.display());
        }
    }
    let acceptance = serde_json::json!({"status":"passed","scope":"private-X11-native-transport-text",
        "host_to_guest":"utf8-multiline-emoji-passed","guest_to_host":"utf8-multiline-emoji-passed",
        "production_worker":"unqualified","Scrap_bridge":"unqualified"});
    fs::write(output.join("rust-private-x11-acceptance.json"),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
}
#[test]
#[ignore = "validates the explicit native refusal when installed QEMU GTK has no operational \
            clipboard peer"]
fn prepared_private_x11_peer_refusal_keeps_local_desktop_usable()
{
    let selected=std::env::var_os("SYSTEM7_VDAGENT_PRIVATE_X11_CAPTURE")
        .expect("SYSTEM7_VDAGENT_PRIVATE_X11_CAPTURE must name a fresh private Xvfb capture");
    let output = PathBuf::from(selected);
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    let case = &report["cases"]["private-x11"];
    assert_eq!(case["termination"], "desktop-ready", "{case}");
    assert_eq!(case["host_display_environment_removed"], true);
    assert_eq!(case["cleanup_errors"], serde_json::json!([]));
    let text = &case["private_text"];
    assert_eq!(text["scope"], "fresh-owned-Xvfb-only");
    assert_eq!(text["host_reoffered_after_caps"], true, "{case}");
    assert_eq!(text["host_target_requests"], 0,
               "this gate specifically qualifies an absent GTK peer, not a failed data \
                conversion: {case}");
    assert_eq!(text["host_utf8_requests"], 0);
    assert_eq!(text["errors"], serde_json::json!([]));
    assert_eq!(text["relay_errors"], serde_json::json!([]));
    assert_eq!(text["guest_text_utf8_hex"], serde_json::Value::Null);
    let serial = fs::read_to_string(output.join("private-x11/vm/serial.log")).unwrap();
    assert!(serial.contains("VDAGENT_NATIVE_POLICY engine=mac-host-clipboard \
                             snapshots=private-fixture-only resident_grants=false"),
            "{serial}");
    assert!(serial.contains("VDAGENT_NATIVE_CAPABILITIES_PASS ")
            && serial.contains("engine=mac-host-clipboard scope=bootstrap-transport-only"),
            "{serial}");
    assert!(serial.contains("VDAGENT_NATIVE_PRIVATE_HOST_PEER_UNAVAILABLE detail=no private X11 \
                             offer arrived after negotiation; enable a working QEMU GTK \
                             clipboard peer and retry; guest clipboard remains local"),
            "{serial}");
    assert!(serial.contains("MODULE_PUBLISHED name=VDAgent Transport Probe ")
            && serial.contains("DESKTOP_READY"),
            "{serial}");
    for failure in ["VDAGENT_NATIVE_FAIL", "VDAGENT_NATIVE_TIMEOUT", "VDAGENT_NATIVE_CODEC_ERROR",
                    "VDAGENT_NATIVE_ENGINE_ERROR", "VDAGENT_NATIVE_CANCEL_RETAINED",
                    "SYSTEM_PANIC:", "SYSTEM_TRAP:", "DRIVER_PANIC_FATAL:"]
    {
        assert!(!serial.contains(failure), "{serial}");
    }
    let acceptance = serde_json::json!({"status":"passed","scope":"private-X11-native-host-peer-refusal",
        "engine_handshake":"passed","local_desktop":"usable","host_text_roundtrip":"unqualified",
        "production_worker":"unqualified","scrap_bridge":"unqualified"});
    fs::write(output.join("rust-private-x11-refusal-acceptance.json"),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
}
fn assert_capture(output: &Path)
{
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["scope"], "native-bootstrap-transport-only");
    assert_eq!(report["original_before"], report["original_after"]);
    for mode in ["capabilities", "wrong-name", "malformed", "absent"]
    {
        let case = &report["cases"][mode];
        assert_eq!(case["termination"], "desktop-ready", "{case}");
        assert_eq!(case["host_clipboard_peer"], false);
        assert_eq!(case["host_display_environment_removed"], true);
        assert_eq!(case["cleanup_errors"], serde_json::json!([]));
        let arguments = case["command"].as_array().unwrap();
        let display = arguments.iter().position(|value| value == "-display").unwrap();
        assert_eq!(arguments[display + 1], "none", "{case}");
        let serial = fs::read_to_string(output.join(mode).join("vm/serial.log")).unwrap();
        assert!(serial.contains("VDAGENT_NATIVE_BEGIN target=System resource=0 \
                                 execution=bounded-bootstrap fixture=true\n"),
                "{serial}");
        assert!(serial.contains("DESKTOP_READY"), "local desktop must remain usable: {serial}");
        assert!(serial.contains("MODULE_PUBLISHED name=VDAgent Transport Probe "), "{serial}");
        for failure in ["VDAGENT_NATIVE_FAIL", "VDAGENT_NATIVE_CANCEL_RETAINED", "SYSTEM_PANIC:",
                        "SYSTEM_TRAP:", "DRIVER_PANIC_FATAL:"]
        {
            assert!(!serial.contains(failure), "{serial}");
        }
        match mode
        {
            "capabilities" =>
            {
                assert!(serial.contains("VDAGENT_NATIVE_OPEN generation=1 \
                                         port=com.redhat.spice.0\n"),
                        "{serial}");
                assert!(serial.contains("VDAGENT_NATIVE_ANNOUNCE protocol=1 message=6 payload=8 \
                                         request_reply=1"),
                        "{serial}");
                assert!(serial.lines().any(|line| {
                                          line.starts_with("VDAGENT_NATIVE_CAPABILITIES_PASS ")
                                          && line.contains("protocol=1 message=6")
                                          && line.ends_with("scope=bootstrap-transport-only")
                                      }),
                        "{serial}");
                assert_eq!(case["native_channel"]["backend"], "qemu-vdagent");
                assert!(arguments.iter()
                                 .any(|argument| {
                                     argument == "qemu-vdagent,id=probeagent,clipboard=on,mouse=off"
                                 }),
                        "{case}");
            }
            "wrong-name" =>
            {
                assert!(serial.contains("VDAGENT_NATIVE_REFUSAL state=3"), "{serial}");
                assert!(serial.contains("configure its exact name and restart System")
                        && serial.contains("required port: com.redhat.spice.0"),
                        "{serial}");
                assert!(!serial.contains("VDAGENT_NATIVE_OPEN"), "{serial}");
            }
            "malformed" =>
            {
                assert!(serial.lines().any(|line| {
                                          line.starts_with("VDAGENT_NATIVE_MALFORMED detail=")
                                          && line.contains("received unsupported protocol 2")
                                          && line.contains("poisoned=true")
                                      }),
                        "{serial}");
                assert_eq!(case["private_peer"]["errors"], serde_json::json!([]));
                assert!(case["private_peer"]["sent_hex"].as_str().is_some_and(|s| !s.is_empty()));
                assert!(!serial.contains("VDAGENT_NATIVE_CAPABILITIES_PASS"), "{serial}");
            }
            "absent" =>
            {
                assert!(serial.contains("VDAGENT_NATIVE_ABSENT detail=optional VBYT service was \
                                         not advertised; guest clipboard stays local\n"),
                        "{serial}")
            }
            _ => unreachable!()
        }
        for folder in [output.join(mode).join("dist"), output.join(mode).join("vm")]
        {
            for entry in fs::read_dir(folder).unwrap()
            {
                let path = entry.unwrap().path();
                assert!(!matches!(path.extension().and_then(|e| e.to_str()),
                                  Some("raw" | "qcow2" | "hfs" | "hfsb" | "img")),
                        "private generated disk retained: {}",
                        path.display());
            }
        }
    }
    let acceptance = serde_json::json!({
        "status":"passed","assertion_owner":"mac-toolbox/tests/vdagent_transport.rs",
        "scope":"native-bootstrap-transport-only","host_clipboard_text":"unqualified",
        "production_worker":"unqualified","cases":4});
    fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
}
