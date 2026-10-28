//! Definition author promises through the public SDK and actual loaded body.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/native_capture.rs"]
mod native_capture;

#[path = "support/display.rs"]
mod display;

#[test]
#[ignore = "requires current optimized image and private terminal fault VM"]
fn native_definition_panic_shows_original_bomb_and_keyboard_restart()
{
    terminal("definition");
}
#[test]
#[ignore = "requires current optimized image and private terminal fault VM"]
fn resident_panic_with_allocator_and_desktop_locked_accepts_pointer_restart()
{
    terminal("system-panic");
}
#[test]
#[ignore = "requires current optimized image and private terminal fault VM"]
fn real_allocation_failure_with_allocator_locked_accepts_keyboard_restart()
{
    terminal("oom");
}
fn terminal(scenario: &str)
{
    use std::fs;
    let run =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let output =
        native_capture::build().join(format!("terminal-{scenario}-{}-{run}", std::process::id()));
    let capture = native_capture::command("capture_system7_definition_panic.py").arg("--output")
                                                                                .arg(&output)
                                                                                .arg("--scenario")
                                                                                .arg(scenario)
                                                                                .output()
                                                                                .unwrap();
    assert!(capture.status.success(),
            "terminal transport failed (inspect capture.json/cleanup.json):\n{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert_eq!(report["result"], "transport-complete", "{report}");
    assert_eq!(report["original_before"], report["original_after"], "base media changed");
    assert_eq!(report["cleanup"]["quarantined"], false);
    assert_eq!(report["cleanup"]["errors"], serde_json::json!([]));
    let owners = report["cleanup"]["owners"].as_array().unwrap();
    assert_eq!(owners.len(), 1);
    assert_eq!(owners[0]["reaped"], true);
    assert_eq!(owners[0]["exit_code"], 0);
    assert_eq!(report["scenario"], scenario);
    assert_eq!(report["terminal"]["kind"],
               if scenario == "oom" { "OutOfMemory" } else { "SystemPanic" });
    assert_eq!(report["terminal"]["flags"], 0,
               "short diagnostics should fit the pinned record: {serial}");
    assert_eq!(report["mutation"]["private_system"], scenario != "definition");
    let unchanged = report["mutation"]["unchanged"].as_u64().unwrap();
    let replaced = report["mutation"]["replaced"].as_u64().unwrap();
    assert!(unchanged > 0);
    assert_eq!(unchanged + replaced, report["mutation"]["resource_count"].as_u64().unwrap());
    assert_eq!(replaced,
               if scenario == "definition" { 1 } else { 3 },
               "only CDEF plus RV64/NABI should change");
    for text in ["DEFINITION_PANIC_FATAL: kind=CDEF id=-32759",
                 "DEFINITION_PANIC_PROBE: safe author body reached",
                 "ordinary_execution=abandoned", "SYSTEM_EMERGENCY_DIAGNOSTIC ",
                 "SYSTEM_EMERGENCY_RESTART_REQUEST"]
    {
        assert!(serial.contains(text), "missing {text}:\n{serial}");
    }
    // PanicInfo may put the message after its source-location line. Preserve
    // the complete terminal diagnostic block through the Restart checkpoint.
    let diagnostic = serial.split_once("SYSTEM_EMERGENCY_DIAGNOSTIC ")
                           .expect("terminal diagnostic record missing")
                           .1
                           .split_once("SYSTEM_EMERGENCY_RESTART_REQUEST")
                           .expect("terminal diagnostic never reached Restart")
                           .0;
    match scenario
    {
        "definition" =>
        {
            assert!(diagnostic.contains("Definition panic: CDEF -32759"), "{diagnostic}");
            assert!(serial.contains("src/main.rs:"), "panic source lost: {serial}");
        }
        "system-panic" =>
        {
            assert!(diagnostic.contains("EMERGENCY_QUALIFICATION_PANIC: allocator and ordinary \
                                         Desktop locks are held"),
                    "{diagnostic}");
            assert_eq!(report["restart_input"], "pointer");
        }
        "oom" =>
        {
            let refusal =
                serial.lines()
                      .find(|line| line.starts_with("EMERGENCY_QUALIFICATION_OOM_REFUSED "))
                      .expect("no real oversized allocator refusal");
            let number = |key: &str| {
                refusal.split_whitespace()
                       .find_map(|part| part.strip_prefix(key))
                       .unwrap()
                       .parse::<u64>()
                       .unwrap()
            };
            let bytes = number("bytes=");
            assert!(bytes > 4096);
            assert_eq!(number("align="), 4096);
            assert!(diagnostic.contains(&format!("Resident System allocation failed: {bytes} \
                                                  bytes, alignment 4096")),
                    "OOM request identity lost: {diagnostic}");
            assert!(diagnostic.contains("Allocator statistics are not read"));
            assert_eq!(report["restart_input"], "keyboard");
        }
        _ => unreachable!()
    }
    for error in ["SYSTEM_EMERGENCY_RECURSIVE", "SYSTEM_EMERGENCY_UNAVAILABLE",
                  "SYSTEM_EMERGENCY_PAINT_FAILED", "SYSTEM_EMERGENCY_CURSOR_FAILED",
                  "SYSTEM_EMERGENCY_DISPLAY_FAILED", "SYSTEM_EMERGENCY_INPUT_FAILED",
                  "SYSTEM_EMERGENCY_INPUT_RETIRED", "SYSTEM_SHUTDOWN:", "REACTOR_READY"]
    {
        assert!(!serial.contains(error),
                "terminal path failed or resumed its abandoned app: {error}\n{serial}");
    }
    assert_eq!(serial.matches("System RISC Boot ROM v1").count(),
               2,
               "Restart must reset hardware into ROM exactly once: {serial}");
    let second_boot = serial.rfind("System RISC Boot ROM v1").unwrap();
    assert!(serial[second_boot..].contains("DESKTOP_READY"),
            "second desktop never became usable: {serial}");
    assert!(!report["input"].as_array().unwrap().is_empty());
    let stages = report["stages"].as_array().unwrap();
    let frame = |name: &str| {
        let stage = stages.iter()
                          .find(|stage| stage["stage"] == name)
                          .unwrap_or_else(|| panic!("missing {name}: {report}"));
        let data = fs::read(output.join(stage["frame"].as_str().unwrap())).unwrap();
        assert!(data.starts_with(&display::header()));
        assert_eq!(data.len(), display::header().len() + display::width() * display::height() * 3);
        data
    };
    let baseline = frame("baseline");
    let moved = frame("cursor-moved");
    assert_eq!(baseline, frame("cursor-restored"), "cursor save-under failed");
    assert_eq!(baseline,
               frame("cancelled"),
               "Escape/Return release did not restore unpressed scene");
    let pressed = frame("pressed");
    let rect = |value: &serde_json::Value| -> [usize; 4] {
        value.as_array()
             .unwrap()
             .iter()
             .map(|v| v.as_u64().unwrap() as usize)
             .collect::<Vec<_>>()
             .try_into()
             .unwrap()
    };
    let button = rect(&report["terminal"]["restart"]);
    let panel = rect(&report["terminal"]["panel"]);
    let contains = |rect: [usize; 4], x: usize, y: usize| {
        x >= rect[0] && x < rect[2] && y >= rect[1] && y < rect[3]
    };
    assert!(panel[0] < panel[2]
            && panel[1] < panel[3]
            && panel[2] <= display::width()
            && panel[3] <= display::height());
    let arrow = include_bytes!("../../../system/assets/cursors/arrow.curs");
    let hotspot_y = u16::from_be_bytes([arrow[64], arrow[65]]) as usize;
    let hotspot_x = u16::from_be_bytes([arrow[66], arrow[67]]) as usize;
    let cursor_box = |index: usize| {
        let point = &report["cursor_positions"][index];
        let x = point[0].as_u64().unwrap() as usize - hotspot_x;
        let y = point[1].as_u64().unwrap() as usize - hotspot_y;
        [x, y, x + 16, y + 16]
    };
    let mut cursor_changes = 0;
    let mut button_changes = 0;
    for y in 0..display::height()
    {
        for x in 0..display::width()
        {
            let at = display::header().len() + (y * display::width() + x) * 3;
            if baseline[at..at + 3] != moved[at..at + 3]
            {
                cursor_changes += 1;
                assert!(contains(cursor_box(0), x, y) || contains(cursor_box(1), x, y),
                        "cursor motion repainted unrelated pixel {x},{y}");
            }
            if baseline[at..at + 3] != pressed[at..at + 3]
            {
                button_changes += 1;
                assert!(contains(button, x, y), "button press repainted unrelated pixel {x},{y}");
            }
        }
    }
    assert!(cursor_changes > 0 && cursor_changes <= 512);
    assert!(button_changes > 0);
    let bomb = include_bytes!("../../../system/assets/crashes/classic-bomb.icon");
    for y in 0..32
    {
        for x in 0..32
        {
            let at = display::header().len()
                     + ((panel[1] + 20 + y) * display::width() + panel[0] + 20 + x) * 3;
            let expected =
                if bomb[y * 4 + x / 8] & (0x80 >> (x & 7)) != 0 { [0; 3] } else { [255; 3] };
            assert_eq!(baseline[at..at + 3], expected, "original bomb differs at {x},{y}");
        }
    }
    if scenario == "system-panic"
    {
        assert_eq!(baseline,
                   frame("pointer-cancelled"),
                   "release outside restarted or left button pressed");
        assert_ne!(baseline, frame("pointer-pressed"));
    }
    assert_ne!(baseline, frame("rebooted"));
    fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&serde_json::json!({"result":"pass","scenario":scenario,"coverage":"boot-pinned terminal reporting and hardware Restart, not privileged unwind or general emergency-service reserve","original_bomb_pixels":1024,"cursor_changed_pixels":cursor_changes,"button_changed_pixels":button_changes,"capture":report})).unwrap()).unwrap();
}
