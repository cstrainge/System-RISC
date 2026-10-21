//! Thin native Menu Manager client. Menus and definitions live in System.
#![no_std]
#![forbid(unsafe_code)]
pub use mac_traps::types::{MenuHandle, MenuId, ResourceScope};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection
{
    pub menu_id: MenuId,
    pub item:    u16,
    pub command: u32
}
use mac_abi::menus::*;
pub use mac_traps::{Client, Error, Result};
use toolbox_events::{Event, Modifiers};
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

    pub fn install_menu_resource(self, scope: mac_traps::types::ResourceScope, id: i16)
                                 -> Result<()>
    {
        self.client.menu(&mut MenuCall { operation: INSTALL,
                                         scope: scope.raw(),
                                         menu_id: id,
                                         ..Default::default() })
    }

    pub fn clear(self) -> Result<()>
    {
        self.client.menu(&mut MenuCall { operation: CLEAR,
                                         ..Default::default() })
    }

    pub fn set_item_enabled(self, id: MenuId, item: u16, enabled: bool) -> Result<()>
    {
        self.client.menu(&mut MenuCall { operation: SET_ENABLED,
                                         menu_id: id.0,
                                         item,
                                         value: u32::from(enabled),
                                         ..Default::default() })
    }

    pub fn set_item_checked(self, id: MenuId, item: u16, checked: bool) -> Result<()>
    {
        self.client.menu(&mut MenuCall { operation: SET_CHECKED,
                                         menu_id: id.0,
                                         item,
                                         value: u32::from(checked),
                                         ..Default::default() })
    }

    pub fn layout(self, id: MenuId) -> Result<Layout>
    {
        let mut call = MenuCall { operation: LAYOUT,
                                  menu_id: id.0,
                                  ..Default::default() };
        self.client.menu(&mut call)?;
        Ok(Layout { title: call.title_bounds,
                    popup: call.popup_bounds })
    }

    pub fn item_count(self, id: MenuId) -> Result<u16>
    {
        let mut call = MenuCall { operation: COMMAND,
                                  menu_id: id.0,
                                  ..Default::default() };
        self.client.menu(&mut call)?;
        Ok(call.value as u16)
    }

    pub fn item(self, id: MenuId, item: u16) -> Result<ItemInfo>
    {
        if item == 0
        {
            return Err(Error::BadArgument);
        }
        let mut call = MenuCall { operation: COMMAND,
                                  menu_id: id.0,
                                  item,
                                  ..Default::default() };
        self.client.menu(&mut call)?;
        Ok(ItemInfo { enabled: call.value & 1 != 0,
                      checked: call.value & 2 != 0,
                      command: call.command })
    }

    pub fn selection(self, event: &Event) -> Result<Option<Selection>>
    {
        let mut call = match *event
        {
            Event::MenuCommand { menu,
                                 item,
                                 .. } =>
            {
                MenuCall { operation: COMMAND,
                           menu_id: menu.0,
                           item,
                           ..Default::default() }
            }
            Event::KeyDown { input,
                             .. } if input.modifiers.contains(Modifiers::COMMAND) =>
            {
                MenuCall { operation: KEY,
                           value: u32::from(event.into_record().character()),
                           ..Default::default() }
            }
            _ => return Ok(None)
        };
        // Input can arrive before the application commits a selection's menu
        // policy. Resolve an unconsumed key now, after preceding queued events.
        match self.client.menu(&mut call)
        {
            Err(Error::NotFound) if call.operation == KEY => return Ok(None),
            result => result?
        }
        Ok(Some(Selection { menu_id: MenuId(call.menu_id),
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

#[derive(Clone, Copy)]
pub struct MenuRef<'a>
{
    client: Client<'a>,
    handle: MenuHandle
}
#[must_use = "dropping a menu owner removes its menu"]
pub struct Menu<'a>
{
    menu: MenuRef<'a>,
    live: bool
}
impl<'a> Menus<'a>
{
    /// Install an owned resource menu. Resource numbers are properties; the
    /// returned generation-checked object identity controls its lifetime.
    pub fn install_owned(self, scope: ResourceScope, resource: i16) -> Result<Menu<'a>>
    {
        let mut call = MenuCall { operation: INSTALL,
                                  scope: scope.raw(),
                                  menu_id: resource,
                                  ..Default::default() };
        self.client.menu(&mut call)?;
        Ok(Menu { menu: self.menu(MenuHandle(call.handle)),
                  live: true })
    }

    pub fn menu(self, handle: MenuHandle) -> MenuRef<'a>
    {
        MenuRef { client: self.client,
                  handle }
    }

    pub fn find(self, id: MenuId) -> Result<MenuRef<'a>>
    {
        let mut call = MenuCall { operation: FIND,
                                  menu_id: id.0,
                                  ..Default::default() };
        self.client.menu(&mut call)?;
        Ok(self.menu(MenuHandle(call.handle)))
    }
}
impl MenuRef<'_>
{
    pub fn handle(self) -> MenuHandle
    {
        self.handle
    }

    fn call(self, operation: u32, item: u16, value: u32) -> Result<MenuCall>
    {
        let mut call = MenuCall { operation,
                                  handle: self.handle.0,
                                  item,
                                  value,
                                  ..Default::default() };
        self.client.menu(&mut call)?;
        Ok(call)
    }

    pub fn id(self) -> Result<MenuId>
    {
        Ok(MenuId(self.call(LAYOUT, 0, 0)?.menu_id))
    }

    pub fn layout(self) -> Result<Layout>
    {
        let call = self.call(LAYOUT, 0, 0)?;
        Ok(Layout { title: call.title_bounds,
                    popup: call.popup_bounds })
    }

    pub fn item_count(self) -> Result<u16>
    {
        Ok(self.call(COMMAND, 0, 0)?.value as u16)
    }

    pub fn item(self, item: u16) -> Result<ItemInfo>
    {
        if item == 0
        {
            return Err(Error::BadArgument);
        }
        let call = self.call(COMMAND, item, 0)?;
        Ok(ItemInfo { enabled: call.value & 1 != 0,
                      checked: call.value & 2 != 0,
                      command: call.command })
    }

    pub fn set_item_enabled(self, item: u16, enabled: bool) -> Result<()>
    {
        self.call(SET_ENABLED, item, u32::from(enabled)).map(|_| ())
    }

    pub fn set_item_checked(self, item: u16, checked: bool) -> Result<()>
    {
        self.call(SET_CHECKED, item, u32::from(checked)).map(|_| ())
    }

    pub fn dispose(self) -> Result<()>
    {
        self.call(DISPOSE, 0, 0).map(|_| ())
    }
}
impl<'a> Menu<'a>
{
    pub fn menu(&self) -> MenuRef<'a>
    {
        self.menu
    }

    pub fn handle(&self) -> MenuHandle
    {
        self.menu.handle
    }

    pub fn into_handle(self) -> MenuHandle
    {
        let handle = self.handle();
        core::mem::forget(self);
        handle
    }

    pub fn dispose(&mut self) -> Result<()>
    {
        if self.live
        {
            match self.menu.dispose()
            {
                Ok(()) | Err(Error::NotFound) => self.live = false,
                Err(error) => return Err(error)
            }
        }
        Ok(())
    }
}
impl<'a> core::ops::Deref for Menu<'a>
{
    type Target = MenuRef<'a>;

    fn deref(&self) -> &Self::Target
    {
        &self.menu
    }
}
impl Drop for Menu<'_>
{
    fn drop(&mut self)
    {
        let _ = self.dispose();
    }
}
