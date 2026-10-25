//! Actual Finder launch and system app-switching interaction over physical input.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/display.rs"]
mod display;
use std::{fs,
          path::{Path, PathBuf},
          process::Command};

use serde_json::Value;
#[path = "support/process_trace.rs"]
mod process_trace;

fn frame(path: &Path) -> Vec<u8>
{
    let bytes = fs::read(path).unwrap();
    let header = display::header();
    assert!(bytes.starts_with(&header));
    assert_eq!(bytes.len(), header.len() + display::width() * display::height() * 3);
    bytes[header.len()..].to_vec()
}

fn region(bytes: &[u8], left: usize, top: usize, right: usize, bottom: usize) -> Vec<u8>
{
    (top..bottom).flat_map(|y| {
        bytes[(y * display::width() + left) * 3..(y * display::width() + right) * 3].iter().copied()
    })
    .collect()
}

/// Render a known resource through the public SDK, independently of Finder's
/// file lookup and the process menu's loaded-image lookup.
fn icon(root: &Path, package: &str, id: i16, size: usize) -> Vec<u8>
{
    icon_flags(root, package, id, size, Default::default())
}
fn icon_flags(root: &Path, package: &str, id: i16, size: usize,
              flags: mac_toolbox::quickdraw::icons::IconFlags)
              -> Vec<u8>
{
    use mac_toolbox::{Point, Rect, quickdraw::SurfacePort};
    let system = fs::read(mac_build::paths::dist_dir(root).join("System.rsrc")).unwrap();
    let application =
        fs::read(mac_build::paths::dist_dir(root).join(format!("{package}.rsrc"))).unwrap();
    let mut pixels = vec![0xffffff; size * size];
    mac_system::testing::with_resource_forks(&system, &application, |services| {
        let mut surface = mac_abi::Surface { pixels: pixels.as_mut_ptr(),
                                             width:  size as u32,
                                             height: size as u32,
                                             origin: Point::default(),
                                             clip:   Rect::new(0, 0, size as i16, size as i16) };
        let connection = unsafe { mac_traps::SurfaceConnection::new(&mut surface, services.trap) };
        SurfacePort::new(connection).plot_icon(mac_toolbox::ResourceScope::from_raw(mac_abi::resources::APPLICATION).unwrap(),
                                               id,
                                               Rect::new(0, 0, size as i16, size as i16),
                                               flags)
                                    .unwrap();
    }).unwrap();
    pixels.iter()
          .flat_map(|pixel| [(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8])
          .collect()
}

#[test]
#[ignore = "requires fresh cargo xtask image and an independent graphical QEMU"]
fn finder_launches_real_apps_and_application_menu_switches_hides_and_restores_them()
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("applications-ui-{}", std::process::id()));
    let windows = std::env::var_os("SYSTEM7_APPLICATION_UI_WINDOWS").is_some();
    let mut command = Command::new("python3");
    command.arg(root.join("System7/scripts/capture_system7_applications_ui.py"))
           .arg("--output")
           .arg(&output)
           .arg("--remap-icon")
           .current_dir(&root);
    if windows
    {
        command.arg("--windows-headless");
    }
    let capture = command.output().unwrap();
    assert!(capture.status.success(),
            "UI capture failed:\n{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"], "base OS artifacts changed");
    assert_eq!(report["owned_vm_exited"], true);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    let quickdraw_id = process_trace::launched(&serial, "QuickDraw Test").0;
    let first_editor_log = serial.split("PROCESS_EXIT app=").next().unwrap();
    let editor_id = process_trace::launched(first_editor_log, "Text Edit").0;
    for failure in ["SYSTEM_PANIC:", "SYSTEM_TRAP:", "SYSTEM_ALLOCATION_FAILED:", "FINDER_ERROR:",
                    "TEXT_EDIT_ERROR", "QUICKDRAW_TEST_ERROR", "PROCESS_ACTION_FAILED",
                    "APPLICATION_MENU_FAILED"]
    {
        assert!(!serial.contains(failure), "unexpected {failure}:\n{serial}");
    }
    assert!(serial.contains("TRAPS_READY: resident ecall dispatcher ABI=1"));
    let snapshots = report["snapshots"].as_array().unwrap();
    let observation = |name: &str| snapshots.iter().find(|value| value["name"] == name).unwrap();
    let image = |name: &str| frame(&output.join(observation(name)["frame"].as_str().unwrap()));
    let prefix =
        |name: &str| &serial[..observation(name)["serial_length"].as_u64().unwrap() as usize];
    let between = |before: &str, after: &str| {
        &serial[observation(before)["serial_length"].as_u64().unwrap() as usize
                ..observation(after)["serial_length"].as_u64().unwrap() as usize]
    };
    let editor_state = |name: &str| {
        prefix(name).lines().rev().find(|line| line.starts_with("FINDER_TEXTEDIT_STATE ")).unwrap()
    };
    let front = |name: &str| {
        mac_toolbox::WindowId(observation(name)["state"]["front"].as_str()
                                                                 .unwrap()
                                                                 .parse()
                                                                 .unwrap())
    };
    assert!(prefix("applications-folder").contains(" name=Applications\n"));
    let fixture: Value =
        serde_json::from_slice(&fs::read(output.join("icon-fixture.json")).unwrap()).unwrap();
    assert_eq!(fixture["icon_id"], 9433, "non-128 loaded-image icon fixture");
    let applications = image("applications-folder");
    for (package, left, top) in [("QuickDraw Test", 168, 90), ("Text Edit", 40, 142)]
    {
        assert_eq!(region(&applications, left, top, left + 32, top + 32),
                   icon(&root, package, 128, 32),
                   "Finder did not draw {package}'s own icon before launch");
    }
    let running = image("running-file-icons");
    assert_eq!(region(&image("running-app-info"), 38, 53, 70, 85),
               icon(&root, "QuickDraw Test", 128, 32),
               "Get Info must show the normal icon even while its application is running");
    assert_eq!(region(&running, 168, 90, 200, 122),
               icon_flags(&root,
                          "QuickDraw Test",
                          128,
                          32,
                          mac_toolbox::quickdraw::icons::IconFlags { open: true,
                                                                     ..Default::default() }),
               "running application's icon did not use the open transform");
    assert_eq!(region(&running, 168, 142, 200, 174),
               icon(&root, "QuickDraw Test", 128, 32),
               "unlaunched copy inherited the running application's mark");
    // The icon must already be normal before any Finder click or activation.
    assert_eq!(region(&image("quickdraw-quit"), 168, 90, 200, 122),
               icon(&root, "QuickDraw Test", 128, 32),
               "exit notification did not clear the running mark automatically");
    let closed = image("closed-file-icons");
    assert_eq!(region(&closed, 168, 90, 200, 122),
               icon(&root, "QuickDraw Test", 128, 32),
               "exiting application retained its running icon");
    let popup = image("three-app-menu");
    // The right-aligned popup sizes to its current resource titles. Renaming
    // TextEdit Exerciser to Text Edit legitimately changes its left edge.
    let popup_left =
        (display::width() - 240..display::width() - 10).find(|&x| {
                                                           (21..137).all(|y| {
                                                                        popup[(y * display::width()
                                                                               + x)
                                                                              * 3
                                                                              ..(y
                                                                                 * display::width()
                                                                                 + x)
                                                                                * 3
                                                                                + 3]
                                                                        == [0, 0, 0]
                                                                    })
                                                       })
                                                       .expect("application menu left border");
    for (package, id, top) in
        [("System", 3, 85), ("QuickDraw Test", 128, 103), ("Text Edit", 128, 121)]
    {
        assert_eq!(region(&popup, popup_left + 17, top, popup_left + 33, top + 16),
                   icon(&root, package, id, 16),
                   "application menu used the wrong icon for {package}");
    }
    for (frame, package, id) in [("finder-initial", "System", 3),
                                 ("quickdraw-launched", "QuickDraw Test", 128),
                                 ("textedit-abc", "Text Edit", 128)]
    {
        assert_eq!(region(&image(frame), display::width() - 30, 1, display::width() - 14, 17),
                   icon(&root, package, id, 16),
                   "application menu title did not follow foreground {package}");
    }
    assert!(prefix("quickdraw-launched").contains("FINDER_MENU menu=1252 item=2 \
                                                   command=736f7065\n"),
            "QuickDraw must launch from Finder's actual File > Open resource command");
    assert!(prefix("quickdraw-launched").contains("name=QuickDraw Test\n"));
    assert!(prefix("quickdraw-page-two").contains("QD_DEMO_READY page=2 transfer\n"));
    assert_ne!(region(&image("quickdraw-launched"), 48, 100, 575, 390),
               region(&image("quickdraw-page-two"), 48, 100, 575, 390),
               "launched QuickDraw application did not react to physical keyboard input");
    assert!(prefix("textedit-abc").contains("FINDER_TEXTEDIT_STATE length=3 selection=3..3\n"));
    let hidden = image("hide-others");
    let restored = image("show-all");
    // The original Hide Others removes Finder's desktop icons as well as windows.
    // Keep this patch clear of the editor's structure and the composited pointer.
    for y in 22..70
    {
        for x in 576..630
        {
            let expected = if (x + y) % 2 == 0 { 0x87 } else { 0xc0 };
            let at = (y * display::width() + x) * 3;
            assert_eq!(&hidden[at..at + 3],
                       &[expected; 3],
                       "Hide Others left another app's desktop pixels at{x},{y}");
        }
    }
    // QuickDraw's restored title overlaps the left part of the volume icon.
    // The right-hand slice remains visible beyond both application structures.
    assert!(region(&restored, 604, 22, 630, 70)
            == region(&image("finder-initial"), 604, 22, 630, 70),
            "Show All failed to restore Finder's visible desktop resources");
    assert!(region(&hidden, 120, 71, 600, 88) == region(&restored, 120, 71, 600, 88),
            "Show All changed foreground editor focus");
    assert_eq!(editor_state("background-editor-before-click"),
               "FINDER_TEXTEDIT_STATE length=3 selection=0..0");
    assert!(between("background-editor-before-click", "first-background-content-click")
            .contains(&format!("PROCESS_ACTIVE app={editor_id}\n")),
            "first background content click must activate the editor application");
    assert_eq!(editor_state("first-background-content-click"),
               "FINDER_TEXTEDIT_STATE length=3 selection=1..1",
               "activation click changed the caret before the Right-arrow probe");
    assert_eq!(editor_state("first-click-caret-reset"),
               "FINDER_TEXTEDIT_STATE length=3 selection=0..0");
    assert_eq!(editor_state("second-foreground-content-click"),
               "FINDER_TEXTEDIT_STATE length=3 selection=2..2",
               "second content click must place the caret before the Left-arrow probe");
    assert_eq!(editor_state("editor-caret-restored"),
               "FINDER_TEXTEDIT_STATE length=3 selection=3..3");
    assert!(!between("editor-caret-restored", "background-command-drag-released")
             .contains(&format!("PROCESS_ACTIVE app={quickdraw_id}\n")),
             "Command-drag activated a background application");
    for stage in ["background-command-drag-held", "background-command-drag-released",
                  "background-ordinary-drag-held"]
    {
        assert_eq!(front(stage),
                   front("editor-caret-restored"),
                   "background drag changed the front window at{stage}");
        assert!(region(&image(stage), 0, 0, 145, 20)
                == region(&image("editor-caret-restored"), 0, 0, 145, 20),
                "background drag changed the foreground app menus at{stage}");
    }
    assert!(region(&image("editor-caret-restored"), 50, 58, 590, 74)
            == region(&image("background-command-drag-released"), 60, 68, 600, 84),
            "Command-drag must move the inactive title without changing its appearance");
    assert!(!between("background-command-drag-released", "background-ordinary-drag-held")
             .contains(&format!("PROCESS_ACTIVE app={quickdraw_id}\n")),
             "ordinary DragWindow activated its background application before release");
    assert!(between("background-ordinary-drag-held", "background-ordinary-drag-released")
            .contains(&format!("PROCESS_ACTIVE app={quickdraw_id}\n")),
            "ordinary DragWindow must activate its application after accepted release");
    assert_eq!(front("background-ordinary-drag-released"), front("quickdraw-page-two"));
    assert!(region(&image("background-ordinary-drag-released"), 0, 0, 145, 20)
            == region(&image("quickdraw-page-two"), 0, 0, 145, 20),
            "accepted background drag did not restore the owning application's menu bar");
    assert!(prefix("quickdraw-resumed").contains("QD_DEMO_READY page=3 regions\n"));
    assert!(prefix("textedit-resumed").contains("FINDER_TEXTEDIT_STATE length=5 selection=5..5\n"),
            "application switching lost editor contents/selection or keyboard routing");
    assert!(prefix("textedit-quit").contains(&format!("PROCESS_EXIT app={editor_id}\n")));
    assert!(prefix("quickdraw-quit").contains(&format!("PROCESS_EXIT app={quickdraw_id}\n")));
    assert_eq!(prefix("textedit-relaunched").matches("TEXT_EDIT_READY\n").count(), 2);
    let original = process_trace::launched(&prefix("textedit-quit"), "Text Edit");
    let relaunched = process_trace::launched(&prefix("textedit-relaunched-input"), "Text Edit");
    assert!(original.0 != relaunched.0 && original.1 != relaunched.1,
            "relaunch must create fresh application and thread identities");
    assert!(prefix("textedit-relaunched-input").contains("FINDER_TEXTEDIT_STATE length=1 \
                                                          selection=1..1\n"));
    assert!(prefix("finder-responsive-after-exit").contains("name=Documents\n"),
            "Finder did not handle a real folder double-click after both applications exited");

    // The original Finder-only application-menu interior, border and row text.
    // Original Finder's menu icon differs from its bundled System FNDR family.
    // That slot is checked against the actual resource above; retain the original
    // comparison for menu layout/text and exclude the recorded cursor tail.
    let golden = include_bytes!("data/application-menu/01-finder-menu.rgb");
    assert_eq!(golden.len(), 103 * 84 * 3);
    let actual = image("finder-only-menu");
    let mut pixels = 0;
    let mut differences = Vec::new();
    for y in 20..103
    {
        for source_x in 533..634
        {
            let x = source_x + display::width() - 640;
            if (550..566).contains(&source_x) && (85..101).contains(&y)
               || (620..625).contains(&source_x) && y < 23
            {
                continue;
            }
            let at = (y * display::width() + x) * 3;
            let reference = ((y - 20) * 103 + source_x - 532) * 3;
            if actual[at..at + 3] != golden[reference..reference + 3]
            {
                differences.push((x, y));
            }
            pixels += 1;
        }
    }
    assert!(differences.is_empty(),
            "{} original application-menu pixel differences; first{:?}; capture{}",
            differences.len(),
            &differences[..differences.len().min(20)],
            output.display());
    let acceptance = serde_json::json!({"status":"passed", "capture":report,
        "acceptance":"mac-toolbox/tests/application_ui.rs", "original_application_menu_pixels":pixels,
        "original_pixel_differences":0,"masked_pixels":{"native_icon_slot":256,"recorded_cursor_tail":15},
        "finder_file_open_and_double_click_launch":"passed","independent_app_keyboard_input":"passed",
        "application_switching_and_editor_state":"passed","hide_others_and_show_all":"passed",
        "activation_click_does_not_click_through":"passed","second_content_click_delivered":"passed",
        "background_command_drag_preserves_foreground":"passed","ordinary_drag_activates_on_release":"passed",
        "resource_menu_quit_and_fresh_relaunch":"passed","finder_alive_after_exit":"passed",
        "application_icon_pixels":7680,"running_app_info_normal_icon":"passed","running_file_identity_icons":"passed","unlaunched_copy_unmarked":"passed","running_icon_cleared_on_exit":"passed","application_icons_before_launch":"passed",
        "application_menu_icon_rows_and_foreground_title":"passed","non_128_icon_bundle":"passed"});
    fs::write(output.join("rust-acceptance.json"), serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
    fs::write(root.join(if windows
                        {
                            "results/system7/application-ui-windows-checks.json"
                        }
                        else
                        {
                            "results/system7/application-ui-checks.json"
                        }),
              serde_json::to_vec_pretty(&acceptance).unwrap()).unwrap();
}
