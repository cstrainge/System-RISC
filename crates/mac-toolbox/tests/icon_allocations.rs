//! Public icon decoding/painting preserves pixels on typed admission refusal.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/allocations.rs"]
mod allocations;
use allocations::Observe;
use mac_formats::resource::{Resource, encode};
use mac_toolbox::{Error, Point, Rect, ResourceScope, System, TypeCode,
                  quickdraw::{SurfacePort, icons::IconFlags}};
#[test]
fn icon_admission_refusal_is_atomic_retryable_and_standard_paint_has_no_heap_scratch()
{
    let data = encode(&[Resource { kind:       *b"ICON",
                                   id:         7,
                                   name:       None,
                                   attributes: 0,
                                   data:       vec![0xff; 128] }]).unwrap();
    mac_system::testing::with_resource_forks(&encode(&[]).unwrap(), &data, |services| {
        let background = 0x123456;
        let mut pixels = [background; 32 * 32];
        let mut surface = mac_abi::Surface { pixels: pixels.as_mut_ptr(),
                                             width:  32,
                                             height: 32,
                                             origin: Point::default(),
                                             clip:   Rect::new(0, 0, 32, 32) };
        let connection = unsafe { mac_traps::SurfaceConnection::new(&mut surface, services.trap) };
        let mut port = SurfacePort::new(connection);
        for size in [None, Some(32 * 32 * 4), Some(32 * 32)]
        {
            let observed = match size
            {
                None => Observe::begin(true, None),
                Some(size) => Observe::begin(true, Some(size))
            };
            let result = port.plot_icon(ResourceScope::Application,
                                        7,
                                        Rect::new(0, 0, 32, 32),
                                        IconFlags::default());
            let allocations = observed.finish();
            assert_eq!(result, Err(Error::NoMemory));
            assert!(result.unwrap_err().to_string().contains("memory"));
            assert!(allocations > 0);
            assert!(pixels.iter().all(|&pixel| pixel == background),
                    "refused icon admission partially painted the destination");
        }
        let observed = Observe::begin(false, Some(32 * 32 * 4));
        let result = port.plot_icon(ResourceScope::Application,
                                    7,
                                    Rect::new(0, 0, 32, 32),
                                    IconFlags::default());
        let pixel_allocations = observed.finish();
        assert!(result.is_ok(), "refused icon must remain usable for retry");
        assert!(pixels.iter().all(|&pixel| pixel == 0));
        assert!(pixel_allocations <= 1,
                "standard icon transform allocated a second pixel-sized heap scratch buffer");
    }).unwrap();
}

#[test]
fn icon_enumeration_and_bundle_admission_refusal_are_retryable()
{
    fn row(kind: &[u8; 4], id: i16, data: &[u8]) -> Resource
    {
        Resource { kind: *kind,
                   id,
                   name: None,
                   attributes: 0,
                   data: data.to_vec() }
    }
    let bundle = b"TEST\0\0\0\x01ICN#\0\0\0\x07\x12\x34FREF\0\0\0\x02\0\x63";
    let app = encode(&[row(b"BNDL", 128, bundle),
                       row(b"FREF", 99, b"TEXT\0\x07\0"),
                       row(b"ICON", 7, &[0xff; 128]),
                       row(b"SICN", 2, &[0xff; 32]),
                       row(b"ics#", 7, &[0xff; 64])]).unwrap();
    mac_system::testing::with_resource_forks(&encode(&[]).unwrap(), &app, |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let icons = system.quickdraw().icons().application();
        let creator = TypeCode::new(*b"TEST");
        let kind = TypeCode::new(*b"TEXT");
        let observed = Observe::begin(true, None);
        let count = icons.count();
        let first = icons.at(0);
        let file = icons.for_file(creator, kind);
        let allocations = observed.finish();
        assert_eq!(count, Err(Error::NoMemory));
        assert_eq!(first, Err(Error::NoMemory));
        assert_eq!(file, Err(Error::NoMemory));
        assert!(allocations >= 3);
        assert!(Error::NoMemory.to_string().contains("memory"));
        assert_eq!(icons.count(), Ok(2));
        assert_eq!(icons.at(0), Ok(7));
        assert_eq!(icons.at(1), Ok(2));
        assert_eq!(icons.at(2), Err(Error::NotFound));
        assert_eq!(icons.for_file(creator, kind), Ok(0x1234));
        assert_eq!(icons.for_file(TypeCode::new(*b"NONE"), kind), Err(Error::NotFound));
    }).unwrap();
}
