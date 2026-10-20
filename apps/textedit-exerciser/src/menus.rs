use mac_toolbox::{Error, *};

use crate::editor;

pub const CUT: u32 = u32::from_be_bytes(*b"cut ");
pub const COPY: u32 = u32::from_be_bytes(*b"copy");
pub const PASTE: u32 = u32::from_be_bytes(*b"past");
pub const CLEAR: u32 = u32::from_be_bytes(*b"cler");
pub const SELECT_ALL: u32 = u32::from_be_bytes(*b"sela");


pub fn install(system: &System<'_>) -> Result<(), Error>
{
    system.menus().clear()?;
    for id in [128, 129]
    {
        system.menus().install_menu_resource(resources::APPLICATION, id)?;
    }
    Ok(())
}

pub fn update(system: &System<'_>, editor: &editor::Editor) -> Result<(), Error>
{
    let selected = editor.selected(system);
    let paste = system.scrap().info().is_ok_and(|info| {
                                         (0..info.flavors).any(|index| {
                                                              system.scrap()
                                                                    .flavor_at(index as usize)
                                                                    .is_ok_and(|flavor| {
                                                                        flavor.kind == scrap::TEXT
                                                                    })
                                                          })
                                     });
    for (item, enabled) in [(1, selected), (2, selected), (3, paste), (4, selected), (6, true)]
    {
        system.menus().set_item_enabled(129, item, enabled)?;
    }
    Ok(())
}
