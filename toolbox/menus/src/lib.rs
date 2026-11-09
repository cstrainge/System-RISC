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
/// The composed, read-only System Apple menu.
pub const APPLE_MENU: MenuId = MenuId(SYSTEM_APPLE_MENU_ID);
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

    /// Install a menu in this application's resident menu bar. The menu stays
    /// installed after this temporary client or any `MenuRef` is dropped.
    /// Use `find` for a view; explicit disposal, `clear` or application teardown
    /// ends the resident lifetime. Failed installation leaves existing menus intact.
    pub fn install_menu_resource(self, scope: mac_traps::types::ResourceScope, id: i16)
                                 -> Result<()>
    {
        self.client.menu(&mut MenuCall { operation: INSTALL,
                                         scope: scope.raw(),
                                         menu_id: id,
                                         ..Default::default() })
    }

    /// Classic `GetMBarHeight`.
    pub fn bar_height(self) -> Result<i16>
    {
        let mut call = MenuCall { operation: GET_BAR_HEIGHT,
                                  ..Default::default() };
        self.client.menu(&mut call)?;
        i16::try_from(call.value).map_err(|_| Error::BadArgument)
    }

    pub fn about_system(self) -> Result<()>
    {
        self.client.menu(&mut MenuCall { operation: SHOW_ABOUT_SYSTEM,
                                         ..Default::default() })
    }

    /// Remove this application's installed menus and cancel its menu tracking.
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
                           value: u32::from(event.as_record()?.character()),
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
    pub title: mac_abi::geometry::Rect,
    pub popup: mac_abi::geometry::Rect
}

/// A generation-checked view of an application-owned resident menu.
///
/// Copying or dropping a view does not change the menu's lifetime. Operations
/// after explicit removal, clear or application teardown return `NotFound`.
#[derive(Clone, Copy)]
pub struct MenuRef<'a>
{
    client: Client<'a>,
    handle: MenuHandle
}
impl<'a> Menus<'a>
{
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
        if self.handle.0 == 0
        {
            return Err(Error::NotFound);
        }
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

    /// Replace an item or append at the next index. System validates UTF-8 and
    /// remeasures before publishing; failure leaves the menu intact.
    pub fn set_item(self, item: u16, text: &str, command: u32, enabled: bool, checked: bool)
                    -> Result<()>
    {
        if self.handle.0 == 0
        {
            return Err(Error::NotFound);
        }
        let mut replacement = MenuItemState::EMPTY;
        if text.len() > replacement.text.len()
        {
            return Err(Error::BadArgument);
        }
        replacement.text[..text.len()].copy_from_slice(text.as_bytes());
        replacement.text_length = text.len() as u32;
        replacement.command = command;
        replacement.enabled = u32::from(enabled);
        replacement.checked = u32::from(checked);
        self.client.menu(&mut MenuCall { operation: SET_ITEM,
                                         handle: self.handle.0,
                                         item,
                                         replacement,
                                         ..Default::default() })
    }

    /// Remove items after this count. This never grows uninitialized entries.
    pub fn truncate(self, count: u16) -> Result<()>
    {
        self.call(TRUNCATE, 0, u32::from(count)).map(|_| ())
    }

    /// Explicitly remove this resident menu. Failure leaves it installed and
    /// this view can retry; a successful removal invalidates every old view.
    pub fn dispose(self) -> Result<()>
    {
        self.call(DISPOSE, 0, 0).map(|_| ())
    }
}
