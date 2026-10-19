//! Menu Manager compatibility through public SDK and real resource handlers.
#![cfg(not(target_arch = "riscv64"))]
use mac_toolbox::{Event, EventKind, System};
#[path = "conformance/menu_sizing.rs"]
mod menu_sizing;
#[test]
fn uncached_menu_sizes_match_original_calc_menu_size()
{
    use mac_formats::resource::{Resource, encode};
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("data/menu-sizing/original.json")).unwrap();
    let bytes = [include_bytes!("data/menu-sizing/31000.bin").as_slice(),
                 include_bytes!("data/menu-sizing/31001.bin").as_slice(),
                 include_bytes!("data/menu-sizing/31002.bin").as_slice(),
                 include_bytes!("data/menu-sizing/31003.bin").as_slice(),
                 include_bytes!("data/menu-sizing/31004.bin").as_slice(),
                 include_bytes!("data/menu-sizing/31005.bin").as_slice(),
                 include_bytes!("data/menu-sizing/31006.bin").as_slice(),
                 include_bytes!("data/menu-sizing/31007.bin").as_slice()];
    let resources: Vec<_> =
        menu_sizing::CASES.iter()
                          .zip(bytes)
                          .zip(expected.as_array().unwrap())
                          .map(|((&(id, name, width, height), bytes), captured)| {
                              assert_eq!(captured["case"], name);
                              assert_eq!(captured["width"], width);
                              assert_eq!(captured["height"], height);
                              Resource { kind: *b"MENU",
                                         id,
                                         name: None,
                                         attributes: 0,
                                         data: bytes.to_vec() }
                          })
                          .collect();
    let fork = encode(&resources).unwrap();
    mac_system::testing::menus::with_resources(&fork, &fork, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        menu_sizing::run(&system).unwrap();
    });
}
#[test]
fn malformed_menu_resources_fail_without_installing_partial_objects()
{
    use mac_formats::resource::{Resource, encode};
    use mac_toolbox::{BAD_ARGUMENT, NOT_FOUND, NOT_SUPPORTED};
    let original = include_bytes!("data/original-menu-257.bin");
    for (bytes, command, expected) in
        [(original[..12].to_vec(), None, BAD_ARGUMENT),
         (original[..original.len() - 1].to_vec(), None, BAD_ARGUMENT),
         ({
              let mut b = original.to_vec();
              b[7] = 1;
              b
          },
          None,
          NOT_SUPPORTED),
         (original.to_vec(), Some(vec![0; 3]), BAD_ARGUMENT)]
    {
        let mut records = vec![Resource { kind:       *b"MENU",
                                          id:         257,
                                          name:       None,
                                          attributes: 0,
                                          data:       bytes }];
        if let Some(data) = command
        {
            records.push(Resource { kind: *b"MCMD",
                                    id: 257,
                                    name: None,
                                    attributes: 0,
                                    data });
        }
        let fork = encode(&records).unwrap();
        mac_system::testing::menus::with_resources(&fork, &fork, |services| {
            let system = unsafe { System::from_raw(services) }.unwrap();
            assert_eq!(system.menus().install_menu_resource(1, 257).unwrap_err().0, expected);
            assert_eq!(system.menus().item_count(257).unwrap_err().0, NOT_FOUND);
        });
    }
}
#[test]
fn original_menu_records_install_and_mutate_through_public_sdk()
{
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("data/original-menu-metadata.json")).unwrap();
    let empty = include_bytes!("data/original-menus.rsrc");
    mac_system::testing::menus::with_resources(empty, empty, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let api = system.menus();
        for menu in oracle["menus"].as_array().unwrap()
        {
            let id = menu["id"].as_i64().unwrap() as i16;
            api.install_menu_resource(1, id).unwrap();
            assert_eq!(api.item_count(id).unwrap() as usize,
                       menu["items"].as_array().unwrap().len());
            let layout = api.layout(id).unwrap();

            assert_eq!(layout.title.left, menu["left"].as_i64().unwrap() as i16);
            if menu["width"].as_i64().unwrap() > 0
            {
                assert_eq!(i64::from(layout.popup.right - layout.popup.left),
                           menu["width"].as_i64().unwrap(),
                           "menu{id} width");
                assert_eq!(i64::from(layout.popup.bottom - layout.popup.top),
                           menu["height"].as_i64().unwrap(),
                           "menu{id} height");
            }
            for item in menu["items"].as_array().unwrap()
            {
                let index = item["index"].as_u64().unwrap() as u16;
                assert_eq!(api.item(id, index).unwrap().enabled,
                           item["enabled"].as_bool().unwrap(),
                           "menu{id} item{index}");
            }
        }
        assert_eq!(api.selection(&Event::default()).unwrap(), None);
        api.set_item_enabled(257, 1, false).unwrap();
        assert!(!api.item(257, 1).unwrap().enabled);
        api.set_item_enabled(257, 1, true).unwrap();
        api.set_item_checked(257, 1, true).unwrap();
        assert!(api.item(257, 1).unwrap().checked);
        let selection = api.selection(&Event { kind: EventKind::MenuCommand,
                                               message: (257u32 << 16) | 1,
                                               ..Default::default() })
                           .unwrap()
                           .unwrap();
        assert_eq!((selection.menu_id, selection.item), (257, 1));
        api.clear().unwrap();
        assert!(api.item_count(257).is_err());
    });
}

fn frame(path: &std::path::Path) -> Vec<u8>
{
    let bytes = std::fs::read(path).unwrap();
    let prefix = b"P6\n640 480\n255\n";
    assert!(bytes.starts_with(prefix), "unexpected QMP PPM header");
    assert_eq!(bytes.len(), prefix.len() + 640 * 480 * 3);
    bytes[prefix.len()..].to_vec()
}
#[test]
#[ignore = "requires built disk image and qemu-system-riscv64; actual publicSDK guest"]
fn real_menu_tracking_and_keyboard_equivalents_reach_same_command_path()
{
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
                                                                   .canonicalize()
                                                                   .unwrap();
    let output = root.join(format!("results/system7/menu-userspace-{}", std::process::id()));
    let capture =
        std::process::Command::new("python3").arg(root.join("System7/scripts/\
                                                             capture_system7_window_tracking.py"))
                                             .args(["--menus", "--output"])
                                             .arg(&output)
                                             .current_dir(&root)
                                             .output()
                                             .unwrap();
    assert!(capture.status.success(),
            "capture transport failed:\n{}\n{}",
            String::from_utf8_lossy(&capture.stdout),
            String::from_utf8_lossy(&capture.stderr));
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output.join("capture.json")).unwrap()).unwrap();
    assert_eq!(report["disk_sha256_before"], report["disk_sha256_after"]);
    let events = report["menu_events"].as_array().unwrap();
    assert_eq!(events.len(), 2, "disabled or cancelled menu produced a command");
    assert_eq!(events[0], "MENU_PROBE menu=257 item=1 command=00000000");
    assert_eq!(events[0], events[1], "Ctrl-N used a different command path from File>New Folder");
    let baseline = frame(&output.join("menu-baseline.ppm"));
    for name in ["menu-selected", "menu-disabled-release", "menu-cancelled", "menu-key",
                 "menu-disabled-key"]
    {
        let actual = frame(&output.join(format!("{name}.ppm")));
        assert!(baseline == actual, "{name} left stale pixels");
    }
    let file = frame(&output.join("menu-file.ppm"));
    assert!(file == frame(&output.join("menu-disabled.ppm")), "disabled row highlighted");
    assert!(file == frame(&output.join("menu-outside.ppm")), "outside hover changed popup");
    let highlighted = frame(&output.join("menu-file-highlight.ppm"));
    assert!(file.as_chunks::<3>()
                .0
                .iter()
                .zip(highlighted.as_chunks::<3>().0)
                .filter(|(a, b)| a != b)
                .count()
            > 1000,
            "enabled row did not highlight");
    assert!(file != frame(&output.join("menu-edit.ppm")), "title hover did not switch menus");
    // The authored MENU has no cached width or shortcut. Its longest label
    // must contain every pixel produced by the public Chicago text service.
    let keyless = frame(&output.join("menu-keyless.ppm"));
    let width = report["menu_label_width"].as_u64().unwrap() as usize;
    assert!(width > 16 && width < 400);
    for y in 20..36
    {
        for x in 24..24 + width
        {
            let at = (y * 640 + x) * 3;
            let reference = ((y + 108) * 640 + x + 92) * 3;
            assert_eq!(&keyless[at..at + 3],
                       &baseline[reference..reference + 3],
                       "authored longest keyless label pixel ({x},{y})");
        }
    }
    assert_eq!(report["menu_sizing_pass"], true,
               "uncached original sizes failed through RV64 ECALL");
    let keyed = frame(&output.join("menu-keyed.ppm"));
    // Original CalcMenuSize makes this139px wide: a91px label,10px gap,
    //24px command column and14px left inset. Compare both complete strings
    // against independent public QuickDraw rendering in the probe window.
    for y in 20..36
    {
        for x in 24..115
        {
            let actual = (y * 640 + x) * 3;
            let reference = ((y + 133) * 640 + x + 92) * 3;
            assert_eq!(&keyed[actual..actual + 3],
                       &baseline[reference..reference + 3],
                       "keyed label ({x},{y})");
        }
        for x in 115..125
        {
            let actual = (y * 640 + x) * 3;
            assert_eq!(&keyed[actual..actual + 3], &[255, 255, 255], "shortcut gap ({x},{y})");
        }
        for x in 125..149
        {
            let actual = (y * 640 + x) * 3;
            let reference = ((y + 158) * 640 + x - 9) * 3;
            assert_eq!(&keyed[actual..actual + 3],
                       &baseline[reference..reference + 3],
                       "command shortcut ({x},{y})");
        }
    }
    let apple = include_bytes!("data/original-menu-apple-title.rgb");
    for y in 0..19
    {
        for x in 10..34
        {
            let at = (y * 640 + x) * 3;
            let original = (y * 24 + x - 10) * 3;
            assert_eq!(&file[at..at + 3],
                       &apple[original..original + 3],
                       "original normal Apple title pixel ({x},{y})");
        }
    }
    let file_title = include_bytes!("data/original-menu-file-title.rgb");
    for y in 0..20
    {
        for x in 33..74
        {
            let at = (y * 640 + x) * 3;
            let original = (y * 41 + x - 33) * 3;
            assert_eq!(&file[at..at + 3],
                       &file_title[original..original + 3],
                       "original selected File title pixel ({x},{y})");
        }
    }
    for (name, expected) in
        [("menu-file", include_bytes!("data/original-menu-file-unselected.rgb").as_slice()),
         ("menu-file-highlight", include_bytes!("data/original-menu-file-row1.rgb").as_slice()),
         ("menu-disabled", include_bytes!("data/original-menu-file-disabled-row2.rgb").as_slice())]
    {
        let actual = frame(&output.join(format!("{name}.ppm")));
        let mut different = Vec::new();
        for y in 20..278
        {
            for x in 33..175
            {
                let expected_at = ((y - 20) * 142 + x - 33) * 3;
                let at = (y * 640 + x) * 3;
                if actual[at..at + 3] != expected[expected_at..expected_at + 3]
                {
                    different.push((x, y));
                }
            }
        }
        assert!(different.is_empty(),
                "{name} differs from original MDEF0: {} pixels, first {:?}",
                different.len(),
                &different[..different.len().min(20)]);
    }
    std::fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&serde_json::json!({"status":"passed","transport":"public SDK-only guest / loaded MDEF / real virtio input", "commands":2,"frames":14,"original_uncached_sizes":8,"native_keyed_pixels":2000,"original_exact_rgb_pixels":111184,"native_keyless_label_pixels":width*16,"scope":"click/key selection, disabled key/item/cancel/barswitch, exact restoration, three original File popup crops, original normal Apple title and selected File title; native authored label matches public font rendering"})).unwrap()).unwrap();
}
