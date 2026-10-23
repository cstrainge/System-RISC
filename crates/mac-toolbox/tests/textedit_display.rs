//! Native display options through the public SDK and real resident TextEdit.
#![cfg(not(target_arch = "riscv64"))]
mod support;
use mac_toolbox::{Error, Point, Rect,
                  quickdraw::fonts::FontSelection,
                  textedit::{DisplayOptions, Ruler}};

#[test]
fn incremental_edits_match_full_layout_at_unicode_and_wrap_boundaries()
{
    support::with_system(|system| {
        let te = system.textedit();
        let view = Rect::new(4, 8, 99, 88);
        let incremental = te.create_owned(view, view).unwrap();
        let reference = te.create_owned(view, view).unwrap();
        let mut text = "alpha beta gamma\rcafé Straße ﬁ\rthird line ".repeat(30);
        incremental.set_text(&text).unwrap();
        let mut random = 1u32;
        for iteration in 0..700
        {
            random = random.wrapping_mul(1664525).wrapping_add(1013904223);
            let boundaries: Vec<_> =
                text.char_indices().map(|(at, _)| at).chain([text.len()]).collect();
            let first = random as usize % boundaries.len();
            let start = boundaries[first];
            let end =
                boundaries[(first + random.rotate_left(9) as usize % 5).min(boundaries.len() - 1)];
            let inserted = ['x', ' ', '\r', 'é', 'ﬁ', '🦀'][iteration % 6];
            incremental.select(start as u32, end as u32).unwrap();
            te.key(incremental.id(), inserted).unwrap();
            text.replace_range(start..end, inserted.encode_utf8(&mut [0; 4]));
            reference.set_text(&text).unwrap();
            reference.set_rects(view, view).unwrap();
            let info = incremental.info().unwrap();
            assert_eq!(info.line_count,
                       reference.info().unwrap().line_count,
                       "edit {iteration}: {text:?}");
            for row in 0..info.line_count
            {
                assert_eq!(te.line(incremental.id(), row),
                           te.line(reference.id(), row),
                           "edit {iteration} row {row}: {text:?}");
            }
        }
        incremental.set_text("é").unwrap();
        let before = incremental.info().unwrap();
        assert_eq!(incremental.select(1, 1), Err(Error::BadArgument));
        assert_eq!(incremental.info().unwrap(), before);
        assert!(Error::BadArgument.to_string().contains("argument"));
    });
}

#[test]
fn display_defaults_capabilities_and_rejected_options_preserve_state()
{
    support::with_system(|system| {
        let te = system.textedit();
        let caps = te.display_capabilities().unwrap();
        assert_eq!(caps.version, 1);
        assert!(caps.ruler && caps.wrap && caps.line_numbers && caps.horizontal_scroll);
        let view = Rect::new(10, 10, 210, 100);
        let editor = te.create_owned(view, view).unwrap();
        assert_eq!(editor.display_options().unwrap(), DisplayOptions::default());
        assert_eq!(editor.display_info().unwrap().text_view, view);
        assert_eq!(editor.display_info().unwrap().gutter_width, 0);
        let before = editor.display_info().unwrap();
        assert_eq!(editor.set_display_options(DisplayOptions::default().ruler(Ruler::Column(0))),
                   Err(Error::BadArgument));
        assert_eq!(editor.set_display_options(DisplayOptions::default().ruler(Ruler::X(-1))),
                   Err(Error::BadArgument));
        assert_eq!(editor.display_info().unwrap(), before);
        let id = editor.id();
        drop(editor);
        assert_eq!(te.display_info(id), Err(Error::NotFound));
    });
    mac_system::testing::with_services(|services| {
        let client = unsafe { mac_toolbox::Client::from_raw(services) }.unwrap();
        let mut call=mac_toolbox::abi::textedit::TextEditDisplayCall { operation:mac_toolbox::abi::textedit::DISPLAY_CAPABILITIES, ..Default::default() };
        call.options.version = 99;
        assert_eq!(client.textedit_display(&mut call), Err(Error::NotSupported));
        assert!(Error::NotSupported.to_string().contains("supported"));
        // SAFETY: the host fixture lends its initialized services for this scope.
        let system = unsafe { mac_toolbox::System::from_raw(services) }.unwrap();
        let view = Rect::new(0, 0, 100, 80);
        let editor = system.textedit().create_owned(view, view).unwrap();
        let before = editor.display_info().unwrap();
        call = mac_toolbox::abi::textedit::TextEditDisplayCall { operation: mac_toolbox::abi::textedit::SET_DISPLAY, id: editor.id(), ..Default::default() };
        call.options.ruler_kind = 99;
        assert_eq!(client.textedit_display(&mut call), Err(Error::NotSupported));
        assert_eq!(editor.display_info().unwrap(), before);
        call.options = Default::default();
        call.operation = 99;
        assert_eq!(client.textedit_display(&mut call), Err(Error::NotSupported));
        assert_eq!(editor.display_info().unwrap(), before);
    });
}

#[test]
fn wrap_horizontal_geometry_and_logical_numbers_compose_without_changing_text()
{
    support::with_system(|system| {
        let te = system.textedit();
        let view = Rect::new(0, 0, 120, 64);
        let editor = te.create_owned_with_options(view,
                                                  view,
                                                  DisplayOptions::default().wrapping(false)
                                                                           .numbered(true))
                       .unwrap();
        let text = format!("{}\r{}", "abcdef".repeat(700), "second");
        editor.set_text(&text).unwrap();
        let initial = editor.display_info().unwrap();
        assert_eq!(initial.logical_lines, 2);
        assert_eq!(initial.editor.line_count, 2);
        assert!(initial.scroll_max_x > u16::MAX as u32 / 4);
        assert_eq!(initial.text_view.left, initial.gutter_width as i16);
        for x in [0, initial.scroll_max_x / 2, initial.scroll_max_x]
        {
            let info = editor.scroll_axes(x, 0).unwrap();
            let point = Point { h: info.text_view.left + 30,
                                v: 1 };
            let at = te.offset(editor.id(), point).unwrap();
            let actual = te.point(editor.id(), at).unwrap();
            assert!((actual.h - point.h).abs() <= 7, "{x}: {point:?} -> {at} -> {actual:?}");
        }
        editor.select(4200, 4200).unwrap();
        te.reveal_selection(editor.id()).unwrap();
        let caret = te.point(editor.id(), 4200).unwrap();
        assert!(caret.h >= initial.text_view.left && caret.h <= initial.text_view.right);
        let selected = editor.info().unwrap();
        editor.set_display_options(DisplayOptions::default().numbered(true)).unwrap();
        let wrapped = editor.display_info().unwrap();
        assert_eq!(wrapped.logical_lines, 2);
        assert!(wrapped.editor.line_count > 2);
        assert_eq!(wrapped.scroll_max_x, 0);
        assert_eq!(wrapped.editor.selection_start, selected.selection_start);
        let mut bytes = vec![0; text.len()];
        assert_eq!(editor.text(&mut bytes).unwrap(), text);
        editor.set_text(&"x\r".repeat(8)).unwrap();
        let nine = editor.display_info().unwrap();
        assert_eq!(nine.logical_lines, 9);
        editor.select(u32::MAX, u32::MAX).unwrap();
        editor.key('\r').unwrap();
        let ten = editor.display_info().unwrap();
        assert_eq!(ten.logical_lines, 10);
        assert!(ten.gutter_width > nine.gutter_width);
    });
}

#[test]
fn ruler_modes_retain_intent_across_fonts_and_horizontal_scroll()
{
    with_fonts(|system| {
        let view = Rect::new(0, 0, 160, 80);
        let editor = system.textedit().create_owned(view, view).unwrap();
        editor.set_font(FontSelection { family: 4,
                                        size:   12,
                                        face:   0 })
              .unwrap();
        let columns = DisplayOptions::default().wrapping(false).ruler(Ruler::Column(80));
        let first = editor.set_display_options(columns).unwrap();
        assert!(first.monospaced);
        assert_eq!(first.ruler_x, Some((first.column_width * 80) as i32));
        editor.set_font(FontSelection { family: 4,
                                        size:   18,
                                        face:   0 })
              .unwrap();
        let larger = editor.display_info().unwrap();
        assert_eq!(larger.options.ruler, Ruler::Column(80));
        assert_eq!(larger.ruler_x, Some((larger.column_width * 80) as i32));
        assert!(larger.column_width > first.column_width);
        editor.set_font(FontSelection { family: 3,
                                        size:   12,
                                        face:   0 })
              .unwrap();
        assert!(!editor.display_info().unwrap().monospaced);
        let pixels = columns.ruler(Ruler::X(100));
        assert_eq!(editor.set_display_options(pixels).unwrap().ruler_x, Some(100));
        editor.set_font(FontSelection { family: 3,
                                        size:   18,
                                        face:   0 })
              .unwrap();
        assert_eq!(editor.display_info().unwrap().ruler_x, Some(100));
        editor.set_text(&"x".repeat(100)).unwrap();
        assert_eq!(editor.scroll_axes(40, 0).unwrap().ruler_x, Some(60));
    });
}

fn with_fonts(run: impl FnOnce(&mac_toolbox::System<'_>))
{
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../system/assets/fonts");
    let resources:Vec<_>=std::fs::read_dir(root).unwrap().map(|entry| {
        let path=entry.unwrap().path();
        let stem=path.file_stem().unwrap().to_str().unwrap();
        let (kind,id)=stem.split_once('-').unwrap();
        mac_formats::resource::Resource {kind:kind.as_bytes().try_into().unwrap(),id:id.parse().unwrap(),data:std::fs::read(path).unwrap(),name:None,attributes:0}
    }).collect();
    mac_system::testing::with_resource_forks(&mac_formats::resource::encode(&resources).unwrap(),
                                             &mac_formats::resource::encode(&[]).unwrap(),
                                             |services| {
                                                 let system =
                                                     unsafe {
                                                         mac_toolbox::System::from_raw(services)
                                                     }.unwrap();
                                                 run(&system);
                                             }).unwrap();
}

#[test]
#[ignore = "requires optimized image and QEMU"]
fn options_dialog_uses_managed_controls_validates_and_persists_display_settings()
{
    use std::{fs, path::PathBuf, process::Command};
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output = root.join(format!("results/system7/textedit-options-{}", std::process::id()));
    let capture = Command::new("python3").arg(root.join("System7/scripts/\
                                                         capture_text_edit_options.py"))
                                         .arg("--output")
                                         .arg(&output)
                                         .output()
                                         .unwrap();
    assert!(capture.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"]);
    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("preferences.json")).unwrap()).unwrap();
    assert_eq!(settings["version"], 2);
    assert_eq!(settings["font_family"], 22);
    assert_eq!(settings["font_size"], 18);
    assert_eq!(settings["wrap"], false);
    assert_eq!(settings["line_numbers"], true);
    assert_eq!(settings["ruler_kind"], 1);
    assert_eq!(settings["ruler_value"], 20);
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    for forbidden in ["SYSTEM_PANIC:", "SYSTEM_ALLOCATION_FAILED:", "TEXT_EDIT_ERROR",
                      "TEXT_EDIT_PREFERENCES_FAILED"]
    {
        assert!(!serial.contains(forbidden), "{forbidden}: {serial}");
    }
    assert!(serial.contains("TEXT_EDIT_OPTIONS_INVALID_LIMIT\n"));
    assert!(serial.contains("TEXT_EDIT_OPTIONS_CANCELLED\n"));
    let (_, _, active_options) =
        support_pixels::ppm(&fs::read(output.join("options.ppm")).unwrap());
    let (_, _, background_options) =
        support_pixels::ppm(&fs::read(output.join("background-options.ppm")).unwrap());
    let selection_pixel = (263 * 640 + 422) * 3;
    assert_ne!(&active_options[selection_pixel..selection_pixel + 3],
               &background_options[selection_pixel..selection_pixel + 3],
               "background Options retained active text selection");
    let (_, _, before_pixels) = support_pixels::ppm(&fs::read(output.join("default.ppm")).unwrap());
    let (_, _, cancelled_pixels) =
        support_pixels::ppm(&fs::read(output.join("cancelled.ppm")).unwrap());
    for y in 98..128
    {
        for x in 128..145
        {
            let at = (y * 640 + x) * 3;
            assert_eq!(&before_pixels[at..at + 3],
                       &cancelled_pixels[at..at + 3],
                       "Cancel changed document text pixels at{x},{y}");
        }
    }
    let (_, _, new_document) =
        support_pixels::ppm(&fs::read(output.join("new-document.ppm")).unwrap());
    assert_eq!(&new_document[(90 * 640 + 120) * 3..(90 * 640 + 120) * 3 + 3],
               &[238, 238, 238],
               "new document did not inherit the edge-to-edge gutter");
    // Renderer-owned gutter/ruler palette is visible in an actual document.
    let (_, _, pixels) = support_pixels::ppm(&fs::read(output.join("applied.ppm")).unwrap());
    for (x, y) in [(120, 90), (120, 200), (120, 394), (125, 90), (125, 394)]
    {
        let at = (y * 640 + x) * 3;
        assert_eq!(&pixels[at..at + 3], &[238, 238, 238], "white border around gutter at {x},{y}");
    }
    assert!(pixels.chunks_exact(3).any(|p| p == [238, 238, 238]), "no gutter/ruler shade");
    assert!(pixels.chunks_exact(3).any(|p| p == [153, 153, 153]), "no gutter/ruler separator");
    fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&serde_json::json!({"status":"passed","capture":report,"settings":settings})).unwrap()).unwrap();
}
mod support_pixels
{
    pub fn ppm(bytes: &[u8]) -> (usize, usize, Vec<u8>)
    {
        let mut at = 0;
        let mut token = || {
            while bytes[at].is_ascii_whitespace()
            {
                at += 1;
            }
            let start = at;
            while !bytes[at].is_ascii_whitespace()
            {
                at += 1;
            }
            std::str::from_utf8(&bytes[start..at]).unwrap()
        };
        assert_eq!(token(), "P6");
        let w = token().parse().unwrap();
        let h = token().parse().unwrap();
        assert_eq!(token(), "255");
        (w, h, bytes[at + 1..].to_vec())
    }
}

#[test]
fn edge_to_edge_gutter_preserves_text_inset_and_reaches_the_last_line()
{
    support::with_system(|system| {
        let view = Rect::new(0, 0, 120, 64);
        let dest = Rect::new(0, 8, 120, 64);
        let editor =
            system.textedit()
                  .create_owned_with_options(dest, view, DisplayOptions::default().numbered(true))
                  .unwrap();
        editor.set_text(&"a\r".repeat(100)).unwrap();
        editor.select(u32::MAX, u32::MAX).unwrap();
        let info = system.textedit().reveal_selection(editor.id()).unwrap();
        assert_eq!(info.scroll_y, info.scroll_max);
        // The native insertion row after the final CR has its bottom at64.
        // Classic TEGetPoint intentionally excludes that empty row.
        assert_eq!(info.scroll_max, 101 * 16 + 8 - 64);
        assert_eq!(editor.display_info().unwrap().text_view.top, 0);
    });
}
