#![cfg(not(target_arch = "riscv64"))]
mod support;
#[allow(dead_code)]
mod conformance;

#[test]
fn native_events_and_errors_are_decoded_through_real_services()
{
    support::with_system(|system| conformance::rust_api::events_and_errors(system).unwrap());
}

#[test]
fn unknown_service_status_is_retained_in_typed_error_diagnostics()
{
    use mac_abi::{Header, SystemServices};
    use mac_toolbox::{Error, System};
    unsafe extern "C" fn unknown(_: u32, _: *mut (), _: usize) -> i64
    {
        -12345
    }
    let services = SystemServices { header:       Header::for_type::<SystemServices>(),
                                    trap:         unknown,
                                    capabilities: 0 };
    let system = unsafe { System::from_raw(&services) }.unwrap();
    let error = system.log("probe").unwrap_err();
    assert_eq!(error, Error::Unknown(-12345));
    assert_eq!(error.status(), -12345);
    assert_eq!(error.to_string(), "unknown System service status -12345");
}

#[test]
fn native_client_mobility_is_an_explicit_threading_contract()
{
    fn mobile<T: Send + Sync>()
    {
    }
    mobile::<mac_toolbox::System<'static>>();
    mobile::<mac_toolbox::windows::Window<'static>>();
    mobile::<mac_toolbox::memory::Handle<'static>>();
    mobile::<mac_toolbox::files::OpenFile<'static>>();
    mobile::<mac_toolbox::controls::ScrollBox<'static>>();
}
