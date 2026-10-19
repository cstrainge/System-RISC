//! Standalone QuickDraw exerciser; all drawing is provided by System services.
#![no_std]
#![no_main]

use core::panic::PanicInfo;

use mac_toolbox::{events::{CMD_KEY, EVERY_EVENT},
                  *};

mod demo;
mod resources
{
    use mac_toolbox as mac_abi;
    include!(concat!(env!("OUT_DIR"), "/resource_ids.rs"));
}

const CLOSE: u32 = u32::from_be_bytes(*b"clos");
const QUIT: u32 = u32::from_be_bytes(*b"quit");

fn run(system: &System<'_>) -> Result<(), Status>
{
    system.menus().clear().map_err(|e| e.0)?;
    for id in [128, 129]
    {
        system.menus()
              .install_menu_resource(mac_toolbox::resources::APPLICATION, id)
              .map_err(|e| e.0)?;
    }
    let window = demo::open(system)?;
    let mut page = 0;
    let mut icon_offset = 0;
    demo::draw(system, window, page, icon_offset)?;
    system.log("QUICKDRAW_TEST_READY\n")?;
    loop
    {
        let event = system.next_event(EVERY_EVENT)?;
        if let Some(selection) = system.menus().selection(&event).map_err(|e| e.0)?
        {
            if [CLOSE, QUIT].contains(&selection.command)
            {
                system.close_window(window)?;
                return Ok(());
            }
            if selection.menu_id == 129 && (1..=10).contains(&selection.item)
            {
                page = (selection.item - 1) as u8;
                demo::draw(system, window, page, icon_offset)?;
            }
            continue;
        }
        match system.window(window)
        {
            Err(NOT_FOUND) => return Ok(()),
            Err(status) => return Err(status),
            Ok(_) =>
            {}
        }
        let mut redraw = event.kind == EventKind::Update && event.message == window;
        if matches!(event.kind, EventKind::KeyDown | EventKind::AutoKey)
           && event.modifiers & CMD_KEY == 0
        {
            let key = event.character();
            if key.is_ascii_digit()
            {
                page = if key == b'0' { 9 } else { key - b'1' };
                redraw = true;
            }
            else if page == 7 && [0x7b, 0x7c].contains(&((event.message >> 8) as u8))
            {
                let count = system.quickdraw().icons().count().map_err(|e| e.0)?;
                if (event.message >> 8) as u8 == 0x7c
                {
                    icon_offset = if icon_offset + 16 < count { icon_offset + 16 } else { 0 };
                }
                else
                {
                    icon_offset = if icon_offset >= 16
                    {
                        icon_offset - 16
                    }
                    else
                    {
                        count.saturating_sub(1) / 16 * 16
                    };
                }
                redraw = true;
            }
        }
        if event.kind == EventKind::MouseDown && event.message == window
        {
            let bounds = system.window(window)?.bounds;
            let y = event.where_.v - bounds.top;
            if (0..23).contains(&y)
            {
                page = ((event.where_.h - bounds.left).max(0) / 56).min(9) as u8;
                redraw = true;
            }
        }
        if redraw
        {
            demo::draw(system, window, page, icon_offset)?;
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
            let _ = system.log("QUICKDRAW_TEST_ERROR\n");
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
