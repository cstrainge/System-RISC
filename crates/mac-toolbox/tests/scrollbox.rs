//! Public SDK conformance for resident compound scroll controls and CDEF1.
#![cfg(not(target_arch = "riscv64"))]
use mac_abi::scrollbox as wire;
use mac_system::testing::files::with_owner;
use mac_toolbox::{controls::{ScrollBoxId, ScrollBoxRef, ScrollBoxSpec},
                  *};
fn spec(count: u32) -> ScrollBoxSpec
{
    ScrollBoxSpec { viewport: Rect::new(8, 8, 108, 60),
                    scrollbar: Rect::new(108, 8, 124, 60),
                    count,
                    ..Default::default() }
}
fn fill(control: ScrollBoxRef<'_>, range: core::ops::Range<u32>)
{
    for row in range
    {
        control.set_row(row, &format!("Row {row} é"), row == 1).unwrap();
    }
}
fn image(control: ScrollBoxRef<'_>) -> Vec<u32>
{
    let mut pixels = vec![0x123456; 160 * 80];
    control.draw_pixels(&mut pixels, 160, 80).unwrap();
    pixels
}
#[test]
fn resident_viewport_reuses_rows_and_matches_fresh_rendering()
{
    mac_system::testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let owned = system.controls().create_scroll_box(WindowId(0), spec(200000)).unwrap();
        let control = owned.control();
        let first = control.info().unwrap();
        assert_eq!(first.missing, 0..3);
        fill(control, first.missing);
        let original = image(control);
        assert_eq!(original[0], 0x123456);
        assert_eq!(original[7 * 160 + 8], 0x123456);
        assert_eq!(original[60 * 160 + 108], 0x123456);
        for top in [1, 2, 1, 0, 32767, 199997, 3, 0]
        {
            let previous = control.info().unwrap().top;
            let view = control.scroll_to(top).unwrap();
            assert_eq!(view.top, top);
            if (i64::from(top) - i64::from(previous)).abs() == 1
            {
                assert_eq!(view.missing.end - view.missing.start,
                           1,
                           "one exposed row, not a full viewport");
            }
            assert_eq!(control.draw_pixels(&mut vec![0; 160 * 80], 160, 80),
                       Err(Error(SERVICE_BUSY)),
                       "never publish incomplete content");
            fill(control, view.missing);
            let reference = system.controls().create_scroll_box(WindowId(0), spec(200000)).unwrap();
            let fresh = reference.control();
            let view = fresh.scroll_to(top).unwrap();
            fill(fresh, view.missing);
            assert_eq!(image(control), image(fresh), "retained copy differs at {top}");
        }
        assert_eq!(image(control), original);
        let end = control.scroll_to(u32::MAX).unwrap();
        assert_eq!(end.top, 199997);
        fill(control, end.missing);
        assert_eq!(control.info().unwrap().maximum(), 199997);
        let resized = control.configure(ScrollBoxSpec { viewport: Rect::new(8, 8, 108, 44),
                                                        scrollbar: Rect::new(108, 8, 124, 44),
                                                        ..spec(200000) })
                             .unwrap();
        assert_eq!(resized.rows(), 2);
        assert_eq!(resized.missing.end - resized.missing.start, 2);
        fill(control, resized.missing);
        control.draw_pixels(&mut vec![0; 160 * 80], 160, 80).unwrap();
    });
}
#[test]
fn resident_scroll_controls_reject_stale_foreign_and_cross_owner_ids_in_both_traps()
{
    mac_system::testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let owned =
            with_owner(7, || system.controls().create_scroll_box(WindowId(0), spec(10)).unwrap());
        let stale = owned.id();
        with_owner(8, || {
            let control = owned.control();
            assert!(matches!(control.info(), Err(Error(ACCESS_DENIED))));
            assert_eq!(control.dispose(), Err(Error(ACCESS_DENIED)));
        });
        with_owner(7, || {
            owned.control().dispose().unwrap();
            let replacement = system.controls().create_scroll_box(WindowId(0), spec(10)).unwrap();
            assert_eq!(stale.0 & 0xffff, replacement.id().0 & 0xffff);
            assert_ne!(stale, replacement.id());
            assert!(matches!(system.controls().scroll_box(stale).info(), Err(Error(NOT_FOUND))));
            for id in [stale,
                       ScrollBoxId(1),
                       ScrollBoxId(u32::MAX),
                       ScrollBoxId(replacement.id().0 & 0xffff)]
            {
                let mut call = wire::Call { id: id.0,
                                            ..Default::default() };
                assert_eq!(unsafe {
                               (services.trap)(mac_abi::traps::PSTRING_SCROLL_BOX,
                                               core::ptr::from_mut(&mut call).cast(),
                                               core::mem::size_of_val(&call))
                           },
                           NOT_FOUND as i64);
            }
            drop(owned);
            assert!(replacement.control().info().is_ok());
            let id = replacement.id();
            drop(replacement);
            assert!(matches!(system.controls().scroll_box(id).info(), Err(Error(NOT_FOUND))));
        });
    });
}
#[test]
fn row_text_has_native_utf8_and_pascal_traps_and_tracking_uses_cdef()
{
    mac_system::testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let owned = system.controls().create_scroll_box(WindowId(0), spec(200000)).unwrap();
        let control = owned.control();
        fill(control, 0..3);
        control.set_row(0, "Café", false).unwrap();
        let expected = image(control);
        control.set_row(0, "changed", false).unwrap();
        let mut call = wire::Call { operation: wire::SET_ROW,
                                    id: owned.id().0,
                                    row: 0,
                                    text_length: 5,
                                    ..Default::default() };
        call.text[..5].copy_from_slice(&[4, b'C', b'a', b'f', 0x8e]);
        assert_eq!(unsafe {
                       (services.trap)(mac_abi::traps::PSTRING_SCROLL_BOX,
                                       core::ptr::from_mut(&mut call).cast(),
                                       core::mem::size_of_val(&call))
                   },
                   OK as i64);
        assert_eq!(image(control), expected);
        // The thumb starts at24..40. Its four-pixel travel must cover200k rows.
        let mut track = wire::Call { operation: wire::TRACK_BEGIN,
                                     id: owned.id().0,
                                     point: Point { h: 116,
                                                    v: 32 },
                                     ..Default::default() };
        system.client().scroll_box(&mut track).unwrap();
        assert_eq!(track.part, mac_toolbox::controls::SCROLL_THUMB);
        track.operation = wire::TRACK_POLL;
        track.flags = wire::BUTTON_DOWN;
        track.point.v = 400;
        system.client().scroll_box(&mut track).unwrap();
        assert_eq!(track.top, 199997);
        assert_eq!(track.end - track.first, 3);
        fill(control, track.first..track.end);
        track.flags = 0;
        system.client().scroll_box(&mut track).unwrap();
        assert_eq!(track.flags & wire::TRACKING, 0);
        let before = image(control);
        let mut invalid = spec(10);
        invalid.row_height = 0;
        assert!(control.configure(invalid).is_err());
        assert_eq!(image(control), before);
        assert_eq!(control.set_row(0, "outside", false), Err(Error(BAD_ARGUMENT)));
    });
}
