//! Masked bitmap admission exercises real offscreen storage through the SDK.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/allocations.rs"]
mod allocations;
use allocations::Observe;
use mac_system::testing::with_services;
use mac_toolbox::{Error, Point, Rect, System,
                  quickdraw::{bitmaps::Bitmap, graphics::TransferMode}};

#[test]
fn masked_indexed_copy_refusals_preserve_the_world_and_retry_with_exact_clipping()
{
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let q = system.quickdraw();
        let regions = q.regions();
        let mut first = [0; 10];
        let mut second = [0; 10];
        let mut horizontal = [0; 128];
        let mut vertical = [0; 128];
        let a = regions.rectangle(Rect::new(0, 0, 8, 2), &mut first).unwrap();
        let b = regions.rectangle(Rect::new(0, 6, 8, 8), &mut second).unwrap();
        let clip = regions.union(a, b, &mut horizontal).unwrap();
        let a = regions.rectangle(Rect::new(0, 0, 2, 8), &mut first).unwrap();
        let b = regions.rectangle(Rect::new(6, 0, 8, 8), &mut second).unwrap();
        let mask = regions.union(a, b, &mut vertical).unwrap();
        let world = q.new_gworld(8, 8).unwrap();
        let background = [0x123456; 64];
        world.set_pixels(&background).unwrap();
        let port = world.graphics();
        port.clip_region(clip.bytes()).unwrap();
        port.move_to(Point { h: 3,
                             v: 4 })
            .unwrap();
        let state = || {
            let s = port.state().unwrap();
            (s.location,
             s.pen_size,
             s.mode,
             s.visibility,
             s.pattern,
             s.background_pattern,
             s.foreground,
             s.background,
             s.clip,
             s.origin)
        };
        let before = state();
        let bounds = Rect::new(0, 0, 8, 8);
        let source = [0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55];
        let palette = [0xabcdef, 0x654321];
        let bitmap = Bitmap::Indexed { data: &source,
                                       row_bytes: 1,
                                       bounds,
                                       depth: 1,
                                       palette: &palette };
        let copy = || port.copy_bits(&bitmap, bounds, bounds, TransferMode::Copy, mask.bytes());
        let observed = Observe::begin(false, None);
        let result = copy();
        let admissions = observed.finish();
        assert_eq!(result, Ok(()));
        assert!(admissions > 2, "exercise geometry admission as well as pixel/palette ownership");
        let mut expected = background;
        for y in 0..8
        {
            for x in 0..8
            {
                if (x < 2 || x >= 6) && (y < 2 || y >= 6)
                {
                    expected[y * 8 + x] = palette[((source[y] >> (7 - x)) & 1) as usize];
                }
            }
        }
        let mut actual = [0; 64];
        world.pixels(&mut actual).unwrap();
        assert_eq!(actual, expected, "clip and mask must both restrict the indexed transfer");
        for ordinal in 1..=admissions
        {
            world.set_pixels(&background).unwrap();
            let observed = Observe::fail_at(ordinal);
            let result = copy();
            let attempted = observed.finish();
            assert!(attempted >= ordinal, "the requested refusal must be reached");
            assert_eq!(result, Err(Error::NoMemory), "allocation {ordinal}/{admissions}");
            world.pixels(&mut actual).unwrap();
            assert_eq!(actual, background, "allocation {ordinal} published partial pixels");
            assert_eq!(state(), before, "failed bitmap admission changed the port");
            assert_eq!(copy(), Ok(()), "the same world must remain usable after refusal");
            world.pixels(&mut actual).unwrap();
            assert_eq!(actual, expected);
        }
        let truncated = Bitmap::Indexed { data: &source[..1],
                                          row_bytes: 1,
                                          bounds,
                                          depth: 1,
                                          palette: &palette };
        assert_eq!(port.copy_bits(&truncated, bounds, bounds, TransferMode::Copy, mask.bytes()),
                   Err(Error::BadArgument));
        assert_eq!(port.copy_bits(&bitmap,
                                  bounds,
                                  bounds,
                                  TransferMode::Copy,
                                  &mask.bytes()[..mask.bytes().len() - 2]),
                   Err(Error::BadArgument));
        world.pixels(&mut actual).unwrap();
        assert_eq!(actual, expected, "malformed input must preserve the last committed pixels");
        assert_eq!(state(), before);
        assert!(Error::NoMemory.to_string().contains("memory"));
        assert!(Error::BadArgument.to_string().contains("argument"));
    });
}
