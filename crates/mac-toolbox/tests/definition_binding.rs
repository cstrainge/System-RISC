//! Definition author promises through the public SDK and actual loaded body.
#![cfg(not(target_arch = "riscv64"))]
use mac_toolbox::{Point, Rect, System,
                  controls::{StandardKind, StandardState},
                  quickdraw::SurfacePort};
#[test]
fn static_text_is_transparent_uses_theme_roles_and_stays_in_its_own_bounds()
{
    mac_system::testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let palette = mac_toolbox::quickdraw::theme::current(system.client()).unwrap();
        let background = 0x314159;
        let width = 96;
        let height = 32;
        let bounds = Rect::new(4, 4, 84, 15); // Deliberately clips a glyph's descent.
        for (text, enabled) in [("", true), ("Glyph gj", true), ("Disabled", false)]
        {
            let mut state = StandardState::new(bounds, text);
            state.maximum = 0;
            state.enabled = u32::from(enabled);
            let mut actual = vec![background; width * height];
            system.controls()
                  .draw_standard_pixels(&mut actual, width, height, StandardKind::StaticText, state)
                  .unwrap();
            let mut expected = vec![background; width * height];
            let metrics = system.quickdraw().fonts().metrics(state.font).unwrap();
            let mut surface = mac_abi::Surface { pixels: expected.as_mut_ptr(),
                                                 width:  width as u32,
                                                 height: height as u32,
                                                 origin: Point::default(),
                                                 clip:   bounds };
            let connection =
                unsafe { mac_traps::SurfaceConnection::new(&mut surface, services.trap) };
            let mut port = SurfacePort::new(connection);
            port.font_text_clipped(state.font,
                                   Point { h: bounds.left,
                                           v: bounds.top + metrics.ascent },
                                   text,
                                   bounds.right - bounds.left,
                                   if enabled { palette.text } else { palette.disabled_text })
                .unwrap();
            assert_eq!(actual, expected, "static text may paint only its clipped themed glyphs");
            assert_eq!(actual[0], background);
        }
    });
}
#[test]
fn failed_definition_validation_publishes_no_control_or_ledger_entry()
{
    use mac_system::testing::{files::with_owner, inspection as fixture};
    mac_system::testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let owner = fixture::add("Definition refusal", 0);
        with_owner(owner, || {
            let snapshot =
                system.inspection().snapshot(mac_toolbox::inspection::Scope::Own).unwrap();
            let before = snapshot.at(0).unwrap();
            let mut state = StandardState::new(Rect::new(2, 2, 80, 20), "Missing font");
            state.maximum = 0;
            state.font.family = i16::MAX;
            let error =
                system.controls()
                      .create_standard(mac_toolbox::WindowId(0), StandardKind::StaticText, state)
                      .err()
                      .unwrap();
            assert_eq!(error, mac_toolbox::Error::NotFound);
            assert!(error.to_string().contains("not found"));
            let after = snapshot.at(0).unwrap();
            assert_eq!((after.live, after.pending, after.births, after.kinds),
                       (before.live, before.pending, before.births, before.kinds));
            let valid = system.controls()
                              .create_static_text(mac_toolbox::WindowId(0),
                                                  Rect::new(2, 2, 80, 20),
                                                  "Retry works",
                                                  Default::default(),
                                                  mac_toolbox::controls::TextAlignment::Left)
                              .unwrap();
            drop(valid);
            assert_eq!(snapshot.at(0).unwrap().pending, 0);
        });
    });
}


#[test]
#[ignore = "requires optimized image and private graphical definition fault VM"]
fn native_definition_panic_reports_the_active_resource_and_shuts_down_loudly()
{
    use std::{fs, path::PathBuf, process::Command};
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("definition-panic-{}", std::process::id()));
    let capture = Command::new("python3").arg(root.join("System7/scripts/\
                                                         capture_system7_definition_panic.py"))
                                         .arg("--output")
                                         .arg(&output)
                                         .current_dir(&root)
                                         .output()
                                         .unwrap();
    assert!(capture.status.success(),
            "definition panic transport failed:\n{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    assert_eq!(report["shutdown_observed"], true, "panic hung instead of shutting down:\n{serial}");
    assert_eq!(report["exit_code"], 1, "fatal definition must signal guest failure");
    assert_eq!(report["original_before"], report["original_after"],
               "probe mutated base installation");
    for marker in ["DEFINITION_PANIC_FATAL: kind=CDEF id=-32759",
                   "DEFINITION_PANIC_PROBE: safe author body reached", "src/main.rs:",
                   "SYSTEM_SHUTDOWN: fatal privileged fault; recovery is not proven"]
    {
        assert!(serial.contains(marker), "missing diagnostic {marker}:\n{serial}");
    }
    assert!(!serial.contains("REACTOR_READY"),
            "a failed definition invocation must not resume the application");
    fs::write(output.join("rust-acceptance.json"), serde_json::to_vec_pretty(&serde_json::json!({"result":"pass","coverage":"safe definition panic; authenticated invocation identity; abort-only M-D04 shutdown; no privileged unwind claim","capture":report})).unwrap()).unwrap();
}
