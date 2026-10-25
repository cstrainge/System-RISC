//! Caller-owned surfaces use checked geometry; resident services own work limits.
#![cfg(not(target_arch = "riscv64"))]
#[path = "support/mod.rs"]
mod support;
use mac_toolbox::{Error, Rect, WindowId,
                  controls::{ScrollBoxSpec, StandardKind, StandardState},
                  quickdraw::images::ErrorKind};
const BACKGROUND: u32 = 0x123456;
const PNG: &[u8] = include_bytes!("data/png/alpha-composite.png");
#[test]
fn full_platform_surfaces_reach_control_scroll_box_and_png_services()
{
    let (width, height) = mac_build::platform::current().memory().unwrap().display_dimensions();
    let (right, bottom) = (width as i16, height as i16);
    let bounds = Rect::new(right - 120, bottom - 64, right - 20, bottom - 36);
    let mut pixels = vec![BACKGROUND; width * height];
    support::with_system(|system| {
        system.controls()
              .draw_standard_pixels(&mut pixels,
                                    width,
                                    height,
                                    StandardKind::Button,
                                    StandardState::new(bounds, "Full surface"))
              .unwrap();
        assert_eq!(pixels[0], BACKGROUND);
        assert_ne!(pixels[(height - 50) * width + width - 70], BACKGROUND);
        pixels.fill(BACKGROUND);
        let owned = system.controls()
                          .create_scroll_box(WindowId(0),
                                             ScrollBoxSpec { viewport: Rect::new(right - 120,
                                                                                 bottom - 100,
                                                                                 right - 36,
                                                                                 bottom - 36),
                                                             scrollbar: Rect::new(right - 36,
                                                                                  bottom - 100,
                                                                                  right - 20,
                                                                                  bottom - 36),
                                                             count: 1,
                                                             ..Default::default() })
                          .unwrap();
        let view = owned.control();
        for row in view.info().unwrap().missing
        {
            view.set_row(row, "Surface row", false).unwrap();
        }
        view.draw_pixels(&mut pixels, width, height).unwrap();
        assert_eq!(pixels[0], BACKGROUND);
        assert_ne!(pixels[(height - 50) * width + width - 70], BACKGROUND);
        pixels.fill(BACKGROUND);
        let destination = Rect::new(right - 8, bottom - 6, right - 2, bottom - 2);
        system.quickdraw()
              .images()
              .draw_to_surface(PNG, &mut pixels, width, height, destination, destination)
              .unwrap();
        assert_eq!(pixels[(height - 6) * width + width - 8], 0xff0000);
        assert_eq!(pixels[0], BACKGROUND);
    });
}
#[test]
fn invalid_geometry_and_resident_surface_limits_leave_pixels_and_controls_usable()
{
    let (width, height) = mac_build::platform::current().memory().unwrap().display_dimensions();
    let mut pixels = vec![BACKGROUND; width * (height + 1)];
    support::with_system(|system| {
        let owned = system.controls()
                          .create_scroll_box(WindowId(0),
                                             ScrollBoxSpec { viewport: Rect::new(8, 8, 108, 60),
                                                             scrollbar: Rect::new(108, 8, 124, 60),
                                                             count: 0,
                                                             ..Default::default() })
                          .unwrap();
        let state = StandardState::new(Rect::new(8, 8, 108, 36), "Valid after refusal");
        let rect = Rect::new(0, 0, 6, 4);
        for (w, h, len) in [(width, height, width * height - 1),
                            (0, height, 0),
                            (usize::MAX, 2, 8),
                            (width, height + 1, width * (height + 1))]
        {
            let buffer = &mut pixels[..len];
            let error =
                system.controls()
                      .draw_standard_pixels(buffer, w, h, StandardKind::Button, state.clone())
                      .unwrap_err();
            assert_eq!(error, Error::BadArgument);
            assert!(!error.to_string().is_empty());
            let error = owned.control().draw_pixels(buffer, w, h).unwrap_err();
            assert_eq!(error, Error::BadArgument);
            assert!(!error.to_string().is_empty());
            let error = system.quickdraw()
                              .images()
                              .draw_to_surface(PNG, buffer, w, h, rect, rect)
                              .unwrap_err();
            assert_eq!(error.service, Error::BadArgument);
            assert_eq!(error.kind, ErrorKind::InvalidDestination);
            assert!(error.to_string().contains("InvalidDestination"));
            assert!(pixels.iter().all(|&p| p == BACKGROUND));
        }
        let buffer = &mut pixels[..width * height];
        system.controls()
              .draw_standard_pixels(buffer, width, height, StandardKind::Button, state)
              .unwrap();
        owned.control().draw_pixels(buffer, width, height).unwrap();
        system.quickdraw()
              .images()
              .draw_to_surface(PNG, buffer, width, height, rect, rect)
              .unwrap();
        assert_eq!(buffer[0], 0xff0000);
    });
}
