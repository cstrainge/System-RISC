use mac_boot_screen::{HAPPY, PROGRESS, Panel, Raster};
#[test]
fn progress_changes_only_its_new_pixels_and_no_change_does_no_work()
{
    let mut pixels = vec![0x123456; 640 * 480];
    let mut panel = Panel::begin(&mut Raster::new(&mut pixels, 640, 480).unwrap(), 7, HAPPY);
    assert_eq!(pixels[0], 0x878787);
    assert_eq!(pixels[1], 0xc0c0c0);
    let before = pixels.clone();
    assert!(panel.advance(&mut Raster::new(&mut pixels, 640, 480).unwrap(), 0).is_none());
    assert_eq!(pixels, before);
    for stage in 1..=7
    {
        let before = pixels.clone();
        let damage =
            panel.advance(&mut Raster::new(&mut pixels, 640, 480).unwrap(), stage).unwrap();
        assert!(damage.left > PROGRESS.left
                && damage.right < PROGRESS.right
                && damage.top > PROGRESS.top
                && damage.bottom < PROGRESS.bottom);
        let mut changed = 0;
        for y in 0..480
        {
            for x in 0..640
            {
                if pixels[y * 640 + x] != before[y * 640 + x]
                {
                    changed += 1;
                    assert!(x >= damage.left as usize
                            && x < damage.right as usize
                            && y >= damage.top as usize
                            && y < damage.bottom as usize);
                }
            }
        }
        assert!(changed > 0);
        let before = pixels.clone();
        assert!(panel.advance(&mut Raster::new(&mut pixels, 640, 480).unwrap(), stage).is_none());
        assert_eq!(pixels, before);
    }
    assert!(panel.advance(&mut Raster::new(&mut pixels, 640, 480).unwrap(), 255).is_none());
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
