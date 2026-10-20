//! Resource-based native menu state and tracking; all geometry/drawing is MDEF.
#![forbid(unsafe_code)]
use alloc::{boxed::Box, vec::Vec};

use mac_abi::{menus::*, *};

use crate::toolbox::{quickdraw::{Canvas, SurfaceExt},
                     resources::manager::Manager as Resources};
struct Menu
{
    state: Box<MenuDefinitionState>
}
pub struct Manager
{
    menus:       Vec<Menu>,
    entry:       DefinitionEntry,
    gate:        TrapGate,
    tracking:    bool,
    active:      Option<usize>,
    highlighted: i16
}
impl Manager
{
    pub fn new(entry: DefinitionEntry, gate: TrapGate) -> Self
    {
        Self { menus: Vec::new(),
               entry,
               gate,
               tracking: false,
               active: None,
               highlighted: 0 }
    }

    fn call(&self, state: &mut MenuDefinitionState, message: i16, param: i64,
            surface: *mut Surface)
            -> i64
    {
        let mut call = DefinitionCall { header: Header::for_type::<DefinitionCall>(),
                                        kind: MDEF,
                                        id: 0,
                                        variation: 0,
                                        message,
                                        reserved: 0,
                                        param,
                                        object: (state as *mut MenuDefinitionState).cast(),
                                        surface,
                                        gate: self.gate };
        crate::toolbox::resources::definitions::invoke(self.entry, &mut call)
    }

    fn measure(&self, state: &mut MenuDefinitionState) -> Result<(), Status>
    {
        let mut pixel = [0u32];
        let mut canvas = Canvas::new(&mut pixel, 1, 1);
        let mut surface = Surface::from_canvas(&mut canvas);
        let status = self.call(state, MEASURE, 0, &mut surface);
        if status < 0 { Err(status as Status) } else { Ok(()) }
    }

    /// Return whether visible menu state changed. Queries and idempotent writes
    /// do not invalidate the compositor.
    pub fn request(&mut self, call: &mut MenuCall, resources: Resources<'_>)
                   -> Result<bool, Status>
    {
        match call.operation
        {
            INSTALL =>
            {
                if self.menus.len() >= 8
                {
                    return Err(NO_MEMORY);
                }
                let menu = resources.get(call.scope,
                                         ResourceKey { kind: *b"MENU",
                                                       id:   call.menu_id });
                let mut state = match menu
                {
                    Ok((_, r)) => parse_menu(&r.data)?,
                    Err(NOT_FOUND) =>
                    {
                        let r = resources.get(call.scope,
                                              ResourceKey { kind: *b"fmnu",
                                                            id:   call.menu_id })?;
                        parse_fmnu(&r.1.data)?
                    }
                    Err(e) => return Err(e)
                };
                if self.menus.iter().any(|m| m.state.menu_id == state.menu_id)
                {
                    return Err(BAD_ARGUMENT);
                }
                if let Ok((_, commands)) = resources.get(call.scope,
                                                         ResourceKey { kind: *b"MCMD",
                                                                       id:   call.menu_id })
                {
                    if commands.data.len() != state.item_count as usize * 4
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    for (item, bytes) in
                        state.items.iter_mut().zip(commands.data.as_chunks::<4>().0)
                    {
                        item.command = u32::from_be_bytes(*bytes);
                    }
                }
                state.title_bounds.left =
                    self.menus.last().map_or(10, |m| m.state.title_bounds.right);
                self.measure(&mut state)?;
                self.menus.push(Menu { state: Box::new(state) });
                Ok(true)
            }
            CLEAR =>
            {
                let changed = !self.menus.is_empty() || self.tracking;
                self.menus.clear();
                self.tracking = false;
                self.active = None;
                self.highlighted = 0;
                Ok(changed)
            }
            LAYOUT =>
            {
                let menu =
                    self.menus.iter().find(|m| m.state.menu_id == call.menu_id).ok_or(NOT_FOUND)?;
                call.title_bounds = menu.state.title_bounds;
                call.popup_bounds = menu.state.popup_bounds;
                Ok(false)
            }
            SET_ENABLED | SET_CHECKED | COMMAND =>
            {
                let menu = self.menus
                               .iter_mut()
                               .find(|m| m.state.menu_id == call.menu_id)
                               .ok_or(NOT_FOUND)?;
                if call.item == 0
                {
                    if call.operation == COMMAND
                    {
                        call.value = menu.state.item_count;
                        call.command = 0;
                        return Ok(false);
                    }
                    if call.operation != SET_ENABLED
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    let enabled = u32::from(call.value != 0);
                    let changed = menu.state.enabled != enabled;
                    menu.state.enabled = enabled;
                    return Ok(changed);
                }
                let item = menu.state
                               .items
                               .get_mut(call.item as usize - 1)
                               .filter(|_| call.item <= menu.state.item_count as u16)
                               .ok_or(NOT_FOUND)?;
                let changed = match call.operation
                {
                    SET_ENABLED =>
                    {
                        let enabled = u32::from(call.value != 0 && item.separator == 0);
                        let changed = item.enabled != enabled;
                        item.enabled = enabled;
                        changed
                    }
                    SET_CHECKED =>
                    {
                        let checked = u32::from(call.value != 0);
                        let changed = item.checked != checked;
                        item.checked = checked;
                        changed
                    }
                    _ =>
                    {
                        call.command = item.command;
                        call.value = u32::from(item.enabled != 0 && menu.state.enabled != 0)
                                     | (u32::from(item.checked != 0) << 1);
                        false
                    }
                };
                Ok(changed)
            }
            _ => Err(NOT_SUPPORTED)
        }
    }

    pub fn is_tracking(&self) -> bool
    {
        self.tracking
    }

    pub fn begin(&mut self, point: Point) -> bool
    {
        if point.v < 0 || point.v >= 20
        {
            return false;
        }
        self.tracking = true;
        self.hover(point);
        true
    }

    pub fn hover(&mut self, point: Point)
    {
        if !self.tracking
        {
            return;
        }
        if point.v >= 0 && point.v < 20
        {
            self.active =
                self.menus
                    .iter()
                    .position(|m| m.state.enabled != 0 && m.state.title_bounds.contains(point));
            self.highlighted = 0;
            return;
        }
        self.highlighted =
            self.active.map_or(0, |index| {
                           let mut state = *self.menus[index].state;
                           self.call(&mut state,
                                     HIT_ITEM,
                                     (((point.v as u16 as u32) << 16) | point.h as u16 as u32)
                                     as i64,
                                     core::ptr::null_mut()) as i16
                       });
    }

    pub fn release(&mut self, point: Point) -> Option<Selection>
    {
        if !self.tracking
        {
            return None;
        }
        self.hover(point);
        let selected = self.active.and_then(|index| self.selection(index, self.highlighted));
        self.tracking = false;
        self.active = None;
        self.highlighted = 0;
        selected
    }

    fn selection(&self, index: usize, item: i16) -> Option<Selection>
    {
        let state = &self.menus.get(index)?.state;
        if item < 1 || item > state.item_count as i16 || state.enabled == 0
        {
            return None;
        }
        let entry = &state.items[item as usize - 1];
        (entry.enabled != 0 && entry.separator == 0).then_some(Selection { menu_id: state.menu_id,
                                                                           item:    item as u16,
                                                                           command: entry.command })
    }

    pub fn has_key(&self, key: u8) -> bool
    {
        key!=0&&self.menus.iter().any(|m|m.state.items[..m.state.item_count as usize].iter().any(|item|item.key!=0&&item.key.eq_ignore_ascii_case(&key)))
    }

    pub fn key(&self, key: u8) -> Option<Selection>
    {
        self.menus.iter().enumerate().find_map(|(index,m)|m.state.items[..m.state.item_count as usize].iter().position(|item|item.key!=0&&item.key.eq_ignore_ascii_case(&key)&&item.enabled!=0&&item.separator==0).and_then(|item|self.selection(index,item as i16+1)))
    }

    pub fn draw(&self, canvas: &mut Canvas)
    {
        self.draw_bar(canvas);
        self.draw_popup(canvas);
    }

    pub fn selected_menu(&self) -> Option<i16>
    {
        self.active.map(|index| self.menus[index].state.menu_id)
    }

    pub fn draw_bar(&self, canvas: &mut Canvas)
    {
        canvas.reset_port();
        if self.menus.is_empty()
        {
            let mut empty = MenuDefinitionState { first: 1,
                                                  ..Default::default() };
            let mut surface = Surface::from_canvas(canvas);
            self.call(&mut empty, DRAW_BAR, 0, &mut surface);
            return;
        }
        for (index, menu) in self.menus.iter().enumerate()
        {
            let mut state = *menu.state;
            state.first = u32::from(index == 0);
            state.bar_selected = u32::from(self.active == Some(index));
            let mut surface = Surface::from_canvas(canvas);
            self.call(&mut state, DRAW_BAR, 0, &mut surface);
        }
    }

    pub fn draw_popup(&self, canvas: &mut Canvas)
    {
        canvas.reset_port();
        if let Some(index) = self.active
        {
            let mut state = *self.menus[index].state;
            state.highlighted = self.highlighted;
            let mut surface = Surface::from_canvas(canvas);
            self.call(&mut state, DRAW_POPUP, 0, &mut surface);
        }
    }
}
fn copy_text(output: &mut [u8], length: &mut u32, raw: &[u8]) -> Result<(), Status>
{
    let value = mac_formats::text::decode(raw);
    if value.len() > output.len()
    {
        return Err(BAD_ARGUMENT);
    }
    output[..value.len()].copy_from_slice(value.as_bytes());
    *length = value.len() as u32;
    Ok(())
}
fn pstr<'a>(bytes: &'a [u8], at: &mut usize) -> Result<&'a [u8], Status>
{
    let size = *bytes.get(*at).ok_or(BAD_ARGUMENT)? as usize;
    *at += 1;
    let text = bytes.get(*at..*at + size).ok_or(BAD_ARGUMENT)?;
    *at += size;
    Ok(text)
}
fn item_text(item: &mut MenuItemState, raw: &[u8]) -> Result<(), Status>
{
    copy_text(&mut item.text, &mut item.text_length, raw)?;
    item.separator = u8::from(raw == b"-");
    if item.separator != 0
    {
        item.enabled = 0
    }
    Ok(())
}
fn parse_menu(bytes: &[u8]) -> Result<MenuDefinitionState, Status>
{
    if bytes.len() < 13
    {
        return Err(BAD_ARGUMENT);
    }
    if bytes[6..8] != [0, 0]
    {
        return Err(NOT_SUPPORTED);
    }
    let mut state = MenuDefinitionState { menu_id: i16::from_be_bytes(bytes[..2].try_into()
                                                                                .unwrap()),
                                          ..Default::default() };
    state.minimum_width = i16::from_be_bytes(bytes[2..4].try_into().unwrap()).max(0);
    let bits = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
    state.enabled = bits & 1;
    let mut at = 12;
    copy_text(&mut state.title, &mut state.title_length, pstr(bytes, &mut at)?)?;
    loop
    {
        let raw = pstr(bytes, &mut at)?;
        if raw.is_empty()
        {
            break;
        }
        let index = state.item_count as usize;
        if index >= MAX_ITEMS
        {
            return Err(NOT_SUPPORTED);
        }
        let metadata = bytes.get(at..at + 4).ok_or(BAD_ARGUMENT)?;
        at += 4;
        let item = &mut state.items[index];
        item.enabled = (bits >> (index + 1)) & 1;
        item.key = metadata[1];
        item.mark = metadata[2];
        item.checked = u32::from(metadata[2] == 0x12);
        item.style = metadata[3];
        item_text(item, raw)?;
        state.item_count += 1;
    }
    Ok(state)
}
fn parse_fmnu(bytes: &[u8]) -> Result<MenuDefinitionState, Status>
{
    if bytes.len() < 13 || bytes[..2] != [0, 1]
    {
        return Err(BAD_ARGUMENT);
    }
    let count = u16::from_be_bytes(bytes[2..4].try_into().unwrap()) as usize;
    if count == 0 || count - 1 > MAX_ITEMS
    {
        return Err(NOT_SUPPORTED);
    }
    let id = i32::from_be_bytes(bytes[4..8].try_into().unwrap());
    let mut state = MenuDefinitionState { menu_id: i16::try_from(id).map_err(|_| BAD_ARGUMENT)?,
                                          ..Default::default() };
    let mut at = 12;
    copy_text(&mut state.title, &mut state.title_length, pstr(bytes, &mut at)?)?;
    at = (at + 1) & !1;
    for index in 0..count - 1
    {
        let head = bytes.get(at..at + 8).ok_or(BAD_ARGUMENT)?;
        at += 8;
        let item = &mut state.items[index];
        item.command = u32::from_be_bytes(head[..4].try_into().unwrap());
        item.key = head[6];
        item.mark = head[7];
        item.checked = u32::from(item.mark == 0x12);
        item_text(item, pstr(bytes, &mut at)?)?;
        at = (at + 1) & !1;
        state.item_count += 1;
    }
    Ok(state)
}
