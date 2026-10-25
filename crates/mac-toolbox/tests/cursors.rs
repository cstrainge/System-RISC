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

/// Independent scalar oracle: union-neighbour boundary and the frozen original
/// indexed XOR profile. This deliberately scans pixels, unlike production.
fn outlined_scene(scene: &[u32], width: usize, height: usize, outline: Option<(&[Rect], i32, i32)>)
                  -> Vec<u32>
{
    let mut result = scene.to_vec();
    let Some((pieces, dx, dy)) = outline
    else
    {
        return result;
    };
    let table = include_bytes!("../../../system/assets/windows/system7-indexed-xor.bin");
    let inside = |x: i64, y: i64| {
        pieces.iter().any(|r| {
                         let x = x - i64::from(dx);
                         let y = y - i64::from(dy);
                         x >= i64::from(r.left)
                         && x < i64::from(r.right)
                         && y >= i64::from(r.top)
                         && y < i64::from(r.bottom)
                     })
    };
    for y in 21..height
    {
        for x in 0..width
        {
            let (xx, yy) = (x as i64, y as i64);
            if (x + y) & 1 == 1
               && inside(xx, yy)
               && (!inside(xx - 1, yy)
                   || !inside(xx + 1, yy)
                   || !inside(xx, yy - 1)
                   || !inside(xx, yy + 1))
            {
                let rgb = scene[y * width + x];
                result[y * width + x] =
                    table.chunks_exact(8)
                         .find_map(|e| {
                             (u32::from_be_bytes(e[..4].try_into().unwrap()) == rgb)
                        .then(|| u32::from_be_bytes(e[4..].try_into().unwrap()))
                         })
                         .unwrap_or(rgb ^ 0xffffff);
            }
        }
    }
    result
}

#[test]
fn window_drag_xors_only_changed_frontbuffer_edges_and_preserves_cursor_layering()
{
    use mac_system::testing::presentation::{DragFrame, pointer};
    corpus([], |system| {
        let (width, height) = (96, 80);
        let full = Rect::new(0, 0, 96, 80);
        let mut colors = vec![0xffffff, 0, 0x123456, 0xedcba9, 0xa5a5a5, 0x0000ff, 0x00ffff];
        for entry in
            include_bytes!("../../../system/assets/windows/system7-indexed-xor.bin").chunks_exact(8)
        {
            let rgb = u32::from_be_bytes(entry[..4].try_into().unwrap());
            colors.extend([rgb, rgb ^ 0xffffff]);
        }
        let scene: Vec<_> =
            (0..width * height).map(|i| colors[(i * 7919) % colors.len()]).collect();
        let original = scene.clone();
        let pieces = [Rect::new(12, 25, 60, 65), Rect::new(30, 30, 80, 70)];
        let mut front = vec![0; scene.len()];
        let mut frame = DragFrame::default();
        let cursors = system.quickdraw().cursors();
        cursors.set(MonoCursor { image:   [0xffff; 16],
                                 mask:    [0; 16],
                                 hotspot: Point::default() })
               .unwrap();
        let mut point = Point { h: 20,
                                v: 25 };
        pointer(point);
        frame.present(&scene, &mut front, width, height, [full], None, true).unwrap();
        let mut previous_outline = None;
        for (index, (dx, dy)) in [(0, 0),
                                  (1, 0),
                                  (1, 1),
                                  (1, 1),
                                  (-35, -20),
                                  (40, 30),
                                  (i32::MIN, i32::MAX),
                                  (0, 0)].into_iter()
                                         .enumerate()
        {
            let outline = Some((pieces.as_slice(), dx, dy));
            let before = outlined_scene(&scene, width, height, previous_outline);
            let after = outlined_scene(&scene, width, height, outline);
            let expected_changes = before.iter().zip(&after).filter(|(a, b)| a != b).count();
            let old_front = front.clone();
            if index == 4
            {
                point = Point { h: 0,
                                v: 0 };
                pointer(point);
            }
            let (changed, copied) =
                frame.present(&scene, &mut front, width, height, [], outline, index == 4).unwrap();
            assert_eq!(changed, expected_changes, "perimeter symmetric difference {dx},{dy}");
            assert!(copied.iter()
                          .all(|r| {
                              (r.right - r.left) as usize * (r.bottom - r.top) as usize <= 256
                          }));
            if previous_outline == outline && index != 4
            {
                assert!(copied.is_empty(), "unchanged feedback did work");
                assert_eq!(front, old_front);
            }
            assert_eq!(front,
                       expected_frame(&after, width, height, point, cursors.info().unwrap(), None));
            assert_eq!(scene, original, "drag wrote the retained scene");
            previous_outline = outline;
        }
        cursors.hide().unwrap();
        frame.present(&scene, &mut front, width, height, [], None, true).unwrap();
        assert_eq!(front, original, "cancelling the drag or hiding cursor left stale XOR pixels");
    });
}

#[test]
fn scene_damage_under_a_held_window_drag_reapplies_only_the_intersection()
{
    use mac_system::testing::presentation::DragFrame;
    corpus([], |system| {
        system.quickdraw().cursors().hide().unwrap();
        let mut scene = vec![0xffffff; 96 * 80];
        let mut front = vec![0; scene.len()];
        let pieces = [Rect::new(12, 25, 80, 70)];
        let outline = Some((pieces.as_slice(), 0, 0));
        let mut frame = DragFrame::default();
        frame.present(&scene, &mut front, 96, 80, [Rect::new(0, 0, 96, 80)], outline, false)
             .unwrap();
        let damage = Rect::new(0, 24, 40, 40);
        let before = front.clone();
        for y in 24..40
        {
            for x in 0..40
            {
                scene[y * 96 + x] = 0x123456;
            }
        }
        let changed_scene = scene.clone();
        let (changed, copied) =
            frame.present(&scene, &mut front, 96, 80, [damage], outline, false).unwrap();
        assert_eq!(changed, 0, "stationary outline was toggled again");
        assert_eq!(copied, [damage], "damage expanded outside its rectangle");
        assert_eq!(front, outlined_scene(&scene, 96, 80, outline));
        for y in 0..80
        {
            for x in 0..96
            {
                if !damage.contains(Point { h: x as i16,
                                            v: y as i16 })
                {
                    assert_eq!(front[y * 96 + x], before[y * 96 + x], "paint escaped damage");
                }
            }
        }
        assert_eq!(scene, changed_scene);
        // A subsequent outline move must erase against the newly painted scene.
        let next = Some((pieces.as_slice(), 4, 3));
        frame.present(&scene, &mut front, 96, 80, [], next, false).unwrap();
        assert_eq!(front, outlined_scene(&scene, 96, 80, next));
        frame.present(&scene, &mut front, 96, 80, [], None, false).unwrap();
        assert_eq!(front, scene);
    });
}

#[test]
fn invalid_window_drag_surfaces_leave_frame_and_overlay_history_unchanged()
{
    use mac_system::testing::presentation::DragFrame;
    corpus([], |system| {
        system.quickdraw().cursors().hide().unwrap();
        let scene = vec![0x123456; 96 * 80];
        let mut front = scene.clone();
        let pieces = [Rect::new(12, 25, 80, 70)];
        let mut frame = DragFrame::default();
        for (width, height) in [(usize::MAX, 2), (96, 81), (32768, 0)]
        {
            assert!(frame.present(&scene,
                                  &mut front,
                                  width,
                                  height,
                                  [Rect::new(0, 0, 96, 80)],
                                  Some((&pieces, 0, 0)),
                                  false)
                         .is_none());
            assert_eq!(front, scene, "invalid dimensions partially published");
        }
        let (_, copies) = frame.present(&scene, &mut front, 96, 80, [], None, false).unwrap();
        assert!(copies.is_empty());
        assert_eq!(front, scene, "failed transition changed retained overlay state");
    });
}

fn feedback_scene(scene: &[u32], width: usize, height: usize,
                  outline: Option<(Rect, Option<&[Rect]>)>,
                  highlight: Option<(Rect, Option<&[Rect]>)>)
                  -> Vec<u32>
{
    let visible = |clips: Option<&[Rect]>, x: usize, y: usize| {
        y >= 21
        && clips.is_none_or(|rects| {
                    rects.iter().any(|r| {
                                    r.contains(Point { h: x as i16,
                                                       v: y as i16 })
                                })
                })
    };
    let mut pixels = scene.to_vec();
    if let Some((bounds, clips)) = highlight
    {
        for y in 0..height
        {
            for x in 0..width
            {
                let (xx, yy) = (x as i32, y as i32);
                let (l, t, r, b) = (i32::from(bounds.left),
                                    i32::from(bounds.top),
                                    i32::from(bounds.right),
                                    i32::from(bounds.bottom));
                if visible(clips, x, y)
                   && xx >= l
                   && xx < r
                   && yy >= t
                   && yy < b
                   && (xx < l + 2 || xx >= r - 2 || yy < t + 2 || yy >= b - 2)
                {
                    pixels[y * width + x] = 0x2222aa;
                }
            }
        }
    }
    if let Some((bounds, clips)) = outline
    {
        let outlined = outlined_scene(&pixels, width, height, Some((&[bounds], 0, 0)));
        for y in 0..height
        {
            for x in 0..width
            {
                if visible(clips, x, y)
                {
                    pixels[y * width + x] = outlined[y * width + x];
                }
            }
        }
    }
    pixels
}

#[test]
fn drag_feedback_layers_xor_at_present_with_cached_clipping_and_no_scene_writes()
{
    use mac_system::testing::presentation::{DragFrame, pointer};
    corpus([], |system| {
        let (width, height) = (96, 80);
        let full = Rect::new(0, 0, 96, 80);
        let scene: Vec<_> = (0..width * height).map(|i| {
                                                   match i % 4
                                                   {
                                                       0 => 0xffffff,
                                                       1 => 0x123456,
                                                       2 => 0,
                                                       _ => 0xa5a5a5
                                                   }
                                               })
                                               .collect();
        let original = scene.clone();
        let clip_a = [Rect::new(0, 21, 50, 80), Rect::new(60, 40, 96, 80)];
        let clip_b = [Rect::new(0, 21, 96, 30), Rect::new(0, 40, 96, 80)];
        let mut front = vec![0; scene.len()];
        let mut frame = DragFrame::default();
        let cursor = system.quickdraw().cursors();
        cursor.set(MonoCursor { image:   [0xffff; 16],
                                mask:    [0; 16],
                                hotspot: Point::default() })
              .unwrap();
        let point = Point { h: 22,
                            v: 25 };
        pointer(point);
        frame.present_feedback(&scene, &mut front, width, height, [full], None, None, true)
             .unwrap();
        let a = Some((Rect::new(20, 25, 80, 70), Some(clip_a.as_slice())));
        let b = Some((Rect::new(10, 23, 90, 75), Some(clip_b.as_slice())));
        let cases = [(None, a),
                     (Some((Rect::new(22, 24, 62, 58), None)), a),
                     (Some((Rect::new(24, 25, 64, 59), None)), a),
                     (Some((Rect::new(28, 28, 68, 62), None)), b),
                     (Some((Rect::new(28, 28, 68, 62), Some(clip_a.as_slice()))), b),
                     (Some((Rect::new(28, 28, 68, 62), Some(clip_b.as_slice()))), None),
                     (Some((Rect::new(-20, 0, 10, 45), None)),
                      Some((Rect::new(95, 79, 96, 80), None))),
                     (None, Some((Rect::new(i16::MIN, i16::MIN, i16::MAX, i16::MAX), None))),
                     (None, Some((Rect::new(i16::MAX, i16::MAX, i16::MAX, i16::MAX), None))),
                     (None, None)];
        for (outline, highlight) in cases
        {
            let (_, copies) = frame.present_feedback(&scene,
                                                     &mut front,
                                                     width,
                                                     height,
                                                     [],
                                                     outline,
                                                     highlight,
                                                     false)
                                   .unwrap();
            assert!(copies.iter()
                          .all(|r| {
                              (r.right - r.left) as usize * (r.bottom - r.top) as usize <= 256
                          }));
            let expected = feedback_scene(&scene, width, height, outline, highlight);
            assert_eq!(front,
                       expected_frame(&expected,
                                      width,
                                      height,
                                      point,
                                      cursor.info().unwrap(),
                                      None),
                       "feedback composition/layering differs for {outline:?}, {highlight:?}");
            assert_eq!(scene, original, "drag feedback wrote into retained pixels");
            let before = front.clone();
            let (changed, copies) = frame.present_feedback(&scene,
                                                           &mut front,
                                                           width,
                                                           height,
                                                           [],
                                                           outline,
                                                           highlight,
                                                           false)
                                         .unwrap();
            assert_eq!(changed, 0, "unchanged feedback repeated XOR work");
            assert!(copies.is_empty(), "unchanged feedback repeated framebuffer copies");
            assert_eq!(front, before);
        }
        cursor.hide().unwrap();
        frame.present_feedback(&scene, &mut front, width, height, [], None, None, true).unwrap();
        assert_eq!(front, original, "cancel left an outline, target highlight or cursor behind");
    });
}

#[test]
fn paint_under_file_drag_feedback_is_bounded_and_invalid_clips_do_not_publish()
{
    use mac_system::testing::presentation::DragFrame;
    corpus([], |system| {
        system.quickdraw().cursors().hide().unwrap();
        let (width, height) = (96, 80);
        let mut scene = vec![0xffffff; width * height];
        let mut front = scene.clone();
        let mut frame = DragFrame::default();
        let clips = [Rect::new(10, 21, 90, 75)];
        let outline = Some((Rect::new(8, 25, 75, 65), Some(clips.as_slice())));
        let highlight = Some((Rect::new(20, 24, 80, 70), None));
        frame.present_feedback(&scene, &mut front, width, height, [], outline, highlight, false)
             .unwrap();
        let damage = Rect::new(15, 21, 50, 45);
        for y in 21..45
        {
            for x in 15..50
            {
                scene[y * width + x] = 0x123456;
            }
        }
        let before = front.clone();
        let (changed, copied) = frame.present_feedback(&scene,
                                                       &mut front,
                                                       width,
                                                       height,
                                                       [damage],
                                                       outline,
                                                       highlight,
                                                       false)
                                     .unwrap();
        assert_eq!(changed, 0, "stationary feedback was toggled on paint");
        assert_eq!(copied, [damage], "paint under feedback expanded its damage");
        assert_eq!(front, feedback_scene(&scene, width, height, outline, highlight));
        for y in 0..height
        {
            for x in 0..width
            {
                if !damage.contains(Point { h: x as i16,
                                            v: y as i16 })
                {
                    assert_eq!(front[y * width + x], before[y * width + x]);
                }
            }
        }
        let snapshot = front.clone();
        let overlapping = [Rect::new(0, 21, 70, 80), Rect::new(20, 25, 90, 75)];
        assert!(frame.present_feedback(&scene,
                                       &mut front,
                                       width,
                                       height,
                                       [],
                                       Some((Rect::new(0, 21, 96, 80), Some(&overlapping))),
                                       None,
                                       false)
                     .is_none(),
                "capture transport accepted ambiguous overlapping clip fragments");
        assert_eq!(front, snapshot, "invalid clip partially published");
        let (changed, copies) = frame.present_feedback(&scene,
                                                       &mut front,
                                                       width,
                                                       height,
                                                       [],
                                                       outline,
                                                       highlight,
                                                       false)
                                     .unwrap();
        assert_eq!(changed, 0, "invalid request damaged the previous feedback state");
        assert!(copies.is_empty());
        frame.present_feedback(&scene, &mut front, width, height, [], None, None, false).unwrap();
        assert_eq!(front, scene, "erasing feedback used stale pixels after a paint");
    });
}
