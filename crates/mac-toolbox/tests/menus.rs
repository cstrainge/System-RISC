//! Menu Manager compatibility through public SDK and real resource handlers.
#![cfg(not(target_arch = "riscv64"))]
use mac_toolbox::{Event, System, abi::EventKind};
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
    use mac_toolbox::abi::{BAD_ARGUMENT, NOT_FOUND, NOT_SUPPORTED};
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
            assert_eq!(system.menus()
                             .install_menu_resource(mac_toolbox::ResourceScope::Application, 257)
                             .unwrap_err()
                             .status(),
                       expected);
            assert_eq!(system.menus().item_count(mac_toolbox::MenuId(257)).unwrap_err().status(),
                       NOT_FOUND);
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
            api.install_menu_resource(mac_toolbox::ResourceScope::Application, id).unwrap();
            assert_eq!(api.item_count(mac_toolbox::MenuId(id)).unwrap() as usize,
                       menu["items"].as_array().unwrap().len());
            let layout = api.layout(mac_toolbox::MenuId(id)).unwrap();

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
                assert_eq!(api.item(mac_toolbox::MenuId(id), index).unwrap().enabled,
                           item["enabled"].as_bool().unwrap(),
                           "menu{id} item{index}");
            }
        }
        assert_eq!(api.selection(&Event::default()).unwrap(), None);
        api.set_item_enabled(mac_toolbox::MenuId(257), 1, false).unwrap();
        assert!(!api.item(mac_toolbox::MenuId(257), 1).unwrap().enabled);
        api.set_item_enabled(mac_toolbox::MenuId(257), 1, true).unwrap();
        api.set_item_checked(mac_toolbox::MenuId(257), 1, true).unwrap();
        assert!(api.item(mac_toolbox::MenuId(257), 1).unwrap().checked);
        let selection = api.selection(&mac_abi::Event { kind: EventKind::MenuCommand,
                                                        message: (257u32 << 16) | 1,
                                                        ..Default::default() }.into())
                           .unwrap()
                           .unwrap();
        assert_eq!((selection.menu_id, selection.item), (mac_toolbox::MenuId(257), 1));
        api.clear().unwrap();
        assert!(api.item_count(mac_toolbox::MenuId(257)).is_err());
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
    let native_keys = report["native_menu_keys"].as_array().unwrap();
    assert_eq!(native_keys.len(), 2, "disabled Ctrl-O/Ctrl-W must reach the app exactly once each");
    for (event, character) in native_keys.iter().zip(*b"ow")
    {
        let fields: std::collections::HashMap<_, _> =
            event.as_str()
                 .unwrap()
                 .split_whitespace()
                 .skip(1)
                 .map(|field| field.split_once('=').unwrap())
                 .collect();
        assert_eq!(fields["kind"], "3", "disabled equivalent must remain KeyDown");
        assert_eq!(fields["character"].parse::<u8>().unwrap(), character);
        assert_ne!(fields["modifiers"].parse::<u16>().unwrap()
                   & mac_toolbox::events::classic::CMD_KEY,
                   0);
        assert_eq!(fields["selection"], "false", "disabled equivalent became menu selection");
    }
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
    std::fs::write(output.join("rust-acceptance.json"),serde_json::to_vec_pretty(&serde_json::json!({"status":"passed","transport":"public SDK-only guest / loaded MDEF / real virtio input", "commands":2,"disabled_native_keys_exactly_once":2,"frames":14,"original_uncached_sizes":8,"native_keyed_pixels":2000,"original_exact_rgb_pixels":111184,"native_keyless_label_pixels":width*16,"scope":"click/key selection, disabled key/item/cancel/barswitch, exact restoration, three original File popup crops, original normal Apple title and selected File title; native authored label matches public font rendering"})).unwrap()).unwrap();
}


#[test]
fn queued_command_keys_resolve_using_current_resource_menu_policy()
{
    let fork = include_bytes!("data/original-menus.rsrc");
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("data/original-menu-metadata.json")).unwrap();
    let menu = oracle["menus"].as_array().unwrap().iter().find(|m| m["id"] == 257).unwrap();
    let item =
        menu["items"].as_array().unwrap().iter().find(|i| i["key"].as_u64().unwrap() != 0).unwrap();
    let index = item["index"].as_u64().unwrap() as u16;
    let event = mac_abi::Event { kind: EventKind::KeyDown,
                                 message: item["key"].as_u64().unwrap() as u32,
                                 modifiers: mac_toolbox::events::classic::CMD_KEY,
                                 ..Default::default() };
    mac_system::testing::menus::with_resources(fork, fork, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let menus = system.menus();
        menus.install_menu_resource(mac_toolbox::ResourceScope::from_raw(mac_abi::resources::APPLICATION).unwrap(), 257).unwrap();
        menus.set_item_enabled(mac_toolbox::MenuId(257), index, false).unwrap();
        assert_eq!(menus.selection(&event.into()).unwrap(), None);
        // The key was already queued while disabled. Prior selection handling
        // enables the item before this same event reaches the application's loop.
        menus.set_item_enabled(mac_toolbox::MenuId(257), index, true).unwrap();
        let selected = menus.selection(&event.into()).unwrap().unwrap();
        assert_eq!((selected.menu_id, selected.item), (mac_toolbox::MenuId(257), index));
        assert_eq!(menus.selection(&mac_abi::Event { kind: EventKind::KeyUp,
                                                     ..event }.into())
                        .unwrap(),
                   None);
        assert_eq!(menus.selection(&mac_abi::Event { modifiers: 0,
                                                     ..event }.into())
                        .unwrap(),
                   None);
        menus.set_item_enabled(mac_toolbox::MenuId(257), index, false).unwrap();
        assert_eq!(menus.selection(&event.into()).unwrap(), None);
    });
}

#[test]
fn owned_resource_menus_reject_stale_foreign_and_cleared_handles()
{
    let resource = mac_formats::resource::Resource { kind:       *b"MENU",
                                                     id:         257,
                                                     name:       None,
                                                     attributes: 0,
                                                     data:
                                                         include_bytes!("data/original-menu-257.\
                                                                         bin").to_vec() };
    let fork = mac_formats::resource::encode(&[resource]).unwrap();
    mac_system::testing::menus::with_resources(&fork, &fork, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        conformance::ownership::menus(&system, 257).unwrap();
    });
}
#[allow(dead_code)]
mod conformance;

#[test]
fn dynamic_document_items_remeasure_and_failed_changes_leave_menu_usable()
{
    use mac_toolbox::{Error, MenuId, resources};
    let fork = include_bytes!("data/original-menus.rsrc");
    mac_system::testing::menus::with_resources(fork, fork, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let mut menu = system.menus().install_owned(resources::APPLICATION, 257).unwrap();
        menu.truncate(0).unwrap();
        menu.set_item(1, "Untitled", 100, true, true).unwrap();
        let short = menu.layout().unwrap();
        menu.set_item(2, "A much longer document name — café", 101, true, false).unwrap();
        let wide = menu.layout().unwrap();
        assert!(wide.popup.right - wide.popup.left > short.popup.right - short.popup.left);
        assert!(wide.popup.bottom > short.popup.bottom);
        assert_eq!(menu.item_count().unwrap(), 2);
        assert!(menu.item(1).unwrap().checked);
        assert_eq!(menu.item(2).unwrap().command, 101);
        let failure = menu.set_item(4, "Skipped index", 0, true, false).unwrap_err();
        assert_eq!(failure, Error::BadArgument);
        assert!(failure.to_string().contains("argument"));
        assert_eq!(menu.layout().unwrap(), wide);
        assert_eq!(menu.truncate(3), Err(Error::BadArgument));
        assert_eq!(menu.item_count().unwrap(), 2);
        assert_eq!(menu.set_item(2,
                                 &"x".repeat(mac_toolbox::abi::INLINE_STRING_BYTES + 1),
                                 0,
                                 true,
                                 false),
                   Err(Error::BadArgument));
        assert_eq!(menu.item(2).unwrap().command, 101);
        menu.set_item(1, "Renamed", 100, true, false).unwrap();
        menu.set_item_checked(2, true).unwrap();
        assert!(!menu.item(1).unwrap().checked);
        assert!(menu.item(2).unwrap().checked);
        menu.truncate(1).unwrap();
        assert_eq!(menu.item_count().unwrap(), 1);
        assert_eq!(system.menus().item(MenuId(257), 2), Err(Error::NotFound));
        menu.dispose().unwrap();
        assert_eq!(menu.set_item(1, "Stale", 0, true, false), Err(Error::NotFound));
    });
}

fn capacity_fork() -> Vec<u8>
{
    let resources: Vec<_> = (32000i16..32012).map(|id| {
                                                 // Original MENU header + Pascal title + empty
                                                 // item-list terminator.
                                                 let mut data = id.to_be_bytes().to_vec();
                                                 data.extend_from_slice(&[0; 6]);
                                                 data.extend_from_slice(&1u32.to_be_bytes());
                                                 data.extend_from_slice(&[1, b'M', 0]);
                                                 mac_formats::resource::Resource { kind: *b"MENU",
                                                                                   id,
                                                                                   name: None,
                                                                                   attributes: 0,
                                                                                   data }
                                             })
                                             .collect();
    mac_formats::resource::encode(&resources).unwrap()
}

#[test]
fn registered_menus_grow_reuse_slots_and_reject_dead_full_width_handles()
{
    let fork = capacity_fork();
    mac_system::testing::menus::with_resources(&fork, &fork, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        conformance::ownership::menu_capacity(&system).unwrap();
        for id in [mac_toolbox::menus::MenuHandle(0), mac_toolbox::menus::MenuHandle(u64::MAX)]
        {
            let error = system.menus().menu(id).item_count().unwrap_err();
            assert_eq!(error, mac_toolbox::Error::NotFound);
            assert!(error.to_string().contains("not found"));
        }
    });
}

#[test]
fn foreign_menu_handles_cannot_cross_domains_and_the_registered_walk_releases_payload_and_backing()
{
    use mac_system::testing::{files::with_owner, inspection as fixture};
    use mac_toolbox::{Error, ResourceScope, inspection::Scope};
    let fork = capacity_fork();
    mac_system::testing::menus::with_resources(&fork, &fork, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let own = system.menus().install_owned(ResourceScope::Application, 32000).unwrap();
        let other = fixture::add("Other menu owner", 0);
        let foreign = with_owner(other, || {
            system.menus().install_owned(ResourceScope::Application, 32000).unwrap().into_handle()
        });
        assert_ne!(own.handle(), foreign);
        let error = system.menus().menu(foreign).layout().unwrap_err();
        assert_eq!(error, Error::NotFound);
        assert!(error.to_string().contains("not found"));
        with_owner(other, || {
            assert_eq!(system.menus().menu(own.handle()).item_count(), Err(Error::NotFound));
            assert_eq!(system.menus().menu(foreign).item_count().unwrap(), 0);
            let snapshot = system.inspection().snapshot(Scope::Own).unwrap().ledger(0).unwrap();
            assert_eq!(snapshot.domain().live, 2);
            assert_eq!(snapshot.len(), 2);
            let kinds: Vec<_> = (0..snapshot.len()).map(|i| snapshot.at(i).unwrap().kind).collect();
            assert!(kinds.contains(b"MENU") && kinds.contains(b"MNST"));
        });
        let outcome = fixture::walk_registered(other, false, false);
        assert!(outcome.failures.is_empty());
        assert_eq!((outcome.report.released, outcome.report.quarantined), (2, 0));
        with_owner(other, || {
            assert_eq!(system.menus().menu(foreign).layout(), Err(Error::NotFound));
            let snapshot = system.inspection().snapshot(Scope::Own).unwrap().ledger(0).unwrap();
            assert_eq!((snapshot.domain().live, snapshot.domain().pending), (0, 0));
        });
        assert!(own.layout().is_ok(), "another domain's teardown changed our menu");
        drop(own);
    });
}
