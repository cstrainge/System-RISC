//! Public SDK conformance for resident compound scroll controls and CDEF1.
#![cfg(not(target_arch = "riscv64"))]
use mac_abi::scrollbox as wire;
use mac_system::testing::files::with_owner;
use mac_toolbox::{abi::{NOT_FOUND, OK},
                  controls::{ScrollBoxId, ScrollBoxRef, ScrollBoxSpec},
                  *};
// Instrument this test thread's allocations only. The public service must not
// require a contiguous viewport-sized buffer during ordinary row operations.
struct BoundedAllocator;
std::thread_local! { static ALLOCATION_LIMIT: core::cell::Cell<usize> = const { core::cell::Cell::new(usize::MAX) }; }
std::thread_local! { static FAIL_AFTER: core::cell::Cell<Option<usize>> = const { core::cell::Cell::new(None) }; }
fn deny_allocation(size: usize) -> bool
{
    if ALLOCATION_LIMIT.try_with(|limit| size > limit.get()).unwrap_or(false)
    {
        return true;
    }
    FAIL_AFTER.try_with(|remaining| {
                  match remaining.get()
                  {
                      Some(0) =>
                      {
                          remaining.set(None);
                          true
                      }
                      Some(n) =>
                      {
                          remaining.set(Some(n - 1));
                          false
                      }
                      None => false
                  }
              })
              .unwrap_or(false)
}
#[global_allocator]
static ALLOCATOR: BoundedAllocator = BoundedAllocator;
// SAFETY: layout/pointer pairs pass unchanged to System; refusal returns null
// before allocation. Constant thread-local Cells require no allocator recursion.
unsafe impl std::alloc::GlobalAlloc for BoundedAllocator
{
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8
    {
        if deny_allocation(layout.size())
        {
            return core::ptr::null_mut();
        }
        unsafe { std::alloc::System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout)
    {
        unsafe {
            std::alloc::System.dealloc(pointer, layout);
        }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8
    {
        if deny_allocation(size)
        {
            return core::ptr::null_mut();
        }
        unsafe { std::alloc::System.realloc(pointer, layout, size) }
    }
}
fn bounded<T>(limit: usize, run: impl FnOnce() -> T) -> T
{
    struct Restore(usize);
    impl Drop for Restore
    {
        fn drop(&mut self)
        {
            ALLOCATION_LIMIT.set(self.0);
        }
    }
    let _restore = Restore(ALLOCATION_LIMIT.replace(limit));
    run()
}
fn allocation_fault<T>(after: usize, run: impl FnOnce() -> T) -> T
{
    struct Restore(Option<usize>);
    impl Drop for Restore
    {
        fn drop(&mut self)
        {
            FAIL_AFTER.set(self.0);
        }
    }
    let _restore = Restore(FAIL_AFTER.replace(Some(after)));
    run()
}
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
                       Err(Error::ServiceBusy),
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
fn large_viewports_configure_and_scroll_without_viewport_sized_allocations()
{
    mac_system::testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let result = bounded(64 * 1024, || -> ToolboxResult<_> {
            let mut spec = ScrollBoxSpec { viewport: Rect::new(8, 8, 308, 308),
                                           scrollbar: Rect::new(308, 8, 324, 308),
                                           count: 100000,
                                           ..Default::default() };
            let owned = system.controls().create_scroll_box(WindowId(0), spec)?;
            let control = owned.control();
            for count in [100000, 99999, 12, 200000]
            {
                spec.count = count;
                let view = control.configure(spec)?;
                for row in view.missing
                {
                    control.set_row(row, "Bounded row é", row % 2 == 0)?;
                }
                for top in [1, 2, 1, 0, 100000, 0]
                {
                    let view = control.scroll_to(top)?;
                    for row in view.missing
                    {
                        control.set_row(row, "Bounded row é", row % 2 == 0)?;
                    }
                }
                control.invalidate()?;
                for row in control.info()?.missing
                {
                    control.set_row(row, "Bounded row é", true)?;
                }
            }
            Ok(owned)
        });
        let owned =
            result.expect("row caches must fit fragmented free space without a 360 KB allocation");
        let mut pixels = vec![0x123456; 340 * 320];
        owned.control().draw_pixels(&mut pixels, 340, 320).unwrap();
        assert_eq!(pixels[0], 0x123456);
        assert_eq!(pixels[307 * 340 + 20], 0xffffff, "fractional bottom row stays background");
    });
}
#[test]
fn row_allocation_failures_return_errors_and_leave_the_control_recoverable()
{
    mac_system::testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let mut failed = 0;
        let mut completed = 0;
        for after in 0..3
        {
            let owned = system.controls().create_scroll_box(WindowId(0), spec(100)).unwrap();
            let control = owned.control();
            let before = control.info().unwrap();
            match allocation_fault(after, || control.set_row(0, "", false))
            {
                Err(Error::NoMemory) =>
                {
                    failed += 1;
                    assert_eq!(control.info().unwrap().missing, before.missing);
                }
                Ok(_) => completed += 1,
                Err(error) => panic!("unexpected allocation result: {error:?}")
            }
            control.set_row(0, "", false).unwrap();
            fill(control, control.info().unwrap().missing);
            image(control);
        }
        assert_eq!(failed, 2, "both pixels and shared-row ownership fail fallibly");
        assert_eq!(completed, 1);
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
            assert!(matches!(control.info(), Err(Error::AccessDenied)));
            assert_eq!(control.dispose(), Err(Error::AccessDenied));
        });
        with_owner(7, || {
            owned.control().dispose().unwrap();
            let replacement = system.controls().create_scroll_box(WindowId(0), spec(10)).unwrap();
            assert_eq!(stale.0 & 0xffff, replacement.id().0 & 0xffff);
            assert_ne!(stale, replacement.id());
            assert!(matches!(system.controls().scroll_box(stale).info(), Err(Error::NotFound)));
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
            assert!(matches!(system.controls().scroll_box(id).info(), Err(Error::NotFound)));
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
        assert_eq!(control.set_row(0, "outside", false), Err(Error::BadArgument));
    });
}
