//! Real client marshaling into the resident QuickDraw ABI adapter.
use mac_abi::{quickdraw::*, traps::*, *};
use mac_system::toolbox::quickdraw::{self as resident, Canvas, SurfaceExt};
use toolbox_quickdraw::{QuickDraw, SurfacePort,
                        bitmaps::Bitmap,
                        graphics::{PortState, Shape, TransferMode, Verb}};
unsafe extern "C" fn gate(number: u32, request: *mut (), size: usize) -> i64
{
    unsafe { resident::native::dispatch(number, request, size, None) }
}
#[test]
fn client_drawing_preserves_surface_origin_clip_and_port_state()
{
    let mut pixels = [0xffffff; 24 * 20];
    let mut canvas = Canvas::new(&mut pixels, 24, 20);
    canvas.set_origin(3, 2);
    canvas.set_clip(Rect::new(1, 1, 18, 15));
    let mut surface = Surface::from_canvas(&mut canvas);
    let mut port =
        SurfacePort::new(unsafe { mac_traps::SurfaceConnection::new(&mut surface, gate) });
    let mut state = PortState { foreground: 0x173d85,
                                clip: Rect::new(2, 2, 10, 10),
                                ..Default::default() };
    port.draw_shape(&mut state, Shape::Rect(Rect::new(-10, -10, 20, 20)), Verb::Paint).unwrap();
    for y in 0..20
    {
        for x in 0..24
        {
            assert_eq!(pixels[y * 24 + x],
                       if (5..13).contains(&x) && (4..12).contains(&y)
                       {
                           0x173d85
                       }
                       else
                       {
                           0xffffff
                       });
        }
    }
    let before = pixels;
    port.draw_shape(&mut state, Shape::Rect(Rect::new(2, 2, 10, 10)), Verb::Invert).unwrap();
    port.draw_shape(&mut state, Shape::Rect(Rect::new(2, 2, 10, 10)), Verb::Invert).unwrap();
    assert_eq!(pixels, before);
    assert_eq!(state.foreground, 0x173d85);
    assert_eq!(surface.origin,
               Point { h: 3,
                       v: 2 });
}
#[test]
fn region_boolean_service_has_a_real_hole_and_accepts_output_size_queries()
{
    let services = SystemServices { header:       Header::for_type::<SystemServices>(),
                                    trap:         gate,
                                    capabilities: 0 };
    let client = mac_traps::Client::new(&services).unwrap();
    let qd = QuickDraw::new(client);
    let mut a = [0; 256];
    let mut b = [0; 256];
    let mut output = [0; 512];
    let outer = qd.regions().rectangle(Rect::new(0, 0, 20, 20), &mut a).unwrap();
    let inner = qd.regions().rectangle(Rect::new(5, 5, 15, 15), &mut b).unwrap();
    let ring = qd.regions().difference(outer, inner, &mut output).unwrap();
    assert!(ring.contains(Point { h: 2,
                                  v: 2 })
                .unwrap());
    assert!(!ring.contains(Point { h: 10,
                                   v: 10 })
                 .unwrap());
    assert!(!ring.intersects(Rect::new(7, 7, 12, 12)).unwrap());
    assert!(ring.intersects(Rect::new(0, 0, 3, 3)).unwrap());
    let mut call = RegionCall { operation: REGION_RECT,
                                bounds: Rect::new(0, 0, 5, 5),
                                ..Default::default() };
    assert_eq!(unsafe {
                   gate(REGION,
                        (&mut call as *mut RegionCall).cast(),
                        core::mem::size_of::<RegionCall>())
               },
               0);
    assert_eq!(call.required, 10);
}
#[test]
fn bitmap_service_copies_inputs_and_scales_mono_without_touching_the_clip()
{
    let mut pixels = [0xabcdef; 16 * 8];
    let mut canvas = Canvas::new(&mut pixels, 16, 8);
    canvas.set_clip(Rect::new(2, 1, 14, 7));
    let mut surface = Surface::from_canvas(&mut canvas);
    let mut port =
        SurfacePort::new(unsafe { mac_traps::SurfaceConnection::new(&mut surface, gate) });
    let data = [0x80];
    let source = Bitmap::Mono { data:      &data,
                                row_bytes: 1,
                                bounds:    Rect::new(0, 0, 2, 1) };
    port.copy_bits(&source, source.bounds(), Rect::new(0, 0, 16, 8), TransferMode::Copy, &[])
        .unwrap();
    for y in 0..8
    {
        for x in 0..16
        {
            assert_eq!(pixels[y * 16 + x],
                       if (2..14).contains(&x) && (1..7).contains(&y)
                       {
                           if x < 8 { 0 } else { 0xffffff }
                       }
                       else
                       {
                           0xabcdef
                       });
        }
    }
    assert_eq!(unsafe { gate(QUICKDRAW, core::ptr::null_mut(), 0) }, BAD_ARGUMENT as i64);
    assert_eq!(unsafe { gate(0xffffffff, core::ptr::null_mut(), 0) }, NOT_SUPPORTED as i64);
}
