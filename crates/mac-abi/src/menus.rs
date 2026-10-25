//! Native resource-based Menu Manager ABI. Classic MenuSelect remains separate.
use crate::{INLINE_STRING_BYTES, Rect};
pub const INSTALL: u32 = 0;
pub const CLEAR: u32 = 1;
pub const SET_ENABLED: u32 = 2;
pub const SET_CHECKED: u32 = 3;
pub const COMMAND: u32 = 4;
pub const LAYOUT: u32 = 5;
/// Resolve a command-key character against current enabled resource items.
pub const KEY: u32 = 6;
/// Dispose a generation-checked menu object, not a semantic resource number.
pub const DISPOSE: u32 = 7;
pub const FIND: u32 = 8;
/// Replace one item, or append at item_count + 1, then remeasure atomically.
pub const SET_ITEM: u32 = 9;
/// Truncate a dynamic menu; growing requires initialized SET_ITEM requests.
pub const TRUNCATE: u32 = 10;
pub const MAX_ITEMS: usize = 31;
pub const MDEF: u32 = u32::from_be_bytes(*b"MDEF");
pub const MEASURE: i16 = 0;
pub const DRAW_BAR: i16 = 1;
pub const DRAW_POPUP: i16 = 2;
pub const HIT_ITEM: i16 = 3;
pub const STANDARD_PROFILE: u16 = 0;
pub const APPLICATION_PROFILE: u16 = 1;
/// An icon family identity. Registered application scopes remain owned by System.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuIcon
{
    pub scope:   crate::resources::Scope,
    pub id:      i16,
    pub flags:   u16,
    pub present: u16
}
impl MenuIcon
{
    pub const fn new(scope: crate::resources::Scope, id: i16) -> Self
    {
        Self { scope,
               id,
               flags: 0,
               present: 1 }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MenuCall
{
    pub operation:    u32,
    pub handle:       u64,
    pub scope:        crate::resources::Scope,
    pub menu_id:      i16,
    pub item:         u16,
    pub reserved:     u16,
    pub value:        u32,
    pub title_bounds: Rect,
    pub popup_bounds: Rect,
    pub command:      u32,
    pub replacement:  MenuItemState
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection
{
    pub menu_id: i16,
    pub item:    u16,
    pub command: u32
}
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuItemState
{
    pub text:        [u8; INLINE_STRING_BYTES],
    pub text_length: u32,
    pub bounds:      Rect,
    pub command:     u32,
    pub enabled:     u32,
    pub checked:     u32,
    pub key:         u8,
    pub mark:        u8,
    pub style:       u8,
    pub separator:   u8,
    pub icon:        MenuIcon
}
impl Default for MenuItemState
{
    fn default() -> Self
    {
        Self::EMPTY
    }
}
impl MenuItemState
{
    pub const EMPTY: Self = Self { text:        [0; INLINE_STRING_BYTES],
                                   text_length: 0,
                                   bounds:      Rect::ZERO,
                                   command:     0,
                                   enabled:     1,
                                   checked:     0,
                                   key:         0,
                                   mark:        0,
                                   style:       0,
                                   separator:   0,
                                   icon:        MenuIcon { scope:   crate::resources::SYSTEM,
                                                           id:      0,
                                                           flags:   0,
                                                           present: 0 } };
}
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MenuDefinitionState
{
    pub minimum_width: i16,
    pub profile:       u16,
    pub menu_id:       i16,
    pub highlighted:   i16,
    pub enabled:       u32,
    pub bar_selected:  u32,
    pub first:         u32,
    pub title:         [u8; INLINE_STRING_BYTES],
    pub title_length:  u32,
    pub title_icon:    MenuIcon,
    pub bar_bounds:    Rect,
    pub title_bounds:  Rect,
    pub popup_bounds:  Rect,
    pub item_count:    u32,
    pub items:         [MenuItemState; MAX_ITEMS]
}
impl Default for MenuDefinitionState
{
    fn default() -> Self
    {
        Self { minimum_width: 0,
               profile:       STANDARD_PROFILE,
               menu_id:       0,
               highlighted:   0,
               enabled:       1,
               bar_selected:  0,
               first:         0,
               title:         [0; INLINE_STRING_BYTES],
               title_length:  0,
               title_icon:    MenuIcon::default(),
               bar_bounds:    Rect::ZERO,
               title_bounds:  Rect::ZERO,
               popup_bounds:  Rect::ZERO,
               item_count:    0,
               items:         [MenuItemState::EMPTY; MAX_ITEMS] }
    }
}
