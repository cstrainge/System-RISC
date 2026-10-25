use mac_boot_screen::{HAPPY, PanelLayout, Raster};
#[test]
fn welcome_panel_leaves_progress_to_the_control_manager()
{
    let mut pixels = vec![0x123456; 640 * 480];
    mac_boot_screen::draw_panel(&mut Raster::new(&mut pixels, 640, 480).unwrap(), HAPPY);
    assert_eq!(pixels[0], 0x878787);
    assert_eq!(pixels[1], 0xc0c0c0);
    let progress = PanelLayout::new(640, 480).progress;
    for y in progress.top..progress.bottom
    {
        for x in progress.left..progress.right
        {
            assert_eq!(pixels[y as usize * 640 + x as usize], 0xffffff);
        }
    }
}
#[test]
fn pattern_handles_small_surfaces_and_icon_mask_preserves_its_surroundings()
{
    for (width, height) in [(1, 1), (3, 1), (1, 3), (5, 7)]
    {
        let mut pixels = vec![0; width * height];
        Raster::new(&mut pixels, width, height).unwrap().background();
        for y in 0..height
        {
            for x in 0..width
            {
                assert_eq!(pixels[y * width + x],
                           if (x + y) & 1 == 0 { 0x878787 } else { 0xc0c0c0 });
            }
        }
    }
    let mut pixels = vec![0x123456; 64 * 64];
    Raster::new(&mut pixels, 64, 64).unwrap().icon(HAPPY, 16, 16, 1);
    for y in 0..64
    {
        for x in 0..64
        {
            if !(16..48).contains(&x) || !(16..48).contains(&y)
            {
                assert_eq!(pixels[y * 64 + x], 0x123456);
                continue;
            }
            let at = (y - 16) * 4 + (x - 16) / 8;
            let bit = 0x80 >> ((x - 16) & 7);
            if HAPPY[128 + at] & bit == 0
            {
                assert_eq!(pixels[y * 64 + x], 0x123456);
            }
        }
    }
}

#[test]
fn clipped_background_restores_only_requested_pixels_with_global_pattern_phase()
{
    for rect in [mac_abi::Rect::new(3, 5, 17, 18),
                 mac_abi::Rect::new(-4, -3, 4, 4),
                 mac_abi::Rect::new(19, 19, 24, 25),
                 mac_abi::Rect::new(3, 7, 4, 8),
                 mac_abi::Rect::new(8, 9, 2, 1)]
    {
        let mut pixels = vec![0x123456; 20 * 20];
        Raster::new(&mut pixels, 20, 20).unwrap().background_rect(rect);
        for y in 0..20
        {
            for x in 0..20
            {
                assert_eq!(pixels[y * 20 + x],
                           if rect.contains(mac_abi::Point { h: x as i16,
                                                             v: y as i16 })
                           {
                               if (x + y) & 1 == 0 { 0x878787 } else { 0xc0c0c0 }
                           }
                           else
                           {
                               0x123456
                           });
            }
        }
    }
}

#[test]
fn welcome_panel_translates_without_changing_its_pixels_or_control_geometry()
{
    let mut small = vec![0; 640 * 480];
    let mut large = vec![0; 800 * 600];
    mac_boot_screen::draw_panel(&mut Raster::new(&mut small, 640, 480).unwrap(), HAPPY);
    mac_boot_screen::draw_panel(&mut Raster::new(&mut large, 800, 600).unwrap(), HAPPY);
    let a = PanelLayout::new(640, 480);
    let b = PanelLayout::new(800, 600);
    assert_eq!(b.bounds, mac_abi::Rect::new(250, 210, 550, 386));
    assert_eq!(b.progress, mac_abi::Rect::new(284, 349, 516, 365));
    assert_eq!(b.title_baseline, a.title_baseline + 60);
    for y in a.bounds.top..a.bounds.bottom + 4
    {
        for x in a.bounds.left..a.bounds.right + 4
        {
            assert_eq!(small[y as usize * 640 + x as usize],
                       large[(y as usize + 60) * 800 + x as usize + 80]);
        }
    }
}
