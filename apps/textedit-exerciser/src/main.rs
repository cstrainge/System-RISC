//! Standalone native editor exercising the resident TextEdit implementation.
#![no_std]
#![no_main]


use mac_toolbox::*;

mod editor;
mod batch;
mod resources
{
    use mac_toolbox as mac_abi;
    include!(concat!(env!("OUT_DIR"), "/resource_ids.rs"));
}
mod menus;
mod ui;
use ui::{Text, text};

fn run(system: &System<'_>) -> Result<(), Error>
{
    menus::install(system)?;
    let mut editor = editor::Editor::default();
    editor.open(system)?;
    menus::update(system, &editor)?;
    system.log("TEXTEDIT_EXERCISER_READY\n")?;
    loop
    {
        let event = system.wait_event(mac_toolbox::events::EventMask::ALL, 30)?;
        if let Some(selection) = system.menus().selection(&event)?
        {
            if [u32::from_be_bytes(*b"clos"), u32::from_be_bytes(*b"quit")].contains(&selection.command)
            {
                editor.close(system)?;
                return Ok(());
            }
            editor.command(system, selection.command, event.input().when)?;
            menus::update(system, &editor)?;
            continue;
        }
        if batch::is_keyboard(event)
        {
            batch::keyboard(system, &mut editor, event)?;
            if editor.window == 0
            {
                return Ok(());
            }
            continue;
        }
        editor.event(system, event)?;
        if editor.window == 0
        {
            return Ok(());
        }
        if !matches!(event, Event::Idle { .. }) && !matches!(event, Event::KeyUp { .. })
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
    if let Err(status) = unsafe { mac_toolbox::panic::install(services) }
    {
        return status;
    }
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(status) => return status.status()
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
            status.status()
        }
    }
}

mac_toolbox::panic_handler!();
