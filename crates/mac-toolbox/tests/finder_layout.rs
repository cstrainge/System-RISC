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
        folder.layout(300);
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
