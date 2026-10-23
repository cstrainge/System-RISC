#![cfg(not(target_arch = "riscv64"))]
mod support;
#[allow(dead_code)]
mod conformance;
use mac_toolbox::{controls::{StandardKind, StandardState},
                  *};

#[test]
fn managed_controls_track_and_validate_lifetimes_through_sdk()
{
    mac_system::testing::with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        conformance::managed_controls::tracking(&system, WindowId(0)).unwrap();
        conformance::managed_controls::reuse(&system, WindowId(0), services.trap).unwrap();
    });
}

#[test]
fn foreign_owners_cannot_read_track_configure_or_dispose_a_control()
{
    use mac_system::testing::files::with_owner;
    support::with_system(|system| {
        let state = StandardState::new(Rect::new(8, 8, 128, 32), "Private");
        let owned = with_owner(7, || {
            system.controls().create_standard(WindowId(0), StandardKind::Button, state).unwrap()
        });
        with_owner(8, || {
            let control = owned.control();
            assert!(matches!(control.info(), Err(Error::AccessDenied)));
            assert_eq!(control.dispose(), Err(Error::AccessDenied));
            assert!(matches!(control.configure(StandardKind::Button, state),
                             Err(Error::AccessDenied)));
            assert!(matches!(control.begin_tracking(Point { h: 20,
                                                            v: 20 },
                                                    0),
                             Err(Error::AccessDenied)));
        });
        with_owner(7, || drop(owned));
    });
}

#[test]
fn progress_meter_has_a_fixed_frame_and_only_completed_work_fills_it()
{
    use mac_toolbox::abi::controls::ProgressRange;
    support::with_system(|system| {
        let bounds = Rect::new(2, 2, 24, 10); // 20 interior pixels.
        for (kind, fill, track) in [(StandardKind::ProgressBar, 0x666666, 0xdadaff),
                                    (StandardKind::MonochromeProgressBar, 0, 0xffffff)]
        {
            let mut pixels = [0xff00ff; 26 * 12];
            for (completed, in_progress, total, edge) in [(0, 10, 20, 3),
                                                          (10, 5, 20, 13),
                                                          (20, 0, 20, 23),
                                                          (0, 0, 20, 3),
                                                          (u64::MAX / 2, 0, u64::MAX, 12),
                                                          (0, 0, 0, 23)]
            {
                let state = StandardState { bounds,
                                            progress: ProgressRange { completed,
                                                                      in_progress,
                                                                      total,
                                                                      determinate: 1,
                                                                      reserved: 0 },
                                            ..Default::default() };
                system.controls().draw_standard_pixels(&mut pixels, 26, 12, kind, state).unwrap();
                for y in 0..12
                {
                    for x in 0..26
                    {
                        let expected = if !(2..24).contains(&x) || !(2..10).contains(&y)
                        {
                            0xff00ff
                        }
                        else if x == 2 || x == 23 || y == 2 || y == 9
                        {
                            0
                        }
                        else if x < edge
                        {
                            fill
                        }
                        else
                        {
                            track
                        };
                        assert_eq!(pixels[y * 26 + x],
                                   expected,
                                   "progress {completed}/{total} at ({x},{y})");
                    }
                }
            }
            let before = pixels;
            let invalid = StandardState { bounds,
                                          progress: ProgressRange { completed: 2,
                                                                    total: 1,
                                                                    determinate: 1,
                                                                    ..Default::default() },
                                          ..Default::default() };
            let error = system.controls()
                              .draw_standard_pixels(&mut pixels, 26, 12, kind, invalid)
                              .unwrap_err();
            assert_eq!(error, Error::BadArgument);
            assert!(!error.to_string().is_empty(), "invalid range needs a diagnostic");
            assert_eq!(pixels, before, "invalid progress must not modify pixels");
        }
    });
}

/// GDB capture stops the real release System before completing boot stages1..3
/// and dumps its front buffer. Keep acceptance here alongside the SDK renderer
/// test; private capture transports do not decide pixel conformance.
#[test]
#[ignore = "requires SYSTEM7_BOOT_PROGRESS_CAPTURE with private release boot captures"]
fn captured_release_boot_uses_the_same_framed_progress_meter()
{
    let folder = std::path::PathBuf::from(std::env::var_os("SYSTEM7_BOOT_PROGRESS_CAPTURE")
        .expect("set SYSTEM7_BOOT_PROGRESS_CAPTURE to the private boot capture directory"));
    for (name, edge) in [("empty", 205), ("one", 237), ("two", 270)]
    {
        let bytes = std::fs::read(folder.join(format!("progress-{name}.bin"))).unwrap();
        assert_eq!(bytes.len(), 640 * 480 * 4, "capture must be a complete front buffer");
        for y in 289..305
        {
            for x in 204..436
            {
                let at = (y * 640 + x) * 4;
                let actual = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
                let expected = if x == 204 || x == 435 || y == 289 || y == 304
                {
                    0
                }
                else if x < edge
                {
                    0x666666
                }
                else
                {
                    0xdadaff
                };
                assert_eq!(actual, expected, "boot {name}, ({x},{y})");
            }
        }
    }
    let serial = std::fs::read_to_string(folder.join("serial.log")).unwrap();
    assert!(serial.contains("DESKTOP_READY:"), "Finder did not reach the desktop: {serial}");
    assert_eq!(serial.matches("MODULE_READY ").count(), 3, "{serial}");
    assert!(serial.contains("HOST_VOLUME_MOUNTED index=0 "), "{serial}");
    for failure in ["MODULE_REJECTED", "SYSTEM_ALLOCATION_FAILED", "SYSTEM_PANIC:", "SYSTEM_TRAP:"]
    {
        assert!(!serial.contains(failure), "unexpected boot failure: {serial}");
    }
}
