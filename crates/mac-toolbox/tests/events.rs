//! Canonical public SDK tests, entering the actual resident host service gate.
#![cfg(not(target_arch = "riscv64"))]
mod support;
#[allow(dead_code)]
mod conformance;

#[test]
fn original_os_queue_and_native_application_queue_contracts()
{
    support::with_system(|system| conformance::events::run(system).unwrap());
}

#[test]
fn compact_event_observations_match_the_frozen_original_capture()
{
    let captured: serde_json::Value =
        serde_json::from_str(include_str!("data/events/original-cases.json")).unwrap();
    // The no-std guest has the same captured values without a JSON parser.
    let compact = include_str!("data/events/observations.rs");
    for row in captured.as_array().unwrap().iter().filter(|row| row["group"] == "events")
    {
        let expected = format!("(\"{}\", {}, {}, {}, {})",
                               row["action"].as_str().unwrap(),
                               row["status"],
                               row["found"],
                               row["what"],
                               row["message"]);
        assert!(compact.contains(&expected), "compact capture omitted {expected}");
    }
    let captured: serde_json::Value =
        serde_json::from_str(include_str!("data/events/original-mouse-queue.json")).unwrap();
    let compact = include_str!("data/events/mouse-observations.rs");
    for row in captured.as_array().unwrap()
    {
        let expected = format!("(\"{}\", {}, {})",
                               row["case"].as_str().unwrap(),
                               row["button"],
                               row["still_down"]);
        assert!(compact.contains(&expected), "compact mouse capture omitted {expected}");
    }
}

#[test]
#[ignore = "requires fresh cargo xtask image and qemu-system-riscv64"]
fn physical_input_uses_real_ecalls_and_keeps_raw_events_during_menu_dispatch()
{
    use std::{fs, path::PathBuf, process::Command};
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join("results/system7").join(format!("events-input-{}", std::process::id()));
    let transport = Command::new("python3").arg(root.join("System7/scripts/\
                                                           capture_system7_events.py"))
                                           .arg("--output")
                                           .arg(&output)
                                           .current_dir(&root)
                                           .output()
                                           .unwrap();
    assert!(transport.status.success(),
            "input transport failed:\n{}\n{}",
            String::from_utf8_lossy(&transport.stdout),
            String::from_utf8_lossy(&transport.stderr));
    let capture: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(capture["original_before"], capture["original_after"]);
    let states = capture["snapshots"].as_array().unwrap();
    let state = |name: &str| &states.iter().find(|row| row["name"] == name).unwrap()["state"];
    let original: serde_json::Value =
        serde_json::from_str(include_str!("data/events/original-input.json")).unwrap();
    let original = original.as_array().unwrap();
    // A and Shift+A compare complete original KeyMap and modifiers. Original
    // ADB device-address bits16..23 are outside this virtio device profile.
    for (name, keys) in [("a-held", "01000000000000000000000000000000"),
                         ("shift-a-held", "01000000000000010000000000000000")]
    {
        let reference = original.iter().find(|row| row["keys"] == keys).unwrap();
        assert_eq!(state(name)["keys"], reference["keys"]);
        assert_eq!(state(name)["modifiers"], reference["event_modifiers"]);
    }
    for name in ["idle", "a-up", "shift-a-up", "command-a-up", "mouse-up"]
    {
        assert_eq!(state(name)["keys"], "00000000000000000000000000000000");
        assert_eq!(state(name)["modifiers"], 128);
    }
    // Ctrl is the documented host adapter for Mac Command. Original command
    // capture establishes byte6 bit7; combining it with A is a native case.
    let command_reference =
        original.iter().find(|row| row["keys"] == "00000000000080000000000000000000").unwrap();
    assert_eq!(command_reference["event_modifiers"], 384);
    assert_eq!(state("command-a-held")["keys"], "01000000000080000000000000000000");
    assert_eq!(state("command-a-held")["modifiers"], command_reference["event_modifiers"]);
    assert_eq!(capture["menu_commands"], 1, "resource menu shortcut was not routed");
    let os = capture["os_events"].as_array().unwrap();
    assert!(os.iter().any(|event| {
                         event["kind"] == 3 && event["message"] == 97 && event["modifiers"] == 384
                     }),
            "menu routing swallowed the raw physical key event");
    for reference in
        original.iter().filter(|row| row["found"] == true && (row["what"] == 3 || row["what"] == 4))
    {
        let kind = &reference["what"];
        let character = reference["message"].as_u64().unwrap() & 0xffff;
        let modifiers = &reference["event_modifiers"];
        assert!(os.iter().any(|event| {
                             event["kind"] == *kind
                             && event["message"] == character
                             && event["modifiers"] == *modifiers
                         }),
                "missing original mapped physical key transition {kind}/{character}/{modifiers}");
    }
    for (name, down) in [("mouse-held", true), ("mouse-up", false)]
    {
        let reference =
            original.iter()
                    .find(|row| {
                        row["button"] == down && row["global_x"] == 241 && row["global_y"] == 180
                    })
                    .unwrap();
        assert_eq!(state(name)["x"], reference["global_x"]);
        assert_eq!(state(name)["y"], reference["global_y"]);
        assert_eq!(state(name)["modifiers"], reference["event_modifiers"]);
    }
    assert!(os.iter().any(|event| {
                         event["kind"] == 1
                         && event["x"] == 241
                         && event["y"] == 180
                         && event["modifiers"] == 0
                     }));
    assert!(os.iter().any(|event| event["kind"] == 2 && event["modifiers"] == 128));
    let still = &capture["still_down"].as_array().unwrap()[0];
    let first_down =
        original.iter()
                .find(|row| row["button"] == true && row["what"] == 1 && row["still_down"] == true)
                .unwrap();
    let queued_up =
        original.iter()
                .find(|row| row["button"] == true && row["what"] == 2 && row["still_down"] == false)
                .unwrap();
    let held_empty =
        original.iter()
                .find(|row| {
                    row["button"] == true && row["found"] == false && row["still_down"] == true
                })
                .unwrap();
    assert_eq!(still["before"], first_down["still_down"]);
    assert_eq!(still["first"], first_down["what"]);
    assert_eq!(still["after"], queued_up["still_down"]);
    assert_eq!(still["physical"], queued_up["button"]);
    assert_eq!(still["peek_available"], true, "StillDown consumed queued mouse-up");
    assert_eq!(still["peek"], queued_up["what"]);
    assert_eq!(still["second"], queued_up["what"]);
    assert_eq!(still["final"], held_empty["still_down"]);
    let mut previous = 0;
    for sample in states
    {
        let sample = &sample["state"];
        let uptime = sample["uptime"].as_u64().unwrap();
        assert!(uptime >= previous);
        assert_eq!(sample["ticks"].as_u64().unwrap(), uptime & u32::MAX as u64);
        previous = uptime;
    }
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert!(serial.contains("TRAPS_READY: resident ecall dispatcher ABI=5"));
    let acceptance = serde_json::to_vec_pretty(&serde_json::json!({"status":"passed","acceptance":"public Event Manager ECALL physical state, raw queue and resource menu composition","capture":capture})).unwrap();
    fs::write(output.join("rust-acceptance.json"), &acceptance).unwrap();
    fs::write(root.join("results/system7/event-manager-checks.json"), &acceptance).unwrap();
}
