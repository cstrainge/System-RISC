use mac_toolbox::{Error, *};

use crate::editor;

pub const CUT: u32 = u32::from_be_bytes(*b"cut ");
pub const COPY: u32 = u32::from_be_bytes(*b"copy");
pub const PASTE: u32 = u32::from_be_bytes(*b"past");
pub const CLEAR: u32 = u32::from_be_bytes(*b"cler");
pub const SELECT_ALL: u32 = u32::from_be_bytes(*b"sela");


pub fn install<'a>(system: &System<'a>) -> Result<[menus::Menu<'a>; 4], Error>
{
    let menus = system.menus();
    menus.clear()?;
    Ok([menus.install_owned(resources::APPLICATION, 128)?,
        menus.install_owned(resources::APPLICATION, 129)?,
        menus.install_owned(resources::APPLICATION, 130)?,
        menus.install_owned(resources::APPLICATION, 131)?])
}
#[derive(Default)]
pub struct Policy
{
    enabled: [Option<bool>; 8],
    font:    Option<quickdraw::fonts::FontSelection>,
    scrap:   Option<scrap::ScrapInfo>,
    paste:   bool
}
impl Policy
{
    pub fn update<'a>(&mut self, system: &System<'a>, editor: Option<&editor::Editor<'a>>,
                      default_font: quickdraw::fonts::FontSelection)
                      -> Result<(), Error>
    {
        let info = editor.map(|editor| editor.info(system)).transpose()?;
        let selected =
            info.is_some_and(|info| info.active && info.selection_start != info.selection_end);
        let dirty =
            info.zip(editor).is_some_and(|(info, editor)| editor.modified(info.content_revision));
        let scrap = system.scrap().info()?;
        if self.scrap != Some(scrap)
        {
            let mut paste = false;
            for at in 0..scrap.flavors
            {
                paste |= system.scrap().flavor_at(at as usize)?.kind == scrap::TEXT;
            }
            self.paste = paste;
            self.scrap = Some(scrap);
        }
        let present = editor.is_some();
        let values = [(128, 4, present),
                      (128,
                       5,
                       present
                       && (dirty || editor.is_some_and(|editor| editor.document.is_none()))),
                      (128, 6, present),
                      (129, 1, selected),
                      (129, 2, selected),
                      (129, 3, present && self.paste),
                      (129, 4, selected),
                      (129, 6, present)];
        for (slot, (menu, item, enabled)) in values.into_iter().enumerate()
        {
            if self.enabled[slot] != Some(enabled)
            {
                system.menus().set_item_enabled(MenuId(menu), item, enabled)?;
                self.enabled[slot] = Some(enabled);
            }
        }
        let font = editor.map_or(default_font, editor::Editor::font);
        if self.font != Some(font)
        {
            for (at, family) in crate::preferences::FAMILIES.into_iter().enumerate()
            {
                let checked = family == font.family;
                if self.font.is_none_or(|old| (old.family == family) != checked)
                {
                    system.menus().set_item_checked(MenuId(130), at as u16 + 1, checked)?;
                }
            }
            for (at, size) in crate::preferences::SIZES.into_iter().enumerate()
            {
                let checked = size == font.size;
                if self.font.is_none_or(|old| (old.size == size) != checked)
                {
                    system.menus().set_item_checked(MenuId(131), at as u16 + 1, checked)?;
                }
            }
            self.font = Some(font);
        }
        Ok(())
    }
}
