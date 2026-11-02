//! Finder command policy. Menu labels/shortcuts/command IDs live in MENU/MCMD.
use mac_toolbox::{Error, *};

use crate::browser::Browser;
pub const NEW_FOLDER: u32 = crate::resources::commands::NEW_FOLDER;
pub const OPEN: u32 = crate::resources::commands::OPEN;
pub const SELECT_ALL: u32 = crate::resources::commands::SELECT_ALL;
pub const GET_INFO: u32 = crate::resources::commands::GET_INFO;
pub const CLOSE: u32 = crate::resources::commands::CLOSE;
pub const PUT_AWAY: u32 = crate::resources::commands::PUT_AWAY;
pub const CLEAR: u32 = crate::resources::commands::CLEAR;
pub const SHOW_CLIPBOARD: u32 = crate::resources::commands::SHOW_CLIPBOARD;
pub const EMPTY: u32 = crate::resources::commands::EMPTY;
pub const RESTART: u32 = crate::resources::commands::RESTART;
pub const SHUTDOWN: u32 = crate::resources::commands::SHUTDOWN;
fn apple_menu_icon_flag(system: &System<'_>) -> Result<bool, Error>
{
    let files = system.files();
    let mut startup = None;
    for index in 0..files.volume_count()?
    {
        let volume = files.volume_at(index)?;
        if volume.startup
        {
            startup = Some(volume.id);
            break;
        }
        if startup.is_none()
        {
            startup = Some(volume.id);
        }
    }
    let Some(startup) = startup
    else
    {
        return Ok(false);
    };
    match files.lookup(startup, files::ROOT_ID, "System Folder/Preferences/limit")
    {
        Ok(_) => Ok(true),
        Err(Error::NotFound) => Ok(false),
        Err(error) => Err(error)
    }
}

fn report_icon_flag_error(system: &System<'_>, operation: &str, error: Error) -> Result<(), Error>
{
    use core::fmt::Write;
    let mut line = crate::browser::Text::<256>::default();
    let _ = writeln!(&mut line, "FINDER_APPLE_MENU_ICON_{operation}: {error}");
    system.log(line.as_str())
}

pub fn install<'a>(system: &System<'a>) -> Result<[mac_toolbox::menus::MenuRef<'a>; 5], Error>
{
    let flag = match apple_menu_icon_flag(system)
    {
        Ok(flag) => flag,
        Err(error) =>
        {
            report_icon_flag_error(system, "LOOKUP_FAILED", error)?;
            false
        }
    };
    if let Err(error) = system.quickdraw().icons().set_apple_menu_icon_flag(flag)
    {
        report_icon_flag_error(system, "SET_FAILED", error)?;
    }
    let menus = system.menus();
    menus.clear()?;
    for id in [1252, 1253, 1254, 1256, 1255]
    {
        menus.install_menu_resource(resources::APPLICATION, id)?;
    }
    match system.power().capabilities()
    {
        Ok(capabilities) =>
        {
            menus.set_item_enabled(MenuId(1255), 7, capabilities.restart)?;
            menus.set_item_enabled(MenuId(1255), 8, capabilities.shutdown)?;
            use core::fmt::Write;
            let mut line = crate::browser::Text::<128>::default();
            let _ = writeln!(&mut line,
                             "FINDER_POWER_CAPABILITIES shutdown={} restart={}",
                             capabilities.shutdown, capabilities.restart);
            system.log(line.as_str())?;
        }
        Err(error) =>
        {
            use core::fmt::Write;
            let mut line = crate::browser::Text::<256>::default();
            let _ = writeln!(&mut line,
                             "FINDER_POWER_UNAVAILABLE: {error}; rebuild or install a compatible \
                              power driver");
            system.log(line.as_str())?;
        }
    }
    // The resident application menu bar owns installation and disposal.
    // Retain generation-checked views for startup menu restoration.
    Ok([menus.find(MenuId(1252))?,
        menus.find(MenuId(1253))?,
        menus.find(MenuId(1254))?,
        menus.find(MenuId(1256))?,
        menus.find(MenuId(1255))?])
}
/// Idle scrollbar tracking changes neither selection nor file command policy.
/// Consumed clipboard events still pass through this shared policy guard.
pub fn update_after_event(system: &System<'_>, browser: &Browser, event: &Event)
                          -> Result<(), Error>
{
    if matches!(event, Event::Idle { .. })
    {
        return Ok(());
    }
    update(system, browser)
}

pub fn update(system: &System<'_>, browser: &Browser) -> Result<(), Error>
{
    let menus = system.menus();
    for (id, item, enabled) in [(1252, 1, browser.can_new_folder(system)),
                                (1252, 2, browser.can_open(system)),
                                (1252, 4, browser.closable_front(system).is_some()),
                                (1252, 6, browser.info_selection(system).is_some()),
                                (1252, 10, browser.can_put_away(system)),
                                (1253, 6, browser.can_trash(system)),
                                (1253, 3, false),
                                (1253, 4, false),
                                (1253, 5, false),
                                (1253, 7, browser.can_select_all()),
                                (1253, 9, true),
                                (1255, 2, browser.cached_trash_count() > 0)]
    {
        menus.set_item_enabled(MenuId(id), item, enabled)?;
    }
    Ok(())
}
