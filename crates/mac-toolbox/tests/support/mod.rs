//! One adapter for the aggregate userspace suite. Test bodies receive only the
//! public `mac_toolbox::System` facade, never resident managers or service calls.
pub fn with_system<R>(run: impl FnOnce(&mac_toolbox::System<'_>) -> R) -> R
{
    mac_system::testing::with_services(|services| {
        // The host harness owns this live table and callbacks for the closure.
        let system = unsafe { mac_toolbox::System::from_raw(services) }.expect("harness ABI");
        run(&system)
    })
}
