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
