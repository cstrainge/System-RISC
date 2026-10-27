//! Public-SDK region/clip admission under allocation refusal.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/allocations.rs"]
mod allocations;
use allocations::Observe;
use mac_system::testing::with_services;
use mac_toolbox::{Error, Point, Rect, System};
#[test]
fn complex_port_snapshots_and_covering_rectangle_paint_need_no_allocation()
{
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let q = system.quickdraw();
        let regions = q.regions();
        let mut a = [0; 10];
        let mut b = [0; 10];
        let mut output = [0; 256];
        let first = Rect::new(2, 3, 12, 11);
        let second = Rect::new(20, 16, 32, 25);
        let a = regions.rectangle(first, &mut a).unwrap();
        let b = regions.rectangle(second, &mut b).unwrap();
        let mask = regions.union(a, b, &mut output).unwrap();
        let world = q.new_gworld(40, 32).unwrap();
        let port = world.graphics();
        port.clip_region(mask.bytes()).unwrap();
        port.colors(0x123456, 0xffffff).unwrap();
        let observed = Observe::begin(true, None);
        let state = port.state();
        let draw = port.paint_rect(Rect::new(0, 0, 40, 32));
        let allocations = observed.finish();
        assert!(state.is_ok());
        assert_eq!(draw, Ok(()));
        assert_eq!(allocations, 0, "retained immutable clip copies must share admitted backing");
        let mut pixels = [0; 40 * 32];
        world.pixels(&mut pixels).unwrap();
        for y in 0..32
        {
            for x in 0..40
            {
                let point = Point { h: x as i16,
                                    v: y as i16 };
                let contains = |r: Rect| {
                    point.h >= r.left && point.h < r.right && point.v >= r.top && point.v < r.bottom
                };
                assert_eq!(pixels[y * 40 + x],
                           if contains(first) || contains(second) { 0x123456 } else { 0xffffff });
            }
        }
    });
}
#[test]
fn refused_clip_and_line_admission_preserve_the_world_port_and_pixels()
{
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let q = system.quickdraw();
        let world = q.new_gworld(32, 32).unwrap();
        let port = world.graphics();
        port.clip_rect(Rect::new(0, 0, 32, 32)).unwrap();
        port.move_to(Point { h: 2,
                             v: 3 })
            .unwrap();
        let before = port.state().unwrap();
        let mut bytes = [0; 1024];
        let mask = q.regions().oval(Rect::new(4, 4, 28, 28), &mut bytes).unwrap();
        let observed = Observe::begin(true, None);
        let clip = port.clip_region(mask.bytes());
        let line = port.line_to(Point { h: 28,
                                        v: 23 });
        let allocations = observed.finish();
        assert_eq!(clip, Err(Error::NoMemory));
        assert_eq!(line, Err(Error::NoMemory));
        assert!(allocations >= 2);
        let after = port.state().unwrap();
        assert_eq!(before.location, after.location);
        assert_eq!(before.clip, after.clip);
        let mut pixels = [0; 32 * 32];
        world.pixels(&mut pixels).unwrap();
        assert_eq!(pixels, [0xffffff; 32 * 32]);
        port.line_to(Point { h: 28,
                             v: 23 })
            .unwrap();
        assert_eq!(port.state().unwrap().location,
                   Point { h: 28,
                           v: 23 });
        port.clip_region(mask.bytes()).unwrap();
        assert!(Error::NoMemory.to_string().contains("memory"));
    });
}
#[test]
fn algebra_and_serialization_refusal_preserve_sources_and_output_then_retry()
{
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let regions = system.quickdraw().regions();
        let mut a = [0; 10];
        let mut b = [0; 10];
        let a = regions.rectangle(Rect::new(0, 0, 10, 10), &mut a).unwrap();
        let b = regions.rectangle(Rect::new(20, 20, 30, 30), &mut b).unwrap();
        let mut output = [0x55; 256];
        let observed = Observe::begin(true, None);
        let union = regions.union(a, b, &mut output).map(|_| ());
        let read = a.contains(Point { h: 2,
                                      v: 3 });
        let intersects = b.intersects(Rect::new(21, 21, 29, 29));
        let allocations = observed.finish();
        assert_eq!(union, Err(Error::NoMemory));
        assert!(allocations > 0);
        assert_eq!(read, Ok(true));
        assert_eq!(intersects, Ok(true));
        assert_eq!(output, [0x55; 256]);
        let union = regions.union(a, b, &mut output).unwrap();
        assert!(union.contains(Point { h: 2,
                                       v: 3 })
                     .unwrap());
        assert!(!union.contains(Point { h: 15,
                                        v: 15 })
                      .unwrap());
        let mut encoded = [0x55; 256];
        let observed = Observe::begin(true, Some(10));
        let refused = regions.offset(union, Point::default(), &mut encoded).map(|_| ());
        let allocations = observed.finish();
        assert_eq!(refused, Err(Error::NoMemory));
        assert!(allocations > 0, "refuse complex encoder admission after successful input parsing");
        assert_eq!(encoded, [0x55; 256]);
        assert!(regions.offset(union, Point::default(), &mut encoded).is_ok());
        let mut rectangle = [0x55; 10];
        let observed = Observe::begin(true, None);
        let result = regions.rectangle(Rect::new(1, 2, 3, 4), &mut rectangle).map(|_| ());
        let allocations = observed.finish();
        assert_eq!(result, Ok(()));
        assert_eq!(allocations, 0, "compact trap output must use its caller buffer directly");
        assert_eq!(rectangle, [0, 10, 0, 2, 0, 1, 0, 4, 0, 3]);
        assert!(regions.rectangle(Rect::new(1, 2, 3, 4), &mut rectangle).is_ok());
        let observed = Observe::begin(true, None);
        let malformed = regions.parse(&[0, 10, 0]).err();
        let allocations = observed.finish();
        assert_eq!(malformed, Some(Error::BadArgument));
        assert_eq!(allocations, 0);
        assert!(Error::BadArgument.to_string().contains("argument"));
    });
}
