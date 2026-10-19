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

mod ui;
mod menus;
mod browser;
mod icon_view;

fn try_action<T>(system: &System<'_>, alerts: &mut ui::Alerts, result: Result<T, Status>)
                 -> Result<Option<T>, Status>
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
            if let Err(error) = alerts.error(system, status)
            {
                let mut message = browser::Text::<80>::default();
                let _ = writeln!(&mut message, "FINDER_ALERT_FAILED status={}", error);
                system.log(message.as_str())?;
            }
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
    for (y, index) in [(20, 4), (45, 5), (70, 6), (100, 7), (120, 8), (140, 9), (160, 10)]
    {
        let mut bytes = [0; 768];
        let text = ui::text(system, index, &mut bytes)?;
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
fn open_about(system: &System<'_>, id: &mut u32) -> Result<(), Status>
{
    if *id == 0 || system.window(*id).is_err()
    {
        let mut bytes = [0; 768];
        let title = ui::text(system, 3, &mut bytes)?;
        *id = system.new_window(Rect::new(250, 150, 550, 345), title, (260, 180), 0)?;
    }
    system.select_window(*id)?;
    about(system, *id)
}
fn finder(system: &System<'_>) -> Result<(), Status>
{
    let mut browser = browser::Browser::default();
    menus::install(system)?;
    browser.initialize(system)?;
    let mut alerts = ui::Alerts::default();
    menus::update(system, &browser)?;
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
        if alerts.window != 0
        {
            let confirm = alerts.event(system, event)?;
            if alerts.window == 0
            {
                browser.redraw_all(system)?;
                menus::update(system, &browser)?;
            }
            if confirm
            {
                let _ = try_action(system, &mut alerts, browser.empty_trash(system))?;
                menus::update(system, &browser)?;
            }
            continue;
        }
        browser.prune(system)?;
        let _ = try_action(system, &mut alerts, browser.track(system, event))?;
        if alerts.window != 0
        {
            continue;
        }
        if let Some(selection) = system.menus().selection(&event).map_err(|e| e.0)?
        {
            let mut log = browser::Text::<120>::default();
            use core::fmt::Write;
            let _ = writeln!(&mut log,
                             "FINDER_MENU menu={} item={} command={:08x}",
                             selection.menu_id, selection.item, selection.command);
            system.log(log.as_str())?;
            let action = match selection.command
            {
                menus::NEW_FOLDER => browser.new_folder(system),
                menus::OPEN => browser.open_selection(system),
                menus::CLOSE =>
                {
                    if let Ok(front) = system.window(0)
                    {
                        system.close_window(front.id)?;
                    }
                    browser.prune(system)
                }
                menus::PUT_AWAY => browser.put_away(system),
                menus::CLEAR => browser.trash_selection(system),
                menus::EMPTY => alerts.confirm_empty(system, &browser),
                menus::ABOUT => open_about(system, &mut about_id),
                _ => Ok(())
            };
            let _ = try_action(system, &mut alerts, action)?;
            menus::update(system, &browser)?;
            continue;
        }
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
                if key == b'q' && event.modifiers & CMD_KEY == 0
                {
                    if demo == 0 || system.window(demo).is_err()
                    {
                        let Some(id) =
                            try_action(system, &mut alerts, quickdraw_demo::open(system))?
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
                    let _ = try_action(system, &mut alerts, open_about(system, &mut about_id))?;
                }
                else if key == b'w' && event.modifiers & CMD_KEY != 0
                {
                    if let Ok(front) = system.window(0)
                    {
                        system.close_window(front.id)?;
                        browser.prune(system)?;
                    }
                }
                else if key == b'n' && event.modifiers & CMD_KEY == 0
                {
                    let _ =
                        try_action(system,&mut alerts,
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
                else if code == 0x33 && event.modifiers & CMD_KEY != 0
                {
                    let _ = try_action(system, &mut alerts, browser.trash_selection(system))?;
                }
                else if code == 0x7e && event.modifiers & CMD_KEY != 0
                {
                    let _ = try_action(system, &mut alerts, browser.parent(system))?;
                }
                else if key == b'\r'
                          || key == b'o' && event.modifiers & CMD_KEY != 0
                          || code == 0x7d && event.modifiers & CMD_KEY != 0
                {
                    let _ = try_action(system, &mut alerts, browser.open_selection(system))?;
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
                    let _ = try_action(system, &mut alerts, browser.click(system, event))?;
                }
            }
            _ =>
            {}
        }
        if event.kind != EventKind::Null
        {
            menus::update(system, &browser)?;
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
