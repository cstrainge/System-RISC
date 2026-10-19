//! Real Finder input routing: no disk work per key, and native selection chords.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs, path::PathBuf, process::Command};

use serde_json::Value;
#[test]
#[ignore = "requires cargo xtask image and QEMU; two private disk boots"]
fn finder_typing_avoids_disk_work_and_preserves_navigation_selection()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join(format!("results/system7/typing-{}", std::process::id()));
    let windows = std::env::var_os("SYSTEM7_TYPING_WINDOWS").is_some();
    let mut command = Command::new("python3");
    if windows
    {
        command.arg("-u");
    }
    command.arg(root.join("System7/scripts/capture_system7_typing.py"))
           .arg("--output")
           .arg(&output);
    if windows
    {
        command.arg("--windows-headless");
    }
    let capture = command.output().unwrap();
    assert!(capture.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    let profile = fs::read_to_string(output.join("profile-vm/serial.log")).unwrap();
    assert!(!profile.contains("TYPING_PROFILE_FAIL"), "{profile}");
    assert!(profile.contains("TRAPS_READY: resident ecall dispatcher ABI=5"));
    let fields = profile.lines().find_map(|line| line.strip_prefix("TYPING_PROFILE ")).unwrap();
    let field = |name: &str| {
        fields.split_whitespace()
              .find_map(|f| f.strip_prefix(name))
              .unwrap()
              .parse::<u64>()
              .unwrap()
    };
    assert_eq!(field("chars="), 20);
    assert_eq!(field("file_calls="),
               0,
               "typing/release must not query disk-backed browser policy: {fields}");
    let states = profile.lines()
                        .filter(|line| line.starts_with("FINDER_TEXTEDIT_STATE "))
                        .collect::<Vec<_>>();
    assert_eq!(states.last().unwrap(), &"FINDER_TEXTEDIT_STATE length=149 selection=149..149");
    assert_eq!(states.len(), 21, "each keyDown edits once; keyUp must not edit");
    let finder = fs::read_to_string(output.join("finder-vm/serial.log")).unwrap();
    assert!(!finder.contains("FINDER_ACTION_FAILED"), "{finder}");
    assert!(finder.contains("FINDER_MENU menu=1257 item=1 command=7465646d"));
    assert!(!finder.lines().filter_map(|line| line.split("dropped=").nth(1)).any(|n| {
                                                                                n.parse::<u64>()
                                                                                 .unwrap()
                                                                                != 0
                                                                            }),
            "{finder}");
    let steps = report["steps"].as_array().unwrap();
    for (name, length, a, b) in [("burst", 20, 20, 20),
                                 ("multiline", 11, 11, 11),
                                 ("home", 11, 8, 8),
                                 ("shift-end", 11, 8, 11),
                                 ("shift-home-reverse", 11, 8, 8),
                                 ("ctrl-home", 11, 0, 0),
                                 ("ctrl-shift-end", 11, 0, 11),
                                 ("ctrl-shift-home-reverse", 11, 0, 0),
                                 ("right-one", 11, 1, 1),
                                 ("right-two", 11, 2, 2),
                                 ("shift-down", 11, 2, 6),
                                 ("shift-down-again", 11, 2, 10),
                                 ("shift-up", 11, 2, 6),
                                 ("shift-up-reverse", 11, 2, 2),
                                 ("shift-left", 11, 1, 2),
                                 ("shift-right-reverse", 11, 2, 2),
                                 ("end", 11, 3, 3),
                                 ("shift-home", 11, 0, 3),
                                 ("shift-end-reverse", 11, 3, 3),
                                 ("ctrl-end", 11, 11, 11),
                                 ("shift-home-backward", 11, 8, 11),
                                 ("shift-end-backward-reverse", 11, 11, 11),
                                 ("shift-home-again", 11, 8, 11),
                                 ("ctrl-shift-home", 11, 0, 11),
                                 ("ctrl-shift-end-reverse", 11, 11, 11)]
    {
        let step = steps.iter().find(|s| s["name"] == name).unwrap();
        assert!(step["log"].as_str()
                           .unwrap()
                           .contains(&format!("FINDER_TEXTEDIT_STATE length={length} \
                                               selection={a}..{b}\n")),
                "{name}: {step}");
    }
    // Timings are observations, not a machine-dependent correctness threshold.
    let acceptance = serde_json::json!({"status":"passed","file_calls_per_key":0,"characters":20,"native_navigation_cases":49,"physical_navigation_cases":23,"edit_ticks":field("edit_ticks="),"menu_ticks":field("menu_ticks="),"burst":report["burst"],"capture":report,"output":output});
    fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
    fs::write(root.join(if windows
                        {
                            "results/system7/typing-windows-checks.json"
                        }
                        else
                        {
                            "results/system7/typing-checks.json"
                        }),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
}
