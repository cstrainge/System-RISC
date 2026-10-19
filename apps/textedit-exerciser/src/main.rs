//! Standalone native editor exercising the resident TextEdit implementation.
#![no_std]
#![no_main]

use core::panic::PanicInfo;

use mac_toolbox::{events::EVERY_EVENT, *};

mod editor;
mod resources
{
    use mac_toolbox as mac_abi;
    include!(concat!(env!("OUT_DIR"), "/resource_ids.rs"));
}
mod menus;
mod ui;
use ui::{Text, text};

fn run(system: &System<'_>) -> Result<(), Status>
{
    menus::install(system)?;
    let mut editor = editor::Editor::default();
    editor.open(system)?;
    menus::update(system, &editor)?;
    system.log("TEXTEDIT_EXERCISER_READY\n")?;
    loop
    {
        let event = system.next_event(EVERY_EVENT)?;
        if let Some(selection) = system.menus().selection(&event).map_err(|e| e.0)?
        {
            if [u32::from_be_bytes(*b"clos"), u32::from_be_bytes(*b"quit")].contains(&selection.command)
            {
                editor.close(system)?;
                return Ok(());
            }
            editor.command(system, selection.command, event.when)?;
            menus::update(system, &editor)?;
            continue;
        }
        editor.event(system, event)?;
        if editor.window == 0
        {
            return Ok(());
        }
        if event.kind != EventKind::Null && event.kind != EventKind::KeyUp
        {
            menus::update(system, &editor)?;
        }
    }
}

/// # Safety
/// The native loader supplies initialized segments and a live service table.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> Status
{
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(status) => return status
    };
    if system.capabilities() & CAP_GRAPHICS == 0
    {
        return NOT_SUPPORTED;
    }
    match run(&system)
    {
        Ok(()) => OK,
        Err(status) =>
        {
            let _ = system.log("TEXTEDIT_EXERCISER_ERROR\n");
            status
        }
    }
}

#[panic_handler]
fn panic(_: &PanicInfo<'_>) -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}
