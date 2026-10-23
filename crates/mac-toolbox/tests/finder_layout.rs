//! Finder caption geometry; native end-to-end coverage lives in finder_selection.
#![cfg(not(target_arch = "riscv64"))]
#[allow(dead_code)]
#[path = "../../../apps/finder/src/icon_view.rs"]
mod icon_view;
#[allow(dead_code)]
#[path = "../../../apps/finder/src/browser.rs"]
mod browser;
#[allow(dead_code)]
#[path = "../../../apps/finder/src/cache.rs"]
mod cache;
#[allow(dead_code)]
#[path = "../../../apps/finder/src/selection.rs"]
mod selection;
#[allow(dead_code)]
#[path = "../../../apps/finder/src/ui.rs"]
mod ui;
#[allow(dead_code)]
#[path = "../../../apps/finder/src/menus.rs"]
mod menus;
use icon_view::{CAPTION_WIDTH, Caption, Layout};
use mac_toolbox::{Error, Rect};

#[test]
#[ignore = "requires the generated System resource fork with its real Geneva fonts"]
fn installed_application_captions_with_real_geneva_metrics_do_not_overlap()
{
    use mac_toolbox::{System, quickdraw::fonts::FontSelection};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
                                                                   .canonicalize()
                                                                   .unwrap();
    let resources = std::fs::read(mac_build::paths::dist_dir(&root).join("System.rsrc")).unwrap();
    let empty = mac_formats::resource::encode(&[]).unwrap();
    mac_system::testing::with_resource_forks(&resources, &empty, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let mut layout = Layout::new(404);
        let mut occupied = Vec::new();
        for name in ["Cursor Test", "Panic Test", "QuickDraw Test", "Reactor Room", "Res Edit",
                     "Text Edit", "Z QuickDraw Copy"]
        {
            let caption = Caption::measure(name, |text| {
                              system.quickdraw().fonts().text_width(FontSelection { family: 3,
                                                                                    size:   9,
                                                                                    face:   0 },
                                                                    text)
                          }).unwrap();
            let icon = layout.place(caption);
            let center = (icon.left + icon.right) / 2;
            let bounds = Rect::new((center - (caption.width() + 1) / 2 - 2).min(icon.left),
                                   icon.top,
                                   (center + caption.width() / 2 + 2).max(icon.right),
                                   icon.bottom + caption.height());
            for previous in &occupied
            {
                assert!(system.quickdraw().intersect(bounds, *previous).unwrap().is_none(),
                        "caption collision for {name}");
            }
            occupied.push(bounds);
            println!("APPLICATION_POINT name={name:?} x={} y={}",
                     24 + center,
                     62 + (icon.top + icon.bottom) / 2);
        }
    }).unwrap();
}
fn width(text: &str) -> Result<i16, Error>
{
    Ok(text.chars().filter(|&c| c != '\u{301}').count() as i16 * 6)
}
#[test]
fn captions_wrap_losslessly_without_splitting_utf8_or_combining_clusters()
{
    for name in ["Short",
                 "A fairly long filename with spaces and extension.txt",
                 "x".repeat(255).as_str(),
                 "Éléphant 🦀 Éléphant 🦀 Éléphant 🦀",
                 "e\u{301}".repeat(85).as_str(),
                 " ".repeat(255).as_str(),
                 ""]
    {
        let caption = Caption::measure(name, width).unwrap();
        let lines: Vec<_> = caption.lines(name).collect();
        assert_eq!(lines.iter().map(|&(s, _)| s).collect::<String>(), name);
        assert!(caption.width() <= CAPTION_WIDTH);
        assert!(lines.iter().all(|&(s, w)| width(s).unwrap() == w && w <= CAPTION_WIDTH));
        assert!(lines.iter().all(|&(s, _)| !s.starts_with('\u{301}')));
        assert_eq!(caption.height(), lines.len() as i16 * 12);
    }
}
#[test]
fn automatic_rows_reserve_full_wrapped_height_and_keep_short_name_pixels()
{
    let short = Caption::measure("Item 00", width).unwrap();
    let long = Caption::measure(&"word ".repeat(20), width).unwrap();
    let mut layout = Layout::new(150);
    let first = layout.place(short);
    assert_eq!(first, Rect::new(16, 28, 48, 60));
    let second = layout.place(short);
    assert_eq!(second, Rect::new(80, 28, 112, 60));
    assert_eq!(layout.place(short).top, 80);
    let mut layout = Layout::new(80);
    let first = layout.place(long);
    assert!(long.bounds(first).left >= 0);
    let second = layout.place(short);
    assert!(second.top >= long.bounds(first).bottom + 8);
    let near_limit = short.bounds(Rect::new(32720, 32700, 32752, 32732));
    assert!(near_limit.left > 32000);
}

#[test]
fn cached_interval_queries_include_tall_captions_below_shorter_neighbors()
{
    cache::edit(0, |folder| {
        folder.count = 3;
        folder.width = 0;
        for index in 0..3
        {
            let item = &mut folder.items[index];
            item.entry = mac_toolbox::files::CatalogInfo::EMPTY;
            item.entry.finder_info[8..10].copy_from_slice(&0x0100u16.to_be_bytes());
            item.entry.finder_info[10..12].copy_from_slice(&(index as i16 * 52).to_be_bytes());
            item.entry.finder_info[12..14].copy_from_slice(&0i16.to_be_bytes());
            item.caption = Caption::measure(if index == 0
                                            {
                                                "a very long caption with many lines and enough \
                                                 words to extend below both neighboring icons and \
                                                 their short captions"
                                            }
                                            else
                                            {
                                                "Short"
                                            },
                                            width).unwrap();
        }
        folder.layout(300).unwrap();
        let mut found = Vec::new();
        let gap = Rect::new(25, 126, 26, 127);
        folder.intersecting(gap, |i, _| found.push(i));
        assert_eq!(found, [0], "tall interval was lost behind a shorter neighbor");
        for y in (0..220).step_by(3)
        {
            for x in (0..150).step_by(5)
            {
                let query = Rect::new(x, y, x + 9, y + 7);
                let mut actual = Vec::new();
                folder.intersecting(query, |i, _| actual.push(i));
                actual.sort();
                let expected: Vec<_> =
                    (0..folder.count).filter(|&i| cache::overlaps(folder.items[i].bounds, query))
                                     .collect();
                assert_eq!(actual, expected, "{query:?}");
            }
        }
    });
}

#[test]
fn unreachable_archive_placements_are_relaid_out_without_mutating_source_metadata()
{
    cache::edit(6, |folder| {
        folder.reset(mac_toolbox::WindowId(100), browser::Location::default());
        folder.count = 1;
        let item = &mut folder.items[0];
        item.entry = mac_toolbox::files::CatalogInfo::EMPTY;
        item.entry.finder_info[8..10].copy_from_slice(&0x100u16.to_be_bytes());
        item.entry.finder_info[10..12].copy_from_slice(&(-10238i16).to_be_bytes());
        item.entry.finder_info[12..14].copy_from_slice(&(-30208i16).to_be_bytes());
        let original = item.entry.finder_info;
        item.caption = Caption::measure("Aldus SuperPaint 3.5", width).unwrap();
        item.icon_bounds = Rect::ZERO;
        folder.layout(404).unwrap();
        let placed = folder.items[0].icon_bounds;
        assert!(placed.left >= 0 && placed.top >= icon_view::INFO_HEIGHT && placed.right < 389);
        let mut visible = Vec::new();
        folder.intersecting(Rect::new(0, 20, 389, 173), |i, _| visible.push(i));
        assert_eq!(visible, [0]);
        assert_eq!(folder.items[0].entry.finder_info, original);
        assert!(folder.placements.contains(0));
        assert_eq!(folder.extent.bottom,
                   folder.items[0].caption.bounds(folder.items[0].icon_bounds).bottom);
    });
    let mut entry = mac_toolbox::files::CatalogInfo::EMPTY;
    entry.finder_info[8..10].copy_from_slice(&0x100u16.to_be_bytes());
    for (h, v) in [(i16::MIN, 0), (0, i16::MIN), (i16::MAX, 0), (0, i16::MAX)]
    {
        entry.finder_info[10..12].copy_from_slice(&v.to_be_bytes());
        entry.finder_info[12..14].copy_from_slice(&h.to_be_bytes());
        assert_eq!(icon_view::saved_icon_bounds(&entry), None);
    }
    entry.finder_info[10..12].copy_from_slice(&0i16.to_be_bytes());
    entry.finder_info[12..14].copy_from_slice(&(-20i16).to_be_bytes());
    assert_eq!(icon_view::saved_icon_bounds(&entry), Some(Rect::new(-4, 28, 28, 60)));
}

#[test]
fn scroll_ranges_include_one_pixel_label_overflow_and_reset_at_the_new_view()
{
    use icon_view::ScrollRange;
    let range = ScrollRange::new(-1, 388, 0, 389, 0);
    assert_eq!((range.minimum, range.maximum), (-1, 0));
    assert_eq!(range.origin(0), -1);
    let fitted = ScrollRange::new(-1, 388, 0, 389, -1);
    assert_eq!(fitted.steps(), 0);
    let range = ScrollRange::new(16, 390, 0, 389, 0);
    assert_eq!((range.minimum, range.maximum), (0, 1));
    assert_eq!(ScrollRange::new(16, 390, 0, 389, 1).steps(), 0);
    // Removing the distant item contracts the range; scrolling it into view
    // removes all obsolete empty travel, retaining the final content origin.
    let wide = ScrollRange::new(16, 2000, 0, 389, 1200);
    assert_eq!((wide.minimum, wide.maximum), (16, 1611));
    let near = ScrollRange::new(1500, 1600, 0, 389, 1200);
    assert_eq!((near.minimum, near.maximum), (1200, 1211));
    let reset = ScrollRange::new(1500, 1600, 0, 389, 1211);
    assert_eq!((reset.minimum, reset.maximum, reset.steps()), (1211, 1211, 0));
    let full = ScrollRange { minimum: i16::MIN,
                             maximum: i16::MAX };
    assert_eq!(full.origin(0), i16::MIN);
    assert_eq!(full.origin(full.steps()), i16::MAX);
    assert_eq!(full.value(i16::MAX), full.steps());
}

#[test]
fn zoom_fits_complete_labels_and_chrome_within_the_desktop()
{
    let (bounds, origin) =
        icon_view::fitted_window(Rect::new(-4, 28, 300, 180), Rect::new(400, 350, 600, 470));
    assert_eq!(bounds.right - bounds.left, 335);
    assert_eq!(bounds.bottom - bounds.top, 203);
    assert_eq!((origin.h, origin.v), (-12, 0));
    assert!(bounds.left >= 4 && bounds.top >= 44 && bounds.right <= 636 && bounds.bottom <= 458);
    let (small, _) =
        icon_view::fitted_window(Rect::new(12, 28, 52, 72), Rect::new(8, 46, 412, 234));
    assert_eq!((small.right - small.left, small.bottom - small.top), (180, 100));
    let (large, _) = icon_view::fitted_window(Rect::new(0, 20, 32000, 32000), Rect::ZERO);
    assert_eq!(large, Rect::new(4, 44, 636, 457));
}

#[test]
fn automatic_placement_reserves_existing_icon_and_multiline_label_bounds()
{
    cache::edit(7, |folder| {
        folder.reset(mac_toolbox::WindowId(99), browser::Location::default());
        folder.count = 4;
        for index in 0..4
        {
            let name = if index == 0
            {
                "A long saved caption that wraps over several lines"
            }
            else
            {
                "new file.txt"
            };
            let item = &mut folder.items[index];
            item.entry = mac_toolbox::files::CatalogInfo::EMPTY;
            item.entry.id = mac_toolbox::Cnid(index as u32 + 3);
            let mut bytes = [0; 256];
            bytes[..name.len()].copy_from_slice(name.as_bytes());
            item.entry.name =
                mac_toolbox::InlineText::from_reply(bytes, name.len() as u32).unwrap();
            item.caption = Caption::measure(name, width).unwrap();
            item.icon_bounds = Rect::ZERO;
            item.bounds = Rect::ZERO;
        }
        folder.items[0].entry.finder_info[8..10].copy_from_slice(&0x100u16.to_be_bytes());
        folder.items[0].entry.finder_info[10..14].fill(0);
        folder.layout(180).unwrap();
        assert_eq!(folder.items[0].icon_bounds, Rect::new(16, 28, 48, 60));
        for a in 0..4
        {
            for b in a + 1..4
            {
                assert!(!cache::overlaps(folder.items[a].bounds, folder.items[b].bounds));
            }
        }
        let unchanged: Vec<_> = folder.items[1..4].iter().map(|i| i.icon_bounds).collect();
        // Removing the first icon does not compact or repack the survivors,
        // even if another layout pass is needed after resizing the window.
        folder.items.copy_within(1..4, 0);
        folder.count = 3;
        folder.layout(260).unwrap();
        assert_eq!(folder.items[..3].iter().map(|i| i.icon_bounds).collect::<Vec<_>>(), unchanged);
    });
}
