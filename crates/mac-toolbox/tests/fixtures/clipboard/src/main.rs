//! Exercise the actual Finder viewer source through the public SDK.
#![no_std]
#![no_main]
use mac_toolbox::{EventKind, Status, System, SystemServices, events::EVERY_EVENT};
#[path = "../../../../../../finder/src/clipboard.rs"]
mod clipboard;
mod ui
{
    use mac_toolbox::{Status, System};
    pub fn text<'b>(system: &System<'_>, index: u32, output: &'b mut [u8])
                    -> Result<&'b str, Status>
    {
        system.resources().application().indexed_string(2000, index, output).map_err(|e| e.0)
    }
}
fn run(system: &System<'_>) -> Result<(), Status>
{
    system.log("CLIPBOARD_PROBE_START\n")?;
    system.quickdraw().cursors().hide().map_err(|e| e.0)?;
    system.log("CLIPBOARD_PROBE_HIDDEN\n")?;
    system.scrap().clear().map_err(|e| e.0)?;
    let mut viewer = clipboard::Viewer::default();
    system.log("CLIPBOARD_PROBE_CLEARED\n")?;
    viewer.open(system)?;
    system.log("DESKTOP_READY: Finder clipboard public SDK fixture\n")?;
    system.log("CLIPBOARD_PROBE_EMPTY\n")?;
    loop
    {
        let event = system.next_event(EVERY_EVENT)?;
        if event.kind != EventKind::KeyDown && viewer.event(system, event)?
        {
            continue;
        }
        if event.kind == EventKind::KeyDown
        {
            match event.character().to_ascii_lowercase()
            {
                b't' =>
                {
                    system.scrap().clear().map_err(|e| e.0)?;
                    system.scrap().put_text("abc").map_err(|e| e.0)?;
                    viewer.refresh(system)?;
                    system.log("CLIPBOARD_PROBE_TEXT\n")?;
                }
                b'e' =>
                {
                    system.scrap().clear().map_err(|e| e.0)?;
                    viewer.refresh(system)?;
                    system.log("CLIPBOARD_PROBE_EMPTY_AGAIN\n")?;
                }
                _ =>
                {}
            }
        }
    }
}
/// # Safety
/// The loader supplies a live service table for the application's lifetime.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> Status
{
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(s) => s,
        Err(e) => return e
    };
    if let Err(error) = run(&system)
    {
        let _ = system.log("CLIPBOARD_PROBE_FAIL\n");
        return error;
    }
    0
}
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}
