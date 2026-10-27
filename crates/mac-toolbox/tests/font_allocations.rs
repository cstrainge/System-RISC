//! Public Font Manager calls preserve typed exhaustion and retry after refusal.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/allocations.rs"]
mod allocations;
use std::{fs, path::Path, sync::OnceLock};

use allocations::Observe;
use mac_formats::resource::{Resource, encode};
use mac_toolbox::{Error, Point, Rect, System,
                  quickdraw::fonts::{BOLD, FontSelection, UNDERLINE}};

fn font_fork() -> &'static [u8]
{
    static FORK: OnceLock<Vec<u8>> = OnceLock::new();
    FORK.get_or_init(|| {
            let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../system/assets/fonts");
            let mut resources = Vec::new();
            for file in fs::read_dir(directory).unwrap()
            {
                let file = file.unwrap().path();
                let (kind, id) =
                    file.file_stem().unwrap().to_str().unwrap().split_once('-').unwrap();
                resources.push(Resource { kind:       kind.as_bytes().try_into().unwrap(),
                                          id:         id.parse().unwrap(),
                                          name:       None,
                                          attributes: 0,
                                          data:       fs::read(&file).unwrap() });
            }
            resources.sort_by_key(|row| (row.kind, row.id));
            encode(&resources).unwrap()
        })
}
fn with_fonts(run: impl FnOnce(&System<'_>))
{
    let application = encode(&[]).unwrap();
    mac_system::testing::with_resource_forks(font_fork(), &application, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        run(&system);
    }).unwrap();
}
#[test]
fn exact_bitmap_strike_queries_and_draw_remain_allocation_free()
{
    with_fonts(|system| {
        let fonts = system.quickdraw().fonts();
        let world = system.quickdraw().new_gworld(96, 48).unwrap();
        let port = world.graphics();
        let font = FontSelection { family: 0,
                                   size:   12,
                                   face:   0 };
        let clip = Rect::new(5, 5, 40, 32);
        port.clip_rect(clip).unwrap();
        let observed = Observe::begin(true, None);
        let width = fonts.text_width(font, "TEST éÅ");
        let metrics = fonts.metrics(font);
        let drawn = port.text(font,
                              Point { h: 2,
                                      v: 24 },
                              "TEST éÅ",
                              0x123456);
        let missing = fonts.text_width(FontSelection { family: 30000,
                                                       ..font },
                                       "g");
        let invalid = fonts.text_width(FontSelection { size: 513,
                                                       ..font },
                                       "g");
        let allocations = observed.finish();
        assert_eq!(allocations, 0, "exact strikes must remain usable without scratch backing");
        assert!(metrics.unwrap().ascent > 0);
        assert_eq!(drawn, width);
        assert!(width.unwrap() > 0);
        assert_eq!(missing, Err(Error::NotFound));
        assert_eq!(invalid, Err(Error::BadArgument));
        let mut pixels = [0; 96 * 48];
        world.pixels(&mut pixels).unwrap();
        assert!(pixels.contains(&0x123456));
        for y in 0..48
        {
            for x in 0..96
            {
                if x < clip.left as usize
                   || x >= clip.right as usize
                   || y < clip.top as usize
                   || y >= clip.bottom as usize
                {
                    assert_eq!(pixels[y * 96 + x], 0xffffff);
                }
            }
        }
        assert!(Error::NotFound.to_string().contains("found"));
        assert!(Error::BadArgument.to_string().contains("argument"));
    });
}

fn refuse_every_draw_allocation(font: FontSelection, text: &str)
{
    with_fonts(|system| {
        let fonts = system.quickdraw().fonts();
        let world = system.quickdraw().new_gworld(64, 48).unwrap();
        let port = world.graphics();
        let bounds = Rect::new(0, 0, 64, 48);
        let baseline = Point { h: 8,
                               v: 32 };
        port.colors(0xffffff, 0xffffff).unwrap();
        let expected_width = fonts.text_width(font, text).unwrap();
        let observed = Observe::begin(false, None);
        let drawn = port.text(font, baseline, text, 0x123456);
        let count = observed.finish();
        assert_eq!(drawn, Ok(expected_width));
        assert!(count > 0, "this case must exercise owned raster/hint backing");
        let mut expected = [0; 64 * 48];
        world.pixels(&mut expected).unwrap();
        assert!(expected.contains(&0x123456));
        for ordinal in 1..=count
        {
            port.paint_rect(bounds).unwrap();
            let observed = Observe::fail_at(ordinal);
            let refused = port.text(font, baseline, text, 0x123456);
            let attempts = observed.finish();
            assert_eq!(refused,
                       Err(Error::NoMemory),
                       "font {font:?}, allocation {ordinal}/{count}");
            assert!(attempts >= ordinal);
            let mut actual = [0; 64 * 48];
            world.pixels(&mut actual).unwrap();
            assert_eq!(actual,
                       [0xffffff; 64 * 48],
                       "single glyph published partial pixels after admission refusal {ordinal}");
            assert_eq!(port.text(font, baseline, text, 0x123456), Ok(expected_width));
            world.pixels(&mut actual).unwrap();
            assert_eq!(actual, expected, "retry changed the admitted glyph at refusal {ordinal}");
        }
        assert!(Error::NoMemory.to_string().contains("memory"));
    });
}
#[test]
fn styled_bitmap_draw_refusals_preserve_pixels_and_retry_the_same_world()
{
    refuse_every_draw_allocation(FontSelection { family: 0,
                                                 size:   12,
                                                 face:   BOLD | UNDERLINE },
                                 "g");
}
#[test]
fn hinted_truetype_draw_refusals_preserve_pixels_and_retry_the_same_world()
{
    // There is no 23-point bitmap strike: this exercises the original sfnt's
    // TrueType instructions, supplied glyph scratch and monochrome rasterizer.
    let font = FontSelection { family: 20,
                               size:   23,
                               face:   UNDERLINE };
    for glyph in ["g", "é", "Å"]
    {
        refuse_every_draw_allocation(font, glyph);
    }
}
#[test]
fn hinted_truetype_measurement_preserves_exhaustion_identity_and_retries()
{
    with_fonts(|system| {
        let fonts = system.quickdraw().fonts();
        let font = FontSelection { family: 20,
                                   size:   23,
                                   face:   0 };
        let observed = Observe::begin(false, None);
        let expected = fonts.text_width(font, "géÅ");
        let count = observed.finish();
        assert!(expected.unwrap() > 0);
        assert!(count > 0);
        for ordinal in 1..=count
        {
            let observed = Observe::fail_at(ordinal);
            let refused = fonts.text_width(font, "géÅ");
            let attempts = observed.finish();
            assert_eq!(refused, Err(Error::NoMemory), "allocation {ordinal}/{count}");
            assert!(attempts >= ordinal);
            assert_eq!(fonts.text_width(font, "géÅ"), expected);
        }
        let observed = Observe::begin(true, None);
        let missing = fonts.text_width(FontSelection { family: 30000,
                                                       ..font },
                                       "g");
        let invalid = fonts.text_width(FontSelection { size: 513,
                                                       ..font },
                                       "g");
        let allocations = observed.finish();
        assert_eq!(missing, Err(Error::NotFound));
        assert_eq!(invalid, Err(Error::BadArgument));
        assert_eq!(allocations, 0, "bad arguments must fail before hint backing admission");
        assert!(Error::NoMemory.to_string().contains("memory"));
    });
}

#[test]
fn empty_hinted_text_validates_selection_without_scratch_or_pixel_changes()
{
    with_fonts(|system| {
        let fonts = system.quickdraw().fonts();
        let font = FontSelection { family: 20,
                                   size:   23,
                                   face:   0 };
        let world = system.quickdraw().new_gworld(32, 32).unwrap();
        let port = world.graphics();
        let observed = Observe::begin(true, None);
        let width = fonts.text_width(font, "");
        let drawn = port.text(font,
                              Point { h: 0,
                                      v: 24 },
                              "",
                              0);
        let missing = port.text(FontSelection { family: 30000,
                                                ..font },
                                Point::default(),
                                "",
                                0);
        let invalid = fonts.text_width(FontSelection { size: 513,
                                                       ..font },
                                       "");
        let allocations = observed.finish();
        assert_eq!(width, Ok(0));
        assert_eq!(drawn, Ok(0));
        assert_eq!(missing, Err(Error::NotFound));
        assert_eq!(invalid, Err(Error::BadArgument));
        assert_eq!(allocations, 0, "empty text must not prepare a hint interpreter or raster");
        let mut pixels = [0; 32 * 32];
        world.pixels(&mut pixels).unwrap();
        assert_eq!(pixels, [0xffffff; 32 * 32]);
    });
}
