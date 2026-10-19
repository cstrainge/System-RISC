//! Public userspace acceptance for resident TextEdit.
#![cfg(not(target_arch = "riscv64"))]
#[allow(dead_code)]
mod conformance;
mod support;
use conformance::textedit as cases;
#[test]
fn original_textedit_layout_editing_and_hit_testing()
{
    support::with_system(|system| {
        assert_eq!(cases::original(system).unwrap(), 356);
    });
}
#[test]
fn native_unicode_ids_and_failure_guarantees()
{
    support::with_system(|system| {
        assert_eq!(cases::native(system).unwrap(), 20);
    });
}

#[test]
fn legacy_text_convenience_traps()
{
    mac_system::testing::with_services(|services| {
        let system = unsafe { mac_toolbox::System::from_raw(services) }.unwrap();
        assert_eq!(cases::legacy(&system, services.trap).unwrap(), 6);
    });
}
#[test]
fn native_limits_and_failed_paste_preserve_both_managers()
{
    support::with_system(|system| {
        use mac_toolbox::{Error, NO_MEMORY, NOT_FOUND,
                          textedit::{MAX_BYTES, MAX_EDITORS}};
        let te = system.textedit();
        let rect = mac_toolbox::Rect::new(0, 0, 100, 100);
        let id = te.create(rect, rect).unwrap();
        let maximum = "a".repeat(MAX_BYTES);
        te.set_text(id, &maximum).unwrap();
        let before = te.info(id).unwrap();
        system.scrap().clear().unwrap();
        system.scrap().put_text("é").unwrap();
        assert_eq!(te.paste(id), Err(Error(NO_MEMORY)));
        assert_eq!(te.info(id).unwrap(), before);
        let mut bytes = [0; 16];
        assert_eq!(system.scrap().text(&mut bytes).unwrap(), "é");
        assert_eq!(te.insert(id, "!"), Err(Error(NO_MEMORY)));
        assert_eq!(te.info(id).unwrap(), before);
        te.dispose(id).unwrap();
        assert_eq!(te.dispose(id), Err(Error(NOT_FOUND)));
        let ids: Vec<_> = (0..MAX_EDITORS).map(|_| te.create(rect, rect).unwrap()).collect();
        assert_eq!(te.create(rect, rect), Err(Error(NO_MEMORY)));
        for id in ids
        {
            te.dispose(id).unwrap();
        }
    });
}
#[test]
fn frozen_layout_and_edit_oracle_matches_original_capture()
{
    let original: serde_json::Value =
        serde_json::from_str(include_str!("data/textedit/original.json")).unwrap();
    let original = original.as_array().unwrap();
    for case in cases::LAYOUT
    {
        let row = original.iter()
                          .find(|r| r["case"] == case.name && r["operation"] == "set_text")
                          .unwrap();
        assert_eq!(row["text_hex"].as_str().unwrap(),
                   case.text.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>());
        assert_eq!(row["line_starts"], serde_json::json!(case.starts));
        assert_eq!(row["points"], serde_json::json!(case.points));
    }
    for case in cases::EDITS
    {
        let row =
            original.iter().find(|r| r["case"] == case.name && r["operation"] == "key").unwrap();
        assert_eq!(row["text_hex"].as_str().unwrap(),
                   case.text.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>());
        assert_eq!(row["selection"], serde_json::json!(case.selection));
    }
}

#[test]
fn native_navigation_extends_reverses_and_preserves_vertical_column()
{
    support::with_system(|system| {
        assert_eq!(cases::navigation(system).unwrap(), 49);
    });
}

#[test]
fn native_vertical_movement_works_beyond_quickdraw_coordinate_range()
{
    support::with_system(|system| {
        use mac_toolbox::textedit::CursorMovement::{Down, Up};
        let te = system.textedit();
        let r = mac_toolbox::Rect::new(0, 0, 100, 100);
        let id = te.create(r, r).unwrap();
        te.set_text(id, &"abc\r".repeat(3000)).unwrap();
        te.select(id, 4 * 2500 + 2, 4 * 2500 + 2).unwrap();
        let info = te.move_cursor(id, Down, true).unwrap();
        assert_eq!((info.selection_start, info.selection_end), (10002, 10006));
        let info = te.move_cursor(id, Up, true).unwrap();
        assert_eq!((info.selection_start, info.selection_end), (10002, 10002));
        te.dispose(id).unwrap();
    });
}
