//! Document codecs and public TextEdit view/content semantics, not app internals.
#![cfg(not(target_arch = "riscv64"))]
mod support;
#[path = "../../../apps/text-edit/src/encoding.rs"]
mod encoding;
#[path = "../../../apps/text-edit/src/preferences.rs"]
mod preferences;
#[path = "../../../apps/text-edit/src/font_choices.rs"]
mod font_choices;
#[path = "../../../apps/text-edit/src/ui.rs"]
mod ui;
use ui::Text;

// The shared app preference module gets localized labels from its resources in
// the guest. Pure codec cases below need only its no-I/O schema methods.
fn text<'a>(_: &mac_toolbox::System<'_>, _: u32, _: &'a mut [u8])
            -> Result<&'a str, mac_toolbox::Error>
{
    Err(mac_toolbox::Error::NotFound)
}

#[test]
fn newline_codecs_preserve_mac_unix_and_windows_style_and_utf8()
{
    for (source, style) in [("café\r第二行\r", encoding::LineEnding::Cr),
                            ("café\n第二行\n", encoding::LineEnding::Lf),
                            ("café\r\n第二行\r\n", encoding::LineEnding::CrLf)]
    {
        let mut bytes = source.as_bytes().to_vec();
        let (length, actual) = encoding::decode(&mut bytes);
        assert_eq!(actual, style);
        let internal = core::str::from_utf8(&bytes[..length]).unwrap();
        assert_eq!(internal, "café\r第二行\r");
        let mut output = [0; 128];
        let size = encoding::encode_chunk(internal, style, &mut output).unwrap();
        assert_eq!(&output[..size], source.as_bytes());
        assert!(encoding::encode_chunk(internal, style, &mut [0; 1]).is_none());
    }
    let mut mixed = b"one\r\ntwo\nthree\rfour".to_vec();
    let (length, style) = encoding::decode(&mut mixed);
    assert_eq!(style, encoding::LineEnding::CrLf);
    assert_eq!(&mixed[..length], b"one\rtwo\rthree\rfour");
}

#[test]
fn font_preferences_are_versioned_json_with_monaco_default_and_strict_validation()
{
    use mac_toolbox::{Error, quickdraw::fonts::FontSelection};
    let default = preferences::Settings::default();
    assert_eq!(default.font().family, 4);
    assert_eq!(default.font().size, 12);
    let selected = preferences::Settings::from_font(FontSelection { family: 22,
                                                                    size:   18,
                                                                    face:   0 });
    let mut bytes = [0; 256];
    let length = selected.encode(&mut bytes).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes[..length]).unwrap();
    assert_eq!(json["version"], 2);
    assert_eq!(preferences::Settings::decode(&bytes[..length]).unwrap().font(), selected.font());
    for invalid in [br#"{"version":2,"font_family":4,"font_size":12}"#.as_slice(),
                    br#"{"version":1,"font_family":999,"font_size":12}"#,
                    br#"{"version":1,"font_family":4,"font_size":-1}"#,
                    br#"{"version":1,"font_family":4,"font_size":12,"extra":0}"#,
                    br#"{"version":1,"version":1,"font_family":4,"font_size":12}"#,
                    br#"{"version":1,"font_family":4,"font_size":12}garbage"#,
                    b"{"]
    {
        let error = preferences::Settings::decode(invalid).err().unwrap();
        assert_eq!(error, Error::CorruptData);
        assert!(error.to_string().contains("damaged or malformed"));
    }
}

#[test]
fn only_text_mutations_change_the_document_content_revision()
{
    support::with_system(|system| {
        use mac_toolbox::{Rect, textedit::CursorMovement};
        let te = system.textedit();
        let view = Rect::new(0, 0, 200, 80);
        let editor = te.create_owned(view, view).unwrap();
        let id = editor.id();
        let original = te.set_text(id, "hello\rworld").unwrap().content_revision;
        te.select(id, 0, 5).unwrap();
        te.activate(id, true).unwrap();
        te.move_cursor(id, CursorMovement::DocumentEnd, false).unwrap();
        te.idle(id, 30).unwrap();
        te.set_rects(id, Rect::new(0, 0, 180, 80), view).unwrap();
        te.scroll_to(id, u32::MAX).unwrap();
        te.reveal_selection(id).unwrap();
        assert_eq!(te.info(id).unwrap().content_revision, original);
        assert_eq!(te.set_text(id, "hello\rworld").unwrap().content_revision, original);
        assert_ne!(te.key(id, '!').unwrap().content_revision, original);
    });
}

#[test]
fn native_document_scrolling_reaches_beyond_classic_coordinate_limits()
{
    support::with_system(|system| {
        use mac_toolbox::{Point, Rect};
        let te = system.textedit();
        let view = Rect::new(0, 0, 200, 80);
        let editor = te.create_owned(view, view).unwrap();
        let source = "x\r".repeat(12_000);
        let original = te.set_text(editor.id(), &source).unwrap();
        let bottom = te.reveal_selection(editor.id()).unwrap();
        assert!(bottom.scroll_y > u16::MAX as u32);
        assert_eq!(bottom.scroll_y, bottom.scroll_max);
        assert_eq!(bottom.content_revision, original.content_revision);
        assert!(te.offset(editor.id(),
                          Point { h: 0,
                                  v: 1 })
                  .unwrap()
                > 23_000);
        let top = te.scroll_to(editor.id(), 0).unwrap();
        assert_eq!(top.scroll_y, 0);
        assert_eq!(te.offset(editor.id(),
                             Point { h: 0,
                                     v: 1 })
                     .unwrap(),
                   0);
        assert_eq!(top.content_revision, original.content_revision);
    });
}

#[test]
#[ignore = "requires fresh optimized cargo xtask image and QEMU"]
fn finder_opens_extensionless_json_preferences_and_text_edit_preserves_their_type_on_save()
{
    use std::{fs, path::PathBuf, process::Command};

    use serde_json::Value;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let output =
        root.join("results/system7").join(format!("text-edit-documents-{}", std::process::id()));
    let capture = Command::new("python3").arg(root.join("System7/scripts/\
                                                         capture_text_edit_documents.py"))
                                         .arg("--output")
                                         .arg(&output)
                                         .current_dir(&root)
                                         .output()
                                         .unwrap();
    assert!(capture.status.success(),
            "document transport failed:\n{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["original_before"], report["original_after"], "base installation changed");
    let serial = fs::read_to_string(output.join("vm/serial.log")).unwrap();
    for forbidden in ["SYSTEM_ALLOCATION_FAILED:", "SYSTEM_PANIC:", "SYSTEM_TRAP:",
                      "TEXT_EDIT_ERROR", "TEXT_EDIT_PREFERENCES_FAILED", "FINDER_ERROR"]
    {
        assert!(!serial.contains(forbidden), "{forbidden}: {serial}");
    }
    assert_eq!(serial.matches("TEXT_EDIT_DOCUMENT_OPEN\n").count(), 2, "{serial}");
    assert_eq!(serial.matches("TEXT_EDIT_DOCUMENT_SAVED\n").count(), 2, "{serial}");
    assert!(serial.contains("STANDARD_FILE_DONE "), "save bypassed System dialog: {serial}");
    assert_eq!(fs::read(output.join("document.bytes")).unwrap(), b"hi");
    for (key, name) in [("registry", "File Types"), ("preferences", "Text Edit Preferences")]
    {
        assert_eq!(report[key]["name"], name);
        let finder = report[key]["finder_info"].as_array().unwrap();
        let codes: Vec<u8> = finder[..8].iter().map(|v| v.as_u64().unwrap() as u8).collect();
        assert_eq!(codes, b"JSONTEDT", "{key}: native JSON/Text Edit association lost");
        let bytes = fs::read(output.join(format!("{key}.bytes"))).unwrap();
        let json: Value =
            serde_json::from_slice(&bytes).expect("saved preferences must remain valid JSON");
        assert_eq!(json["version"], if key == "preferences" { 2 } else { 1 });
        if key == "preferences"
        {
            assert_eq!(json["font_family"], 3);
            assert_eq!(json["font_size"], 18);
        }
    }
    fs::write(output.join("rust-acceptance.json"), serde_json::to_vec_pretty(
        &serde_json::json!({"status":"passed", "capture":report,
                           "coverage":"real Finder JSON association, System save dialog, font preferences, JSON type retained"})).unwrap()).unwrap();
}
