#![cfg(not(target_arch = "riscv64"))]
mod support;
#[allow(dead_code)]
mod conformance;

#[test]
fn native_resource_scopes_preserve_full_ids_and_reject_malformed_tags()
{
    use mac_abi::resources::{self as wire, Scope};
    use mac_toolbox::ResourceScope;
    for raw in [wire::SYSTEM, wire::CHAIN, wire::APPLICATION, Scope::image(0xfedc_ba98_7654_3210)]
    {
        assert!(raw.valid());
        assert_eq!(ResourceScope::from_raw(raw).unwrap().raw(), raw);
    }
    for raw in [Scope::image(0),
                Scope { kind:     4,
                        reserved: 0,
                        image:    1 },
                Scope { kind:     2,
                        reserved: 0,
                        image:    1 },
                Scope { kind:     3,
                        reserved: 1,
                        image:    1 }]
    {
        assert!(!raw.valid());
        let error = ResourceScope::from_raw(raw).unwrap_err();
        assert_eq!(error, mac_toolbox::Error::BadArgument);
        assert!(!error.to_string().is_empty());
    }
}

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
    mobile::<mac_toolbox::textedit::Editor<'static>>();
    mobile::<mac_toolbox::menus::Menu<'static>>();
    mobile::<mac_toolbox::dialogs::Alert<'static>>();
}

#[test]
fn native_and_legacy_editors_reject_reused_and_foreign_object_ids()
{
    mac_system::testing::with_services(|services| {
        let system = unsafe { mac_toolbox::System::from_raw(services) }.unwrap();
        conformance::ownership::editors(&system, Some(services.trap)).unwrap();
    });
}

#[test]
fn editor_owners_clean_up_scope_failures_and_borrowed_references()
{
    use mac_toolbox::{Error, Rect, ToolboxResult};
    support::with_system(|system| {
        let bounds = Rect::new(0, 0, 120, 60);
        let mut previous = None;
        for _ in 0..96
        {
            let editor = system.textedit().create_owned(bounds, bounds).unwrap();
            if let Some(stale) = previous
            {
                assert_eq!(system.textedit().info(stale), Err(Error::NotFound));
            }
            editor.set_text("borrowed editor café").unwrap();
            assert_eq!(editor.editor().info().unwrap().text_length, 21);
            previous = Some(editor.id());
        }
        assert_eq!(system.textedit().info(previous.unwrap()), Err(Error::NotFound));
        let mut observed = None;
        let failed: ToolboxResult<()> = (|| {
            let editor = system.textedit().create_owned(bounds, bounds)?;
            observed = Some(editor.id());
            editor.set_text("é")?;
            editor.select(1, 1)?; // fails between UTF-8 bytes and drops the owner
            Ok(())
        })();
        assert_eq!(failed, Err(Error::BadArgument));
        assert_eq!(system.textedit().info(observed.unwrap()), Err(Error::NotFound));
    });
}

#[test]
fn app_local_editors_have_distinct_identities_in_simultaneously_live_contexts()
{
    use std::sync::mpsc::channel;
    let (left_tx, left_rx) = channel();
    let (right_tx, right_rx) = channel();
    let run = |send: std::sync::mpsc::Sender<mac_toolbox::textedit::EditorId>,
               receive: std::sync::mpsc::Receiver<mac_toolbox::textedit::EditorId>| {
        support::with_system(|system| {
            let bounds = mac_toolbox::Rect::new(0, 0, 120, 60);
            let editor = system.textedit().create_owned(bounds, bounds).unwrap();
            editor.set_text("own context").unwrap();
            send.send(editor.id()).unwrap();
            let foreign = receive.recv().unwrap();
            assert_ne!(editor.id(), foreign);
            assert_eq!(system.textedit().set_text(foreign, "foreign"),
                       Err(mac_toolbox::Error::NotFound));
            assert_eq!(system.textedit().info(foreign), Err(mac_toolbox::Error::NotFound));
            let mut bytes = [0; 32];
            assert_eq!(editor.text(&mut bytes).unwrap(), "own context");
        });
    };
    std::thread::scope(|scope| {
        scope.spawn(|| run(left_tx, right_rx));
        scope.spawn(|| run(right_tx, left_rx));
    });
}
