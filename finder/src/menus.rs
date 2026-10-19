//! Finder command policy. Menu labels/shortcuts/command IDs live in MENU/MCMD.
use mac_toolbox::*;

use crate::browser::Browser;
pub const ABOUT: u32 = u32::from_be_bytes(*b"abou");
pub const NEW_FOLDER: u32 = u32::from_be_bytes(*b"new ");
pub const OPEN: u32 = u32::from_be_bytes(*b"sope");
pub const CLOSE: u32 = u32::from_be_bytes(*b"clos");
pub const PUT_AWAY: u32 = u32::from_be_bytes(*b"sput");
pub const CLEAR: u32 = u32::from_be_bytes(*b"cler");
pub const SHOW_CLIPBOARD: u32 = u32::from_be_bytes(*b"shcl");
pub const CUT: u32 = u32::from_be_bytes(*b"cut ");
pub const COPY: u32 = u32::from_be_bytes(*b"copy");
pub const PASTE: u32 = u32::from_be_bytes(*b"past");
pub const SELECT_ALL: u32 = u32::from_be_bytes(*b"sela");
pub const TEXTEDIT: u32 = u32::from_be_bytes(*b"tedm");
pub const EMPTY: u32 = u32::from_be_bytes(*b"empt");
pub fn install(system: &System<'_>) -> Result<(), Status>
{
    let menus = system.menus();
    menus.clear().map_err(|e| e.0)?;
    for id in [1251, 1252, 1253, 1254, 1256, 1255, 1257]
    {
        menus.install_menu_resource(resources::APPLICATION, id).map_err(|e| e.0)?;
    }
    Ok(())
}
pub fn update(system: &System<'_>, browser: &Browser, editor: &crate::textedit_demo::Editor)
              -> Result<(), Status>
{
    update_edit(system, editor)?;
    let menus = system.menus();
    let editing = editor.active(system);
    for (id, item, enabled) in
        [(1252, 1, browser.can_new_folder(system)),
         (1252, 2, browser.can_open(system)),
         (1252, 4, system.window(0).is_ok()),
         (1252, 10, browser.can_put_away(system)),
         (1253, 6, if editing { editor.selected(system) } else { browser.can_trash(system) }),
         (1253, 9, true),
         (1255, 2, browser.trash_count(system)? > 0)]
    {
        menus.set_item_enabled(id, item, enabled).map_err(|e| e.0)?;
    }
    Ok(())
}

/// Text edits do not change the filesystem or folder selection. Keep their
/// menu updates independent of disk-backed browser and Trash policy.
pub fn update_edit(system: &System<'_>, editor: &crate::textedit_demo::Editor)
                   -> Result<(), Status>
{
    let menus = system.menus();
    let editing = editor.active(system);
    let selected = editor.selected(system);
    let paste = editing
                && system.scrap().info().is_ok_and(|info| {
                                            (0..info.flavors).any(|index| {
                                                                 system.scrap()
                                                                       .flavor_at(index as usize)
                                                                       .is_ok_and(|flavor| {
                                                                           flavor.kind
                                                                           == scrap::TEXT
                                                                       })
                                                             })
                                        });
    for (item, enabled) in [(3, selected), (4, selected), (5, paste), (7, editing)]
    {
        menus.set_item_enabled(1253, item, enabled).map_err(|e| e.0)?;
    }
    if editing
    {
        menus.set_item_enabled(1253, 6, selected).map_err(|e| e.0)?;
    }
    Ok(())
}
