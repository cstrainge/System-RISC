//! Finder command policy. Menu labels/shortcuts/command IDs live in MENU/MCMD.
use mac_toolbox::*;

use crate::browser::Browser;
pub const ABOUT: u32 = u32::from_be_bytes(*b"abou");
pub const NEW_FOLDER: u32 = u32::from_be_bytes(*b"new ");
pub const OPEN: u32 = u32::from_be_bytes(*b"sope");
pub const GET_INFO: u32 = u32::from_be_bytes(*b"sinf");
pub const CLOSE: u32 = u32::from_be_bytes(*b"clos");
pub const PUT_AWAY: u32 = u32::from_be_bytes(*b"sput");
pub const CLEAR: u32 = u32::from_be_bytes(*b"cler");
pub const SHOW_CLIPBOARD: u32 = u32::from_be_bytes(*b"shcl");
pub const EMPTY: u32 = u32::from_be_bytes(*b"empt");
pub fn install(system: &System<'_>) -> Result<(), Status>
{
    let menus = system.menus();
    menus.clear().map_err(|e| e.0)?;
    for id in [1251, 1252, 1253, 1254, 1256, 1255]
    {
        menus.install_menu_resource(resources::APPLICATION, id).map_err(|e| e.0)?;
    }
    Ok(())
}
/// Idle scrollbar tracking changes neither selection nor file command policy.
/// Consumed clipboard events still pass through this shared policy guard.
pub fn update_after_event(system: &System<'_>, browser: &Browser, event: Event)
                          -> Result<(), Status>
{
    if event.kind == EventKind::Null
    {
        return Ok(());
    }
    update(system, browser)
}

pub fn update(system: &System<'_>, browser: &Browser) -> Result<(), Status>
{
    let menus = system.menus();
    for (id, item, enabled) in [(1252, 1, browser.can_new_folder(system)),
                                (1252, 2, browser.can_open(system)),
                                (1252, 4, system.window(0).is_ok()),
                                (1252, 6, browser.selection(system).is_some()),
                                (1252, 10, browser.can_put_away(system)),
                                (1253, 6, browser.can_trash(system)),
                                (1253, 9, true),
                                (1255, 2, browser.trash_count(system)? > 0)]
    {
        menus.set_item_enabled(id, item, enabled).map_err(|e| e.0)?;
    }
    Ok(())
}
