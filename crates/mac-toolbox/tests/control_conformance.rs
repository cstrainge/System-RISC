//! Every acceptance call enters aggregate SDK -> Client -> real resident CDEF.
#![cfg(not(target_arch = "riscv64"))]
mod support;
mod conformance;
#[test]
fn original_standard_control_pixels_match_via_public_api()
{
    support::with_system(|system| {
        assert_eq!(conformance::controls::original_control_pixels(system).unwrap(), 54)
    });
}
#[test]
fn original_control_hit_trace_matches_via_public_api()
{
    support::with_system(|system| {
        assert_eq!(conformance::controls::original_control_hits(system).unwrap(), 8424)
    });
}
#[test]
fn safe_control_pixel_borrows_validate_storage()
{
    support::with_system(|system| conformance::controls::control_pixel_bounds(system).unwrap());
}
#[test]
fn aggregate_core_acceptance_remains_live()
{
    support::with_system(|system| conformance::run_all(system).unwrap());
}

#[test]
fn original_scrollbar_pixels_match_via_public_api()
{
    support::with_system(|system| {
        assert_eq!(conformance::controls::original_scrollbar_pixels(system).unwrap(), 32)
    });
}

fn with_geneva_controls(run: impl FnOnce(&mac_toolbox::System<'_>, mac_abi::TrapGate))
{
    use mac_formats::resource::{Resource, encode};
    let resources:Vec<_>=[(*b"FOND",3,include_bytes!("../../../system/assets/fonts/FOND-3.bin").as_slice()),
        (*b"FONT",393,include_bytes!("../../../system/assets/fonts/FONT-393.bin").as_slice())]
        .into_iter().map(|(kind,id,data)|Resource {kind,id,name:None,attributes:0,data:data.to_vec()}).collect();
    mac_system::testing::with_resource_forks(&encode(&resources).unwrap(),
                                             &encode(&[]).unwrap(),
                                             |services| {
                                                 let system =
                                                     unsafe {
                                                         mac_toolbox::System::from_raw(services)
                                                     }.unwrap();
                                                 run(&system, services.trap);
                                             }).unwrap();
}
#[test]
fn application_font_bold_control_captions_match_resident_font_manager_pixels()
{
    with_geneva_controls(|system, gate| {
        use mac_toolbox::{Point, Rect,
                          controls::{FontSelection, StandardKind, StandardState},
                          quickdraw::SurfacePort};
        for family in [1, 3]
        {
            let font = FontSelection { family,
                                       size: 9,
                                       face: 1 };
            let metrics = system.quickdraw().fonts().metrics(font).unwrap();
            assert_eq!((metrics.ascent, metrics.descent, metrics.leading), (10, 2, 0));
            for kind in [StandardKind::WindowFontButton,
                         StandardKind::WindowFontCheckbox,
                         StandardKind::WindowFontRadio]
            {
                for highlighted in [0, 1, 255]
                {
                    for value in [0, 1]
                    {
                        for title in ["Locked", "Wiim é Ω caption too long to fit"]
                        {
                            const WIDTH: usize = 82;
                            const HEIGHT: usize = 18;
                            let bounds = Rect::new(0, 0, WIDTH as i16, HEIGHT as i16);
                            let mut state = StandardState::new(bounds, title);
                            state.font = font;
                            state.highlighted = highlighted;
                            state.value = value;
                            let mut actual = [0x123456; WIDTH * HEIGHT];
                            system.controls()
                                  .draw_standard_pixels(&mut actual, WIDTH, HEIGHT, kind, state)
                                  .unwrap();
                            let mut empty = state;
                            empty.title_length = 0;
                            let mut expected = [0x123456; WIDTH * HEIGHT];
                            system.controls()
                                  .draw_standard_pixels(&mut expected, WIDTH, HEIGHT, kind, empty)
                                  .unwrap();
                            let width = system.quickdraw().fonts().text_width(font, title).unwrap();
                            let button = kind == StandardKind::WindowFontButton;
                            let x = if button { (WIDTH as i16 - width) >> 1 } else { 18 };
                            let y = HEIGHT as i16 - ((HEIGHT as i16 - 12) >> 1) - 2;
                            let color = if highlighted == 255
                            {
                                0xa5a5a5
                            }
                            else if highlighted != 0 && button
                            {
                                0xffffff
                            }
                            else
                            {
                                0
                            };
                            let mut text = [0x123456; WIDTH * HEIGHT];
                            let mut surface = mac_abi::Surface { pixels: text.as_mut_ptr(),
                                                                 width:  WIDTH as u32,
                                                                 height: HEIGHT as u32,
                                                                 origin: Point::default(),
                                                                 clip:   Rect::new(x, 0,
                                                                                   WIDTH as i16,
                                                                                   HEIGHT as i16) };
                            // The public borrowed surface transport receives exclusive,
                            // initialized pixel storage and the live service callback.
                            let connection =
                                unsafe { mac_traps::SurfaceConnection::new(&mut surface, gate) };
                            let mut canvas = SurfacePort::new(connection);
                            assert_eq!(canvas.font_width(font, title).unwrap(), width);
                            assert_eq!(canvas.font_text(font,
                                                        Point { h: x,
                                                                v: y },
                                                        title,
                                                        color)
                                             .unwrap(),
                                       width);
                            for (out, ink) in expected.iter_mut().zip(text)
                            {
                                if ink != 0x123456
                                {
                                    *out = ink;
                                }
                            }
                            assert_eq!(actual, expected,
                                       "family{family} {kind:?} highlight{highlighted} \
                                        value{value} {title}");
                        }
                    }
                }
            }
        }
    });
}
#[test]
fn styled_surface_text_preserves_origin_partial_glyph_clip_and_restores_borrowed_clip()
{
    with_geneva_controls(|_, gate| {
        use mac_toolbox::{Point, Rect, controls::FontSelection, quickdraw::SurfacePort};
        let font = FontSelection { family: 1,
                                   size:   9,
                                   face:   1 };
        let original_clip = Rect::new(4, 3, 65, 30);
        let origin = Point { h: 7,
                             v: 4 };
        let baseline = Point { h: 5,
                               v: 10 };
        let mut full = [0xffffff; 80 * 36];
        let mut actual = full;
        for (pixels, clipped) in [(&mut full, false), (&mut actual, true)]
        {
            let mut surface = mac_abi::Surface { pixels: pixels.as_mut_ptr(),
                                                 width: 80,
                                                 height: 36,
                                                 origin,
                                                 clip: original_clip };
            {
                let connection = unsafe { mac_traps::SurfaceConnection::new(&mut surface, gate) };
                let mut canvas = SurfacePort::new(connection);
                if clipped
                {
                    canvas.font_text_clipped(font, baseline, "Wiim é", 7, 0).unwrap();
                }
                else
                {
                    canvas.font_text(font, baseline, "Wiim é", 0).unwrap();
                }
            }
            assert_eq!(surface.clip, original_clip);
        }
        assert!(actual.contains(&0));
        for y in 0..36
        {
            for x in 0..80
            {
                assert_eq!(actual[y * 80 + x],
                           if (12..19).contains(&x) { full[y * 80 + x] } else { 0xffffff });
            }
        }
        let before = actual;
        let mut surface = mac_abi::Surface { pixels: actual.as_mut_ptr(),
                                             width: 80,
                                             height: 36,
                                             origin,
                                             clip: original_clip };
        {
            let connection = unsafe { mac_traps::SurfaceConnection::new(&mut surface, gate) };
            let mut canvas = SurfacePort::new(connection);
            assert!(canvas.font_text_clipped(FontSelection { family: 20,
                                                             size:   18,
                                                             face:   1 },
                                             baseline,
                                             "Error",
                                             4,
                                             0)
                          .is_err());
        }
        assert_eq!(surface.clip, original_clip);
        assert_eq!(actual, before);
    });
}
