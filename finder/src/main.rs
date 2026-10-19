//! Finder is a separate resource-fork application using System Toolbox services.
#![no_std]
#![no_main]
use core::panic::PanicInfo;

use mac_toolbox::{System,
                  events::{CMD_KEY, EVERY_EVENT},
                  *};
mod quickdraw_demo;
mod resources
{
    use mac_toolbox as mac_abi;
    include!(concat!(env!("OUT_DIR"), "/resource_ids.rs"));
}

mod browser;
mod icon_view;

fn try_action<T>(system: &System<'_>, result: Result<T, Status>) -> Result<Option<T>, Status>
{
    use core::fmt::Write;
    match result
    {
        Ok(value) => Ok(Some(value)),
        Err(status) =>
        {
            let mut message = browser::Text::<80>::default();
            let _ = writeln!(&mut message, "FINDER_ACTION_FAILED status={}", status);
            system.log(message.as_str())?;
            Ok(None)
        }
    }
}

fn about(system: &System<'_>, id: u32) -> Result<(), Status>
{
    system.begin_update(id)?;
    let port = system.quickdraw().graphics(WindowId(id));
    port.reset().map_err(|e| e.0)?;
    port.colors(0xffffff, 0xffffff).map_err(|e| e.0)?;
    port.paint_rect(Rect::new(0, 0, 300, 210)).map_err(|e| e.0)?;
    for (y, text) in [(20, "RISCy System"),
                      (45, "Finder running in Rust"),
                      (70, "Mounted HFS volumes and both forks"),
                      (100, "Double-click: open folders"),
                      (120, "Ctrl-O / Return: open selection"),
                      (140, "Ctrl-Up: parent; Ctrl-W: close"),
                      (160, "Q: QuickDraw; A: About")]
    {
        port.text(mac_toolbox::quickdraw::fonts::FontSelection { family: 3,
                                                                 size:   9,
                                                                 face:   0 },
                  Point { h: 16,
                          v: y },
                  text,
                  0)
            .map_err(|e| e.0)?;
    }
    system.end_update(id)
}
fn finder(system: &System<'_>) -> Result<(), Status>
{
    let mut browser = browser::Browser::default();
    browser.initialize(system)?;
    let mut demo = 0;
    let mut about_id = 0;
    let mut demo_page = 0;
    let mut icon_offset = 0u32;
    for _ in 0..32
    {
        let event = system.next_event(EVERY_EVENT)?;
        if event.kind == EventKind::Update && browser.contains(event.message)
        {
            browser.draw(system, event.message)?;
        }
        if event.kind == EventKind::Null
        {
            break;
        }
    }
    system.log("DESKTOP_READY: Finder and native Window Manager\n")?;
    loop
    {
        let event = system.next_event(EVERY_EVENT)?;
        browser.prune(system)?;
        browser.track(system, event)?;
        match event.kind
        {
            EventKind::Update =>
            {
                if event.message == demo && demo != 0
                {
                    quickdraw_demo::draw(system, demo, demo_page, icon_offset)?;
                }
                else if event.message == about_id && about_id != 0
                {
                    about(system, about_id)?;
                }
                else if browser.contains(event.message)
                {
                    browser.draw(system, event.message)?;
                }
            }
            EventKind::Activate =>
            {
                browser.activate(event);
                browser.redraw_all(system)?;
            }
            EventKind::KeyDown | EventKind::AutoKey =>
            {
                let key = event.character().to_ascii_lowercase();
                let code = (event.message >> 8) as u8;
                if key == b'q'
                {
                    if demo == 0 || system.window(demo).is_err()
                    {
                        let Some(id) = try_action(system, quickdraw_demo::open(system))?
                        else
                        {
                            continue;
                        };
                        demo = id;
                    }
                    system.select_window(demo)?;
                    quickdraw_demo::draw(system, demo, demo_page, icon_offset)?;
                }
                else if key == b'a' && event.modifiers & CMD_KEY == 0
                {
                    if about_id == 0 || system.window(about_id).is_err()
                    {
                        let Some(id) = try_action(system,
                                                  system.new_window(Rect::new(250, 150, 550,
                                                                              345),
                                                                    "About RISCy System",
                                                                    (260, 180),
                                                                    0))?
                        else
                        {
                            continue;
                        };
                        about_id = id;
                    }
                    system.select_window(about_id)?;
                    about(system, about_id)?;
                }
                else if key == b'w' && event.modifiers & CMD_KEY != 0
                {
                    if let Ok(front) = system.window(0)
                    {
                        system.close_window(front.id)?;
                        browser.prune(system)?;
                    }
                }
                else if key == b'n'
                {
                    let _ =
                        try_action(system,
                                   browser.open(system,
                                         browser::Location { volume: browser.startup,
                                                             folder: mac_toolbox::files::ROOT_ID }))?;
                }
                else if system.window(0).is_ok_and(|w| w.id == demo) && demo != 0
                {
                    if (b'1'..=b'9').contains(&key) || key == b'0'
                    {
                        demo_page = if key == b'0' { 9 } else { key - b'1' };
                        quickdraw_demo::draw(system, demo, demo_page, icon_offset)?;
                    }
                    else if demo_page == 7 && [0x7b, 0x7c].contains(&code)
                    {
                        let count = system.quickdraw().icons().count().map_err(|e| e.0)?;
                        if code == 0x7c
                        {
                            icon_offset =
                                if icon_offset + 16 < count { icon_offset + 16 } else { 0 };
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
                        quickdraw_demo::draw(system, demo, demo_page, icon_offset)?;
                    }
                }
                else if code == 0x7e && event.modifiers & CMD_KEY != 0
                {
                    let _ = try_action(system, browser.parent(system))?;
                }
                else if key == b'\r'
                          || key == b'o' && event.modifiers & CMD_KEY != 0
                          || code == 0x7d && event.modifiers & CMD_KEY != 0
                {
                    let _ = try_action(system, browser.open_selection(system))?;
                }
                else if code == 0x74
                {
                    browser.scroll(system, -104)?;
                }
                else if code == 0x79
                {
                    browser.scroll(system, 104)?;
                }
            }
            EventKind::MouseDown =>
            {
                if event.message == demo && demo != 0
                {
                    let window = system.window(demo)?;
                    let x = event.where_.h - window.bounds.left;
                    let y = event.where_.v - window.bounds.top;
                    if (0..23).contains(&y)
                    {
                        demo_page = (x.max(0) / 56).min(9) as u8;
                        quickdraw_demo::draw(system, demo, demo_page, icon_offset)?;
                    }
                }
                else if event.message == 0 || browser.contains(event.message)
                {
                    let _ = try_action(system, browser.click(system, event))?;
                }
            }
            _ =>
            {}
        }
    }
}

/// # Safety
/// Loader supplies initialized ELF segments and resident System services.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> Status
{
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(status) => return status
    };
    if let Err(status) = system.log("Finder: native application entry reached\n")
    {
        return status;
    }
    if system.capabilities() & CAP_GRAPHICS == 0
    {
        return OK;
    }
    match finder(&system)
    {
        Ok(()) => OK,
        Err(status) =>
        {
            let _ = system.log("FINDER_ERROR: Toolbox operation failed\n");
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
