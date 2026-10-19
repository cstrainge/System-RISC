//! Test application uses only the aggregate userspace SDK, no resident code.
#![no_std]
#![no_main]
use mac_toolbox::{System,
                  events::{EVERY_EVENT, EventKind},
                  quickdraw::Rect,
                  windows::WindowSpec};

fn run(system: &System<'_>) -> mac_toolbox::ToolboxResult<()>
{
    let window = system.windows().create(WindowSpec::new(Rect::new(100,100,540,400), "TRACKING TEST")
        .minimum(160,100).procedure(8))?;
    system.quickdraw().cursors().hide()?;
    system.log("DESKTOP_READY: public SDK tracking fixture\n").map_err(mac_toolbox::Error)?;
    loop
    {
        let event = system.events().next_event(EVERY_EVENT)?;
        if event.kind == EventKind::Update && event.message == window.0
        {
            let _ = system.windows().begin_update(window)?;
            let info = system.windows().info(window)?;
            let port = system.quickdraw().graphics(window);
            port.colors(0xffffff, 0xffffff)?;
            port.paint_rect(Rect::new(0,
                                      0,
                                      info.bounds.right - info.bounds.left,
                                      info.bounds.bottom - info.bounds.top))?;
            system.windows().end_update(window)?;
            system.log("WINDOW_PROBE_READY\n").map_err(mac_toolbox::Error)?;
        }
    }
}

/// # Safety
/// Loader supplies the live public service table for this application.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const mac_toolbox::SystemServices)
                                -> mac_toolbox::Status
{
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(error) => return error
    };
    match run(&system)
    {
        Ok(()) => 0,
        Err(error) => error.0
    }
}
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}
