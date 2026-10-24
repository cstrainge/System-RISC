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

fn expected_frame(scene: &[u32], width: usize, height: usize, point: Point,
                  cursor: mac_toolbox::quickdraw::cursors::CursorSnapshot, colors: Option<&[u32]>)
                  -> Vec<u32>
{
    let mut expected = scene.to_vec();
    if cursor.flags & CURSOR_VISIBLE == 0
    {
        return expected;
    }
    for y in 0..height
    {
        for x in 0..width
        {
            let column = x as i32 - i32::from(point.h) + i32::from(cursor.cursor.hotspot.h);
            let row = y as i32 - i32::from(point.v) + i32::from(cursor.cursor.hotspot.v);
            if !(0..16).contains(&column) || !(0..16).contains(&row)
            {
                continue;
            }
            let index = y * width + x;
            let bit = 0x8000 >> column;
            let mask = cursor.cursor.mask[row as usize] & bit != 0;
            let image = cursor.cursor.image[row as usize] & bit != 0;
            if let Some(colors) = colors
            {
                let color = colors[row as usize * 16 + column as usize] & 0xffffff;
                expected[index] = if mask { color } else { scene[index] ^ (!color & 0xffffff) };
            }
            else
            {
                expected[index] = match (mask, image)
                {
                    (true, true) => 0,
                    (true, false) => 0xffffff,
                    (false, true) => scene[index] ^ 0xffffff,
                    (false, false) => scene[index]
                };
            }
        }
    }
    expected
}

#[test]
fn pointer_presents_preserve_the_scene_and_original_pixels_across_damage_and_visibility()
{
    use mac_system::testing::presentation::{pointer, present};
    corpus([], |system| {
        let cursor = system.quickdraw().cursors();
        let full = Rect::new(0, 0, 64, 48);
        let mut scene: Vec<u32> = (0..64 * 48).map(|i| (i * 7919) as u32 & 0xffffff).collect();
        let mut front = vec![0; scene.len()];
        let mut old = Rect::ZERO;
        for point in [Point { h: 20,
                              v: 20 },
                      Point { h: 21,
                              v: 20 },
                      Point { h: 0,
                              v: 0 },
                      Point { h: 63,
                              v: 47 },
                      Point { h: -16,
                              v: -16 },
                      Point { h: i16::MIN,
                              v: i16::MIN },
                      Point { h: i16::MAX,
                              v: i16::MAX },
                      Point { h: 35,
                              v: 25 }]
        {
            pointer(point);
            let state = cursor.info().unwrap();
            let left = point.h.saturating_sub(state.cursor.hotspot.h);
            let top = point.v.saturating_sub(state.cursor.hotspot.v);
            let next = Rect::new(left, top, left.saturating_add(16), top.saturating_add(16));
            let before = scene.clone();
            if old == Rect::ZERO
            {
                present(&scene, &mut front, 64, 48, [full]);
            }
            else
            {
                present(&scene, &mut front, 64, 48, [old, next, next]);
            }
            assert_eq!(scene, before, "a pointer-only present wrote the retained scene");
            assert_eq!(front,
                       expected_frame(&scene, 64, 48, point, state, None),
                       "pointer {point:?}");
            old = next;
        }
        // Damage under a stationary cursor: publish new scene pixels and sprite
        // together, including inversion bits, without a separate cursor repaint.
        let point = system.events().mouse().unwrap();
        scene[25 * 64 + 35..25 * 64 + 40].fill(0x2468ac);
        present(&scene, &mut front, 64, 48, [Rect::new(35, 25, 40, 26)]);
        assert_eq!(front, expected_frame(&scene, 64, 48, point, cursor.info().unwrap(), None));
        for standard in [StandardCursor::IBeam, StandardCursor::Crosshair, StandardCursor::Arrow]
        {
            cursor.set_standard(standard).unwrap();
            present(&scene, &mut front, 64, 48, [full]);
            assert_eq!(front, expected_frame(&scene, 64, 48, point, cursor.info().unwrap(), None));
        }
        cursor.hide().unwrap();
        present(&scene, &mut front, 64, 48, [full]);
        assert_eq!(front, scene);
        cursor.show().unwrap();
        present(&scene, &mut front, 64, 48, [full]);
        assert_eq!(front, expected_frame(&scene, 64, 48, point, cursor.info().unwrap(), None));
    });
}

#[test]
fn present_preserves_all_mono_and_color_mask_operations_without_double_inverting_overlaps()
{
    use mac_system::testing::presentation::{pointer, present};
    corpus([], |system| {
        let cursor = system.quickdraw().cursors();
        let image = MonoCursor { image:   [0x5555; 16],
                                 mask:    [0x3333; 16],
                                 hotspot: Point { h: 7,
                                                  v: 5 } };
        let point = Point { h: 15,
                            v: 12 };
        let scene: Vec<u32> = (0..32 * 32).map(|i| 0xa0000000 | (i * 4129) as u32).collect();
        let full = Rect::new(0, 0, 32, 32);
        pointer(point);
        cursor.set(image).unwrap();
        let mut front = vec![0; scene.len()];
        present(&scene, &mut front, 32, 32, [full, Rect::new(9, 9, 14, 14)]);
        assert_eq!(front, expected_frame(&scene, 32, 32, point, cursor.info().unwrap(), None));
        let colors: Vec<u32> = (0..256).map(|i| 0xff000000 | (i * 65539) as u32).collect();
        cursor.set_color(image, &colors).unwrap();
        present(&scene, &mut front, 32, 32, [full, Rect::new(9, 9, 14, 14)]);
        assert_eq!(front,
                   expected_frame(&scene, 32, 32, point, cursor.info().unwrap(), Some(&colors)));
        let before = cursor.info().unwrap();
        let error = cursor.set_color(image, &colors[..255]).unwrap_err();
        assert_eq!(error, mac_toolbox::Error::BadArgument);
        assert!(!error.to_string().is_empty());
        assert_eq!(cursor.info().unwrap(), before);
        present(&scene, &mut front, 32, 32, [full]);
        assert_eq!(front, expected_frame(&scene, 32, 32, point, before, Some(&colors)));
    });
}

#[test]
fn invalid_present_extents_and_empty_damage_leave_both_buffers_untouched()
{
    use mac_system::testing::presentation::present;
    corpus([], |_| {
        let scene = vec![0x123456; 32 * 32];
        let mut front = vec![0xabcdef; 32 * 32];
        let before = front.clone();
        for (width, height) in [(usize::MAX, 2), (33, 32), (i16::MAX as usize + 1, 0)]
        {
            assert!(present(&scene,&mut front,width,height,[Rect::new(0,0,32,32)]).is_empty());
            assert_eq!(front, before);
        }
        assert!(present(&scene[..32], &mut front, 32, 32, [Rect::new(0, 0, 32, 32)]).is_empty());
        assert!(present(&scene, &mut front[..32], 32, 32, [Rect::new(0, 0, 32, 32)]).is_empty());
        assert!(present(&scene,&mut front,32,32,[Rect::ZERO,Rect::new(40,40,50,50)]).is_empty());
        assert_eq!(front, before);
        assert_eq!(scene, vec![0x123456; 32 * 32]);
    });
}
