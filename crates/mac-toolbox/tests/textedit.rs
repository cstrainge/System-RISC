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
        use mac_toolbox::{Error,
                          textedit::{MAX_BYTES, MAX_EDITORS}};
        let te = system.textedit();
        let rect = mac_toolbox::Rect::new(0, 0, 100, 100);
        let id = te.create(rect, rect).unwrap();
        let maximum = "a".repeat(MAX_BYTES);
        te.set_text(id, &maximum).unwrap();
        let before = te.info(id).unwrap();
        system.scrap().clear().unwrap();
        system.scrap().put_text("é").unwrap();
        assert_eq!(te.paste(id), Err(Error::NoMemory));
        assert_eq!(te.info(id).unwrap(), before);
        let mut bytes = [0; 16];
        assert_eq!(system.scrap().text(&mut bytes).unwrap(), "é");
        assert_eq!(te.insert(id, "!"), Err(Error::NoMemory));
        assert_eq!(te.info(id).unwrap(), before);
        te.dispose(id).unwrap();
        assert_eq!(te.dispose(id), Err(Error::NotFound));
        let ids: Vec<_> = (0..MAX_EDITORS).map(|_| te.create(rect, rect).unwrap()).collect();
        assert_eq!(te.create(rect, rect), Err(Error::NoMemory));
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

fn with_editor_fonts(run: impl FnOnce(&mac_toolbox::System<'_>))
{
    use mac_formats::resource::{Resource, encode};
    let resources: Vec<_> = [
        (*b"FOND", 0, include_bytes!("../../../system/assets/fonts/FOND-0.bin").as_slice()),
        (*b"FONT", 12, include_bytes!("../../../system/assets/fonts/FONT-12.bin").as_slice()),
        (*b"FOND", 3, include_bytes!("../../../system/assets/fonts/FOND-3.bin").as_slice()),
        (*b"FONT", 393, include_bytes!("../../../system/assets/fonts/FONT-393.bin").as_slice())
    ].into_iter().map(|(kind,id,data)|Resource {kind,id,data:data.to_vec(),name:None,attributes:0}).collect();
    mac_system::testing::with_resource_forks(&encode(&resources).unwrap(),
                                             &encode(&[]).unwrap(),
                                             |services| {
                                                 let system =
                                                     unsafe {
                                                         mac_toolbox::System::from_raw(services)
                                                     }.unwrap();
                                                 run(&system);
                                             }).unwrap();
}

#[test]
fn native_editor_font_geometry_matches_resident_quickdraw_and_is_per_instance()
{
    with_editor_fonts(|system| {
        use mac_toolbox::{Point, Rect,
                          textedit::{CursorMovement, FontSelection}};
        let te = system.textedit();
        let fonts = system.quickdraw().fonts();
        let font = FontSelection { family: 1,
                                   size:   9,
                                   face:   0 };
        let metrics = fonts.metrics(font).unwrap();
        let r = Rect::new(10, 20, 310, 200);
        let id = te.create(r, r).unwrap();
        let other = te.create(r, r).unwrap();
        let text = "Wiim é\rWiim é";
        te.set_text(id, text).unwrap();
        te.set_text(other, text).unwrap();
        let original = te.info(other).unwrap();
        let info = te.set_font(id, font).unwrap();
        assert_eq!(info.ascent, metrics.ascent);
        assert_eq!(info.line_height, metrics.ascent + metrics.descent + metrics.leading);
        assert_eq!(fonts.metrics(FontSelection { family: 3,
                                                 ..font })
                        .unwrap(),
                   metrics);
        let line = te.line(id, 0).unwrap();
        assert_eq!(line.width, fonts.text_width(font, "Wiim é").unwrap());
        for at in "Wiim é".char_indices().map(|(at, _)| at).chain(["Wiim é".len()])
        {
            let point = te.point(id, at as u32).unwrap();
            assert_eq!(point.h, 11 + fonts.text_width(font, &text[..at]).unwrap());
            assert_eq!(point.v, 20 + info.line_height);
            assert_eq!(te.offset(id,
                                 Point { v: 20 + info.line_height / 2,
                                         ..point })
                         .unwrap(),
                       at as u32);
        }
        te.select(id, 3, 3).unwrap();
        let moved = te.move_cursor(id, CursorMovement::Down, true).unwrap();
        assert_eq!((moved.selection_start, moved.selection_end), (3, 11));
        let before = te.info(id).unwrap();
        let changed = te.set_font(id,
                                  FontSelection { face: 1,
                                                  ..font })
                        .unwrap();
        assert_eq!((changed.selection_start, changed.selection_end),
                   (before.selection_start, before.selection_end));
        assert_eq!(te.line(id, 0).unwrap().width,
                   fonts.text_width(FontSelection { face: 1,
                                                    ..font },
                                    "Wiim é")
                        .unwrap());
        assert_eq!(te.info(other).unwrap(), original);
        te.dispose(id).unwrap();
        te.dispose(other).unwrap();
    });
}

#[test]
fn native_editor_font_reflow_new_characters_paste_and_failure_are_consistent()
{
    with_editor_fonts(|system| {
        use mac_toolbox::{Error, Rect, textedit::FontSelection};
        let te = system.textedit();
        let fonts = system.quickdraw().fonts();
        let font = FontSelection { family: 1,
                                   size:   9,
                                   face:   0 };
        let r = Rect::new(0, 0, 40, 200);
        let id = te.create(r, r).unwrap();
        te.set_text(id, "WWWWWWWWWWWWWWWW").unwrap();
        let before = te.info(id).unwrap();
        let info = te.set_font(id, font).unwrap();
        assert!(info.line_count < before.line_count);
        let mut bytes = [0; 128];
        let text = te.text(id, &mut bytes).unwrap().to_owned();
        for row in 0..info.line_count
        {
            let line = te.line(id, row).unwrap();
            assert_eq!(line.width,
                       fonts.text_width(font, &text[line.start as usize..line.end as usize])
                            .unwrap());
        }
        let wide = Rect::new(0, 0, 300, 200);
        te.set_rects(id, wide, wide).unwrap();
        te.set_text(id, "a").unwrap();
        te.key(id, 'é').unwrap();
        system.scrap().clear().unwrap();
        system.scrap().put_text("Ω日本").unwrap();
        te.paste(id).unwrap();
        let text = te.text(id, &mut bytes).unwrap().to_owned();
        assert_eq!(text, "aéΩ日本");
        assert_eq!(te.line(id, 0).unwrap().width, fonts.text_width(font, &text).unwrap());
        let before = te.info(id).unwrap();
        let point = te.point(id, before.text_length).unwrap();
        assert_eq!(te.set_font(id,
                               FontSelection { family: 12345,
                                               ..font }),
                   Err(Error::NotFound));
        assert_eq!(te.set_font(id,
                               FontSelection { size: -1,
                                               ..font }),
                   Err(Error::BadArgument));
        assert_eq!(te.info(id).unwrap(), before);
        assert_eq!(te.point(id, before.text_length).unwrap(), point);
        assert_eq!(te.text(id, &mut bytes).unwrap(), text);
        te.dispose(id).unwrap();
    });
}
