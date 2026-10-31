//! Finder command policy. Menu labels/shortcuts/command IDs live in MENU/MCMD.
use mac_toolbox::{Error, *};

use crate::browser::Browser;
pub const NEW_FOLDER: u32 = u32::from_be_bytes(*b"new ");
pub const OPEN: u32 = u32::from_be_bytes(*b"sope");
pub const SELECT_ALL: u32 = u32::from_be_bytes(*b"sela");
pub const GET_INFO: u32 = u32::from_be_bytes(*b"sinf");
pub const CLOSE: u32 = u32::from_be_bytes(*b"clos");
pub const PUT_AWAY: u32 = u32::from_be_bytes(*b"sput");
pub const CLEAR: u32 = u32::from_be_bytes(*b"cler");
pub const SHOW_CLIPBOARD: u32 = u32::from_be_bytes(*b"shcl");
pub const EMPTY: u32 = u32::from_be_bytes(*b"empt");
pub const RESTART: u32 = u32::from_be_bytes(*b"rest");
pub const SHUTDOWN: u32 = u32::from_be_bytes(*b"shut");
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

pub fn install<'a>(system: &System<'a>) -> Result<[menus::Menu<'a>; 5], Error>
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
    let installed = [menus.install_owned(resources::APPLICATION, 1252)?,
                     menus.install_owned(resources::APPLICATION, 1253)?,
                     menus.install_owned(resources::APPLICATION, 1254)?,
                     menus.install_owned(resources::APPLICATION, 1256)?,
                     menus.install_owned(resources::APPLICATION, 1255)?];
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
    Ok(installed)
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
                                (1252, 4, system.window(WindowId(0)).is_ok()),
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
