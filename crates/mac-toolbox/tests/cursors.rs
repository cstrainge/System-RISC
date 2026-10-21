//! Public cursor services against the pinned original resource corpus.
#![cfg(not(target_arch = "riscv64"))]
use mac_abi::quickdraw::*;
use mac_formats::resource::{Resource, encode};
use mac_toolbox::{Point, Rect, System,
                  quickdraw::cursors::{MonoCursor, StandardCursor}};

fn corpus(extra: impl IntoIterator<Item = Resource>, run: impl FnOnce(&System<'_>))
{
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let provenance: serde_json::Value = serde_json::from_str(include_str!(
        "../../../system/assets/cursors/provenance.json")).unwrap();
    let mut system = Vec::new();
    let mut application = Vec::new();
    for entry in provenance["resources"].as_array().unwrap()
    {
        let kind: [u8; 4] = entry["type"].as_str().unwrap().as_bytes().try_into().unwrap();
        let resource =
            Resource { kind,
                       id: entry["id"].as_i64().unwrap() as i16,
                       attributes: entry["attributes"].as_u64().unwrap() as u8,
                       name: None,
                       data: std::fs::read(root.join(entry["asset"].as_str().unwrap())).unwrap() };
        if entry["package"] == "System"
        {
            system.push(resource);
        }
        else
        {
            application.push(resource);
        }
    }
    application.extend(extra);
    mac_system::testing::with_resource_forks(&encode(&system).unwrap(), &encode(&application).unwrap(),
        |services| run(&unsafe { System::from_raw(services) }.unwrap())).unwrap();
}

fn mono(bytes: &[u8]) -> MonoCursor
{
    let word = |at| u16::from_be_bytes([bytes[at], bytes[at + 1]]);
    MonoCursor { image:   core::array::from_fn(|i| word(i * 2)),
                 mask:    core::array::from_fn(|i| word(32 + i * 2)),
                 hotspot: Point { v: word(64) as i16,
                                  h: word(66) as i16 } }
}

fn color_resource(depth: usize, id: i16) -> Resource
{
    let stride = depth * 2;
    let count = 1 << depth;
    let pixels = 146;
    let table = pixels + stride * 16;
    let mut data = vec![0; table + 8 + count * 8];
    let word = |data: &mut [u8], at: usize, value: u16| {
        data[at..at + 2].copy_from_slice(&value.to_be_bytes());
    };
    let long = |data: &mut [u8], at: usize, value: usize| {
        data[at..at + 4].copy_from_slice(&(value as u32).to_be_bytes());
    };
    word(&mut data, 0, 0x8001);
    long(&mut data, 2, 96);
    long(&mut data, 6, pixels);
    data[20..88].copy_from_slice(include_bytes!("../../../system/assets/visual/43555253-2.bin"));
    word(&mut data, 100, 0x8000 | stride as u16);
    word(&mut data, 106, 16);
    word(&mut data, 108, 16);
    word(&mut data, 128, depth as u16);
    word(&mut data, 130, 1);
    word(&mut data, 132, depth as u16);
    long(&mut data, 138, table);
    word(&mut data, table + 6, count as u16 - 1);
    for index in 0..count
    {
        let at = table + 8 + index * 8;
        word(&mut data, at, index as u16);
        word(&mut data, at + 2, (index as u16) * 257);
        word(&mut data, at + 4, 0xffff);
        word(&mut data, at + 6, 0x4040);
    }
    for y in 0..16
    {
        for x in 0..16
        {
            let bit = x * depth;
            data[pixels + y * stride + bit / 8] |=
                (((x + y) % count) as u8) << (8 - depth - bit % 8);
        }
    }
    Resource { kind: *b"crsr",
               id,
               name: None,
               attributes: 0,
               data }
}

#[test]
fn indexed_color_cursor_formats_validate_before_changing_resident_state()
{
    let mut resources = Vec::new();
    for depth in [1, 2, 4, 8]
    {
        resources.push(color_resource(depth, depth as i16));
    }
    let mut missing_table = color_resource(8, 9);
    missing_table.data.truncate(146 + 256);
    resources.push(missing_table);
    let mut overlapping = color_resource(8, 10);
    overlapping.data[6..10].copy_from_slice(&96u32.to_be_bytes());
    resources.push(overlapping);
    let mut wrong_dimensions = color_resource(8, 11);
    wrong_dimensions.data[106..108].copy_from_slice(&15u16.to_be_bytes());
    resources.push(wrong_dimensions);
    // Color frames take precedence over a same-ID monochrome animation frame.
    let animated = vec![0, 2, 0, 0, 0, 1, 0, 0, 0, 4, 0, 0];
    resources.push(Resource { kind:       *b"acur",
                              id:         23,
                              name:       None,
                              attributes: 0,
                              data:       animated });
    corpus(resources, |system| {
        let cursor = system.quickdraw().cursors();
        for depth in [1, 2, 4, 8]
        {
            cursor.application().set_color_resource(depth).unwrap();
            let state = cursor.info().unwrap();
            assert_ne!(state.flags & CURSOR_COLOR, 0);
            assert_eq!(state.cursor.hotspot,
                       Point { h: 5,
                               v: 5 });
        }
        let before = cursor.info().unwrap();
        for bad in [9, 10, 11]
        {
            assert!(cursor.application().set_color_resource(bad).is_err());
            assert_eq!(cursor.info().unwrap(), before);
        }
        cursor.animate_resource(23, 0).unwrap();
        assert_ne!(cursor.info().unwrap().flags & CURSOR_COLOR, 0);
        cursor.advance_animation().unwrap();
        assert_eq!(cursor.info().unwrap().frame_index, 1);
        assert_eq!(cursor.info().unwrap().cursor.hotspot,
                   Point { h: 5,
                           v: 5 });
    });
}

#[test]
fn original_builtins_resources_and_application_lookup_are_exact()
{
    corpus([], |system| {
        let cursor = system.quickdraw().cursors();
        let arrow = mono(include_bytes!("../../../system/assets/cursors/arrow.curs"));
        assert_eq!(cursor.info().unwrap().cursor, arrow);
        assert_eq!(arrow.image[9], 0x7c00);
        for (mode, id, v, h) in [(StandardCursor::IBeam, 1, 4, 7),
                                 (StandardCursor::Crosshair, 2, 5, 5),
                                 (StandardCursor::Plus, 3, 8, 8),
                                 (StandardCursor::Watch, 4, 8, 8)]
        {
            cursor.set_standard(mode).unwrap();
            let state = cursor.info().unwrap();
            assert_eq!(state.cursor.hotspot,
                       Point { v,
                               h });
            let resource =
                system.resources()
                      .system()
                      .get(mac_toolbox::ResourceKey { kind: mac_toolbox::TypeCode::new(*b"CURS"),
                                                      id })
                      .unwrap();
            let mut bytes = [0; 68];
            assert_eq!(resource.read(0, &mut bytes).unwrap(), 68);
            assert_eq!(state.cursor, mono(&bytes));
            resource.release().unwrap();
            assert_eq!(state.flags & CURSOR_AUTOMATIC, 0);
        }
        for (scope, ids) in
            [(0, &[-20161, -6078, -6077, -6076, -6075, -6074, -6073, -6072, -5760, 1, 2, 3, 4][..]),
             (1, &[6500, 6501, 6502, 6503, 6504, 6505, 6506][..])]
        {
            for &id in ids
            {
                let (cursors, resources) = if scope == 0
                {
                    (cursor.system(), system.resources().system())
                }
                else
                {
                    (cursor.application(), system.resources().application())
                };
                let resource = resources.get(mac_toolbox::ResourceKey { kind: mac_toolbox::TypeCode::new(*b"CURS"),
                                                                        id })
                                        .unwrap();
                let mut bytes = [0; 68];
                resource.read(0, &mut bytes).unwrap();
                cursors.set_resource(id).unwrap();
                assert_eq!(cursor.info().unwrap().cursor, mono(&bytes), "CURS {id}");
                resource.release().unwrap();
            }
        }
        cursor.set_standard(StandardCursor::Arrow).unwrap();
        assert_eq!(cursor.info().unwrap().cursor, arrow);
        assert_ne!(cursor.info().unwrap().flags & CURSOR_AUTOMATIC, 0);
        assert!(cursor.system().set_resource(0).is_err(), "arrow is a global, not CURS0");
    });
}

#[test]
fn visibility_nesting_obscuring_and_shield_offset_follow_quickdraw()
{
    corpus([], |system| {
        let cursor = system.quickdraw().cursors();
        cursor.hide().unwrap();
        cursor.hide().unwrap();
        assert_eq!(cursor.visibility().unwrap(), -2);
        cursor.show().unwrap();
        assert_eq!(cursor.visibility().unwrap(), -1);
        cursor.show().unwrap();
        cursor.show().unwrap();
        assert_eq!(cursor.visibility().unwrap(), 0);
        cursor.obscure().unwrap();
        cursor.show().unwrap();
        let state = cursor.info().unwrap();
        assert_eq!(state.visibility, 0);
        assert_ne!(state.flags & CURSOR_OBSCURED, 0);
        assert_eq!(state.flags & CURSOR_VISIBLE, 0);
        cursor.init().unwrap();
        let mouse = system.events().mouse().unwrap();
        // Offset is subtracted: this rect covers the current arrow after conversion.
        cursor.shield(Rect::new(mouse.h + 100, mouse.v + 100, mouse.h + 116, mouse.v + 116),
                      Point { h: 100,
                              v: 100 })
              .unwrap();
        let state = cursor.info().unwrap();
        assert_eq!(state.visibility, -1);
        assert_eq!(state.flags & (CURSOR_VISIBLE | CURSOR_DEFERRED_HIDE), 0);
        cursor.init().unwrap();
        cursor.shield(Rect::new(200, 200, 216, 216), Point::default()).unwrap();
        let state = cursor.info().unwrap();
        assert_eq!(state.visibility, -1, "nonintersecting ShieldCursor still nests");
        assert_ne!(state.flags & CURSOR_DEFERRED_HIDE, 0);
        assert_ne!(state.flags & CURSOR_VISIBLE, 0);
        cursor.hide().unwrap();
        assert_eq!(cursor.info().unwrap().flags & CURSOR_VISIBLE, 0);
        cursor.init().unwrap();
        assert_eq!(cursor.info().unwrap().visibility, 0);
    });
}

#[test]
fn original_acur_sequences_wrap_and_failures_preserve_the_previous_cursor()
{
    let broken = Resource { kind:       *b"acur",
                            id:         99,
                            name:       None,
                            attributes: 0,
                            data:       vec![0, 1, 0, 0, 0, 99, 0, 0] };
    corpus([broken], |system| {
        let cursors = system.quickdraw().cursors();
        for (id, frames, application) in
            [(-6078, [4, -6078, -6077, -6076, -6075, -6074, -6073, -6072], false),
             (6500, [4, 6500, 6501, 6502, 6503, 6504, 6505, 6506], true)]
        {
            // Finder's acur resolves its first CURS4 dependency from the System.
            let selected = if application { cursors.application() } else { cursors.system() };
            selected.animate_resource(id, 0).unwrap();
            for frame in 0..=16
            {
                let state = cursors.info().unwrap();
                assert_eq!(state.frame_index as usize, frame % 8);
                assert_eq!(state.frame_count, 8);
                assert_ne!(state.flags & CURSOR_ANIMATED, 0);
                let resource =
                    system.resources()
                          .get(mac_toolbox::ResourceKey { kind:
                                                              mac_toolbox::TypeCode::new(*b"CURS"),
                                                          id:   frames[frame % 8] })
                          .unwrap();
                let mut bytes = [0; 68];
                resource.read(0, &mut bytes).unwrap();
                assert_eq!(state.cursor, mono(&bytes));
                resource.release().unwrap();
                cursors.advance_animation().unwrap();
            }
        }
        let before = cursors.info().unwrap();
        assert!(cursors.animate_resource(99, 0).is_err());
        assert_eq!(cursors.info().unwrap(), before);
        assert!(cursors.set_resource(30000).is_err());
        assert_eq!(cursors.info().unwrap(), before);
        let mut invalid = before.cursor;
        invalid.hotspot.h = 16;
        assert!(cursors.set(invalid).is_err());
        assert_eq!(cursors.info().unwrap(), before);
        assert!(cursors.set_color(before.cursor, &[0; 255]).is_err());
        assert_eq!(cursors.info().unwrap(), before);
        cursors.set_color(before.cursor, &[0x123456; 256]).unwrap();
        assert_ne!(cursors.info().unwrap().flags & CURSOR_COLOR, 0);
        assert_eq!(cursors.info().unwrap().flags & CURSOR_ANIMATED, 0);
        cursors.init().unwrap();
        assert_eq!(cursors.info().unwrap().flags & CURSOR_ANIMATED, 0);
    });
}
