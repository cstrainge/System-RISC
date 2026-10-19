extern crate std;
use alloc::{string::ToString, vec::Vec};

use super::*;

fn bundle() -> FontManager
{
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
    let mut resources = Vec::new();
    for file in std::fs::read_dir(path).unwrap()
    {
        let file = file.unwrap().path();
        let stem = file.file_stem().unwrap().to_str().unwrap();
        let (kind, id) = stem.split_once('-').unwrap();
        let id: i16 = id.parse().unwrap();
        let name = match (kind, id)
        {
            ("FOND", 0) => Some("Chicago"),
            ("FOND", 2) => Some("New York"),
            ("FOND", 3) => Some("Geneva"),
            ("FOND", 4) => Some("Monaco"),
            ("FOND", 16) => Some("Palatino"),
            ("FOND", 20) => Some("Times"),
            ("FOND", 21) => Some("Helvetica"),
            ("FOND", 22) => Some("Courier"),
            ("FOND", 23) => Some("Symbol"),
            _ => None
        };
        resources.push(Resource { kind: kind.as_bytes().try_into().unwrap(),
                                  id,
                                  name: name.map(|name| name.as_bytes().to_vec()),
                                  attributes: 0,
                                  data: std::fs::read(file).unwrap() });
    }
    // Match packaged resource-map order, including FONT/NFNT ID collisions.
    resources.sort_by_key(|r| (r.kind, r.id));
    let manager = FontManager::take_resources(&mut resources);
    assert!(resources.is_empty());
    manager
}

#[test]
fn all_original_families_and_scalable_faces_are_available()
{
    let manager = bundle();
    assert_eq!(manager.catalog().len(), 9);
    for name in ["Chicago", "Geneva", "Monaco", "New York", "Palatino", "Times", "Helvetica",
                 "Courier", "Symbol"]
    {
        let family = manager.family_id(name).unwrap();
        assert_eq!(manager.family_name(family), Some(name));
        assert!(manager.metrics(FontSelection { family,
                                                size: 18,
                                                face: 0 })
                       .unwrap()
                       .ascent
                > 0);
        assert!(matches!(manager.source(FontSelection { family,
                                                        size: 17,
                                                        face: 0 },
                                        FontPreference::OutlinePreferred)
                                .unwrap(),
                         FontSource::TrueType { .. }));
    }
    let scalable: Vec<_> =
        manager.resources().iter().filter(|resource| resource.kind == *b"sfnt").collect();
    assert_eq!(scalable.len(), 17);
    for resource in scalable
    {
        let face = ttf_parser::Face::parse(&resource.data, 0).unwrap();
        assert!(face.number_of_glyphs() > 150);
        assert!(face.tables().glyf.is_some());
    }
}

#[test]
fn original_system_strikes_keep_exact_pixels_and_metrics()
{
    let manager = bundle();
    for (family, size, font) in
        [(0, 12, super::super::font::CHICAGO_12), (3, 9, super::super::font::GENEVA_9)]
    {
        let selection = FontSelection { family,
                                        size,
                                        face: 0 };
        assert_eq!(manager.text_width(selection, "TEST RISCy System éÅ").unwrap(),
                   font.width("TEST RISCy System éÅ"));
        let mut expected = alloc::vec![0xffffff;320*40];
        let mut actual = expected.clone();
        font.draw(&mut Canvas::new(&mut expected, 320, 40), 4, 24, "TEST RISCy System éÅ", 0);
        let width = manager.draw(selection,
                                 &mut Canvas::new(&mut actual, 320, 40),
                                 Point { h: 4,
                                         v: 24 },
                                 "TEST RISCy System éÅ",
                                 0)
                           .unwrap();
        assert_eq!(width, font.width("TEST RISCy System éÅ"));
        assert_eq!(expected, actual);
    }
    assert_eq!(manager.text_width(FontSelection::default(), "TEST"), Ok(26));
}

#[test]
fn scalable_simple_compound_and_mac_roman_text_draw_with_clipping()
{
    let manager = bundle();
    let selection = FontSelection { family: 20,
                                    size:   23,
                                    face:   0 };
    let mut pixels = alloc::vec![0xffffff;320*80];
    let mut canvas = Canvas::new(&mut pixels, 320, 80);
    canvas.set_origin(7, 3);
    canvas.set_clip(super::super::Rect::new(2, 2, 180, 50));
    let clip = canvas.clip();
    let text = "Hamburgefonts 0123 éÅ";
    let advance = manager.draw(selection,
                               &mut canvas,
                               Point { h: 0,
                                       v: 32 },
                               text,
                               0)
                         .unwrap();
    assert_eq!(advance, manager.text_width(selection, text).unwrap());
    assert_eq!(canvas.clip(), clip);
    assert_eq!(canvas.origin(),
               Point { h: 7,
                       v: 3 });
    assert!(canvas.pixels.iter().filter(|&&pixel| pixel == 0).count() > 100);
    for y in 0..80
    {
        for x in 0..320
        {
            if canvas.pixels[y * 320 + x] == 0
            {
                assert!((9..187).contains(&x) && (5..53).contains(&y));
            }
        }
    }
    // At least one of the accented original sfnt glyphs is compound; the
    // parser/rasterizer must traverse it rather than silently drawing nothing.
    let resource =
        manager.resources()
               .iter()
               .find(|r| {
                   r.kind == *b"sfnt"
                   && r.id
                      == match manager.source(selection, FontPreference::OutlinePreferred).unwrap()
                      {
                          FontSource::TrueType { id,
                                                 .. } => id,
                          _ => panic!()
                      }
               })
               .unwrap();
    let face = ttf_parser::Face::parse(&resource.data, 0).unwrap();
    assert!(face.glyph_bounding_box(super::outline::glyph(&face, 'é')).is_some());
    assert!(face.glyph_bounding_box(super::outline::glyph(&face, 'Å')).is_some());
}

#[test]
fn malformed_resources_unknown_families_and_unbounded_sizes_fail_safely()
{
    let manager = bundle();
    assert_eq!(manager.metrics(FontSelection { family: 900,
                                               size:   12,
                                               face:   0 }),
               Err(NOT_FOUND));
    for size in [-1, 513, i16::MAX]
    {
        assert_eq!(manager.text_width(FontSelection { family: 0,
                                                      size,
                                                      face: 0 },
                                      "TEST"),
                   Err(BAD_ARGUMENT));
    }
    assert_eq!(manager.family_id("gEnEvA"), Some(3));
    assert_eq!(manager.family_name(1), Some("Geneva"));
    let mut resources = alloc::vec![Resource { kind:       *b"FOND",
                                               id:         7,
                                               name:       Some("Broken".to_string().into_bytes()),
                                               attributes: 0,
                                               data:       alloc::vec![0;55] }];
    let manager = FontManager::take_resources(&mut resources);
    assert!(manager.catalog().is_empty());
}

#[test]
fn hinted_widths_and_metrics_match_original_system_runtime()
{
    // Independent original System7.6.1 QuickDrawProbe observations, retained in
    // results/quickdraw-reference/runtime.txt, page1 (SetOutlinePreferred true).
    let manager = bundle();
    for (family, size, width, ascent, descent, leading, max_width) in [(0, 12, 139, 12, 3, 1, 17),
                                                                       (3, 9, 109, 9, 2, 1, 12),
                                                                       (4, 9, 105, 9, 2, 1, 7),
                                                                       (4, 12, 147, 12, 3, 1, 10),
                                                                       (20, 12, 117, 9, 3, 0, 12),
                                                                       (20, 18, 177, 14, 4, 0, 18),
                                                                       (20, 24, 235, 18, 6, 0, 24),
                                                                       (21, 12, 131, 9, 3, 0, 13),
                                                                       (21, 18, 200, 14, 4, 0, 19),
                                                                       (22, 12, 147, 9, 3, 0, 10)]
    {
        let selection = FontSelection { family,
                                        size,
                                        face: 0 };
        let text = if family == 0 { "TEST RISCy System éÅ" } else { "Hamburgefonts 0123 éÅ" };
        assert_eq!(manager.text_width_with_preference(selection,
                                                      text,
                                                      FontPreference::OutlinePreferred),
                   Ok(width));
        assert_eq!(manager.metrics_with_preference(selection, FontPreference::OutlinePreferred),
                   Ok(FontMetrics { ascent,
                                    descent,
                                    leading,
                                    max_width }));
    }
    for (face, width, ascent, descent) in [(0, 177, 14, 4),
                                           (1, 189, 14, 4),
                                           (2, 177, 14, 4),
                                           (3, 183, 14, 4),
                                           (4, 177, 14, 4),
                                           (8, 198, 15, 5),
                                           (16, 198, 15, 6),
                                           (32, 156, 14, 4),
                                           (64, 198, 14, 4)]
    {
        let selection = FontSelection { family: 20,
                                        size: 18,
                                        face };
        assert_eq!(manager.text_width_with_preference(selection,
                                                      "Hamburgefonts 0123 éÅ",
                                                      FontPreference::OutlinePreferred),
                   Ok(width));
        let metrics =
            manager.metrics_with_preference(selection, FontPreference::OutlinePreferred).unwrap();
        assert_eq!((metrics.ascent, metrics.descent), (ascent, descent));
    }
}

#[test]
fn all_seventeen_original_sfnt_faces_execute_hint_programs_and_draw()
{
    let manager = bundle();
    let mut count = 0;
    for family in manager.catalog()
    {
        for association in family.faces.iter().filter(|face| face.size == 0)
        {
            let selection = FontSelection { family: family.id,
                                            size:   17,
                                            face:   association.face };
            let mut pixels = alloc::vec![0xffffff;280*40];
            let width = manager.draw_with_preference(selection,
                                                     &mut Canvas::new(&mut pixels, 280, 40),
                                                     Point { h: 4,
                                                             v: 27 },
                                                     "Hamburgefonts éÅ",
                                                     0,
                                                     FontPreference::OutlinePreferred)
                               .unwrap();
            assert!(width > 40);
            assert!(pixels.iter().filter(|&&color| color == 0).count() > 80);
            count += 1;
        }
    }
    assert_eq!(count, 17);
}

#[test]
fn actual_fond_styled_faces_take_priority_over_synthesis()
{
    let manager = bundle();
    let selection = FontSelection { family: 3,
                                    size:   9,
                                    face:   ITALIC };
    let source = manager.source(selection, FontPreference::BitmapPreferred).unwrap();
    let FontSource::Bitmap { kind,
                             id,
                             size,
                             face } = source
    else
    {
        panic!("Geneva9italic NFNT strike missing")
    };
    assert_eq!((kind, size, face), (*b"NFNT", 9, ITALIC));
    let resource = manager.resources()
                          .iter()
                          .find(|resource| resource.kind == kind && resource.id == id)
                          .unwrap();
    let font = BitmapFont::parse(&resource.data).unwrap();
    let mut expected = alloc::vec![0xffffff;220*32];
    let mut actual = expected.clone();
    font.draw(&mut Canvas::new(&mut expected, 220, 32), 4, 22, "Geneva italic éÅ", 0);
    manager.draw(selection,
                 &mut Canvas::new(&mut actual, 220, 32),
                 Point { h: 4,
                         v: 22 },
                 "Geneva italic éÅ",
                 0)
           .unwrap();
    assert_eq!(actual, expected);
    for face in [BOLD, ITALIC, BOLD | ITALIC]
    {
        assert!(matches!(manager.source(FontSelection{family:20,size:18,face},FontPreference::OutlinePreferred).unwrap(),FontSource::TrueType{face:actual,..} if actual==face));
    }
}
