//! Finder command policy. Menu labels/shortcuts/command IDs live in MENU/MCMD.
use mac_toolbox::{Error, *};

use crate::browser::Browser;
pub use crate::resources::commands::{ABOUT, CLEAR, CLOSE, EMPTY, GET_INFO, NEW_FOLDER, OPEN,
                                     PUT_AWAY, RESTART, SELECT_ALL, SHOW_CLIPBOARD, SHUTDOWN};
pub fn install<'a>(system: &System<'a>) -> Result<[menus::Menu<'a>; 6], Error>
{
    let menus = system.menus();
    menus.clear()?;
    let installed = [menus.install_owned(resources::APPLICATION, 1251)?,
                     menus.install_owned(resources::APPLICATION, 1252)?,
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
