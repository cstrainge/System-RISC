//! Thin native Menu Manager client. Menus and definitions live in System.
#![no_std]
#![forbid(unsafe_code)]
pub use mac_abi::menus::Selection;
use mac_abi::{Event, EventKind, menus::*};
pub use mac_traps::{Client, Error, Result};
#[derive(Clone, Copy)]
pub struct Menus<'a>
{
    client: Client<'a>
}
impl<'a> Menus<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    pub fn install_menu_resource(self, scope: i16, id: i16) -> Result<()>
    {
        self.client.menu(&mut MenuCall { operation: INSTALL,
                                         scope,
                                         menu_id: id,
                                         ..Default::default() })
    }

    pub fn clear(self) -> Result<()>
    {
        self.client.menu(&mut MenuCall { operation: CLEAR,
                                         ..Default::default() })
    }

    pub fn set_item_enabled(self, id: i16, item: u16, enabled: bool) -> Result<()>
    {
        self.client.menu(&mut MenuCall { operation: SET_ENABLED,
                                         menu_id: id,
                                         item,
                                         value: u32::from(enabled),
                                         ..Default::default() })
    }

    pub fn set_item_checked(self, id: i16, item: u16, checked: bool) -> Result<()>
    {
        self.client.menu(&mut MenuCall { operation: SET_CHECKED,
                                         menu_id: id,
                                         item,
                                         value: u32::from(checked),
                                         ..Default::default() })
    }

    pub fn layout(self, id: i16) -> Result<Layout>
    {
        let mut call = MenuCall { operation: LAYOUT,
                                  menu_id: id,
                                  ..Default::default() };
        self.client.menu(&mut call)?;
        Ok(Layout { title: call.title_bounds,
                    popup: call.popup_bounds })
    }

    pub fn item_count(self, id: i16) -> Result<u16>
    {
        let mut call = MenuCall { operation: COMMAND,
                                  menu_id: id,
                                  ..Default::default() };
        self.client.menu(&mut call)?;
        Ok(call.value as u16)
    }

    pub fn item(self, id: i16, item: u16) -> Result<ItemInfo>
    {
        if item == 0
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        let mut call = MenuCall { operation: COMMAND,
                                  menu_id: id,
                                  item,
                                  ..Default::default() };
        self.client.menu(&mut call)?;
        Ok(ItemInfo { enabled: call.value & 1 != 0,
                      checked: call.value & 2 != 0,
                      command: call.command })
    }

    pub fn selection(self, event: &Event) -> Result<Option<Selection>>
    {
        if event.kind != EventKind::MenuCommand
        {
            return Ok(None);
        }
        let mut call = MenuCall { operation: COMMAND,
                                  menu_id: (event.message >> 16) as i16,
                                  item: event.message as u16,
                                  ..Default::default() };
        self.client.menu(&mut call)?;
        Ok(Some(Selection { menu_id: call.menu_id,
                            item:    call.item,
                            command: call.command }))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ItemInfo
{
    pub enabled: bool,
    pub checked: bool,
    pub command: u32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout
{
    pub title: mac_abi::Rect,
    pub popup: mac_abi::Rect
}
