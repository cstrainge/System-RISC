//! Resource-based native menu state and tracking; all geometry/drawing is MDEF.
#![forbid(unsafe_code)]
use alloc::{boxed::Box, vec::Vec};

use mac_abi::{menus::*, *};
use mac_object_table::domain::{CompletionError, DomainId, PoolError, ReleasedResource};

use crate::toolbox::{handles::{KindLease, OwnerRef, Store},
                     quickdraw::{Canvas, SurfaceExt},
                     resources::manager::Manager as Resources};
pub(crate) mod lifetime;
struct Menu
{
    id:    u64,
    state: Box<MenuDefinitionState>
}
#[derive(Clone, Copy)]
struct Layout
{
    title: Rect,
    popup: Rect,
    rows:  [Rect; MAX_ITEMS]
}
impl Layout
{
    fn read(state: &MenuDefinitionState) -> Self
    {
        Self { title: state.title_bounds,
               popup: state.popup_bounds,
               rows:  core::array::from_fn(|i| state.items[i].bounds) }
    }

    fn apply(&self, state: &mut MenuDefinitionState)
    {
        state.title_bounds = self.title;
        state.popup_bounds = self.popup;
        for (item, bounds) in state.items.iter_mut().zip(self.rows)
        {
            item.bounds = bounds;
        }
    }
}

struct Bar
{
    order:       Vec<u64>,
    tracking:    bool,
    active:      Option<u64>,
    highlighted: i16,
    scratch:     Box<MenuDefinitionState>,
    reflow:      Vec<(u64, Layout)>
}
impl Bar
{
    fn new(owner: DomainId) -> Result<Self, PoolError>
    {
        let mut order = Vec::new();
        order.try_reserve(1).map_err(|_| {
                                 PoolError::Storage { owner,
            kind: crate::toolbox::handles::KindCode(*b"MNST"), used: 0,
            requested_slots: 1, limit: Some(1) }
                             })?;
        let scratch = new_state().map_err(|_| {
                                     PoolError::Storage { owner,
                                               kind: crate::toolbox::handles::KindCode(*b"MNST"),
                                               used: 0,
                                               requested_slots: 1,
                                               limit: Some(1) }
                                 })?;
        let mut reflow = Vec::new();
        reflow.try_reserve(1).map_err(|_| PoolError::Storage { owner,
            kind: crate::toolbox::handles::KindCode(*b"MNST"), used: 0, requested_slots: 1, limit: Some(1) })?;
        Ok(Self { order,
                  scratch,
                  reflow,
                  tracking: false,
                  active: None,
                  highlighted: 0 })
    }

    fn cancel(&mut self)
    {
        self.tracking = false;
        self.active = None;
        self.highlighted = 0;
    }
}
pub struct Manager
{
    objects: Store<Menu, Bar>,
    owner:   Option<OwnerRef>,
    entry:   DefinitionEntry,
    gate:    TrapGate
}
fn pool_status(error: PoolError) -> Status
{
    crate::toolbox::handles::registry_status(&mac_object_table::registry::Error::Pool(error))
}
impl Manager
{
    pub(crate) fn new(kinds: (KindLease, KindLease), entry: DefinitionEntry, gate: TrapGate)
                      -> Result<Self, Status>
    {
        Ok(Self { objects: Store::owned_state(kinds.0, kinds.1, Bar::new).map_err(pool_status)?,
                  owner: None,
                  entry,
                  gate })
    }

    fn bind(&mut self, owner: &OwnerRef) -> Result<(), Status>
    {
        if self.owner.as_ref().is_some_and(|current| current.owner() != owner.owner())
        {
            return Err(ACCESS_DENIED);
        }
        if self.owner.is_none()
        {
            self.owner = Some(owner.clone());
        }
        Ok(())
    }

    fn bar(&self) -> Option<&Bar>
    {
        self.objects.state(self.owner.as_ref()?.owner().incarnation())
    }

    fn bar_mut(&mut self) -> Option<&mut Bar>
    {
        self.objects.state_mut(self.owner.as_ref()?.owner().incarnation())
    }

    fn id_at(&self, index: usize) -> Option<u64>
    {
        self.bar()?.order.get(index).copied()
    }

    fn menu_id(&self, resource: i16) -> Option<u64>
    {
        self.bar()?.order.iter().copied().find(|id| {
                                             self.objects
                                                 .get(*id)
                                                 .is_some_and(|m| m.state.menu_id == resource)
                                         })
    }

    fn measure(&self, state: &mut MenuDefinitionState) -> Result<(), Status>
    {
        Self::measure_with(self.entry, self.gate, state)
    }

    fn measure_with(entry: DefinitionEntry, gate: TrapGate, state: &mut MenuDefinitionState)
                    -> Result<(), Status>
    {
        state.bar_bounds = Rect::new(0, 0, crate::display::WIDTH as i16, 20);
        let mut pixel = [0u32];
        let mut canvas = Canvas::new(&mut pixel, 1, 1);
        let mut surface = Surface::from_canvas(&mut canvas);
        let status = invoke(entry, gate, state, MEASURE, 0, &mut surface);
        if status < 0 { Err(i32::try_from(status).unwrap_or(BAD_ABI)) } else { Ok(()) }
    }

    /// Return whether visible menu state changed. Read/no-change calls never
    /// allocate backing or invalidate the compositor.
    pub(crate) fn request(&mut self, owner: &OwnerRef, call: &mut MenuCall,
                          resources: Resources<'_>)
                          -> Result<bool, Status>
    {
        self.bind(owner)?;
        if call.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        if call.handle != 0
        {
            if matches!(call.operation, INSTALL | CLEAR | KEY)
            {
                return Err(BAD_ARGUMENT);
            }
            let menu = self.objects.get(call.handle).ok_or(NOT_FOUND)?;
            if self.objects.owner(call.handle) != Some(owner.owner())
            {
                return Err(ACCESS_DENIED);
            }
            call.menu_id = menu.state.menu_id;
        }
        match call.operation
        {
            FIND =>
            {
                call.handle = self.menu_id(call.menu_id).ok_or(NOT_FOUND)?;
                Ok(false)
            }
            DISPOSE =>
            {
                let id = call.handle;
                self.objects.get(id).ok_or(NOT_FOUND)?;
                let entry = self.entry;
                let gate = self.gate;
                {
                    let (bar, values) =
                        self.objects.state_values(owner.owner().incarnation()).ok_or(NOT_FOUND)?;
                    let index = bar.order.iter().position(|other| *other == id).ok_or(NOT_FOUND)?;
                    bar.reflow.clear();
                    let mut left = values.get(id).ok_or(NOT_FOUND)?.state.title_bounds.left;
                    for &next in &bar.order[index + 1..]
                    {
                        let menu = values.get(next).ok_or(NOT_FOUND)?;
                        bar.scratch.as_mut().clone_from(menu.state.as_ref());
                        bar.scratch.title_bounds.left = left;
                        Self::measure_with(entry, gate, &mut bar.scratch)?;
                        let layout = Layout::read(&bar.scratch);
                        // Measure owns geometry only. A definition that changes
                        // its inputs is refused before any visible publication.
                        Layout::read(&menu.state).apply(&mut bar.scratch);
                        if bar.scratch.as_ref() != menu.state.as_ref()
                        {
                            return Err(BAD_ABI);
                        }
                        if bar.reflow.len() == bar.reflow.capacity()
                        {
                            return Err(NO_MEMORY);
                        }
                        left = layout.title.right;
                        bar.reflow.push((next, layout));
                    }
                }
                // All fallible definition calls precede invalidation. Scratch
                // and geometry capacity were prepared when menus were installed.
                let removed = self.objects.remove(id).ok_or(NOT_FOUND)?;
                self.unpublish(id);
                drop(removed);
                while let Some((next, layout)) = self.bar_mut().and_then(|bar| bar.reflow.pop())
                {
                    if let Some(menu) = self.objects.get_mut(next)
                    {
                        layout.apply(&mut menu.state);
                    }
                }
                Ok(true)
            }
            KEY =>
            {
                let selection = self.key(u8::try_from(call.value).map_err(|_| BAD_ARGUMENT)?)
                                    .ok_or(NOT_FOUND)?;
                call.menu_id = selection.menu_id;
                call.item = selection.item;
                call.command = selection.command;
                Ok(false)
            }
            INSTALL =>
            {
                let mut state = new_state()?;
                match resources.get(call.scope,
                                    ResourceKey { kind: *b"MENU",
                                                  id:   call.menu_id })
                {
                    Ok((_, r)) => parse_menu(&r.data, &mut state)?,
                    Err(NOT_FOUND) =>
                    {
                        let (_, r) = resources.get(call.scope,
                                                   ResourceKey { kind: *b"fmnu",
                                                                 id:   call.menu_id })?;
                        parse_fmnu(&r.data, &mut state)?;
                    }
                    Err(error) => return Err(error)
                }
                if self.menu_id(state.menu_id).is_some()
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
                state.title_bounds.left = self.bar()
                                              .and_then(|bar| bar.order.last())
                                              .and_then(|id| self.objects.get(*id))
                                              .map_or(10, |menu| menu.state.title_bounds.right);
                self.measure(&mut state)?;
                if let Some(bar) = self.bar_mut()
                {
                    bar.order.try_reserve(1).map_err(|_| NO_MEMORY)?;
                    bar.reflow
                       .try_reserve(bar.order
                                       .len()
                                       .saturating_add(1)
                                       .saturating_sub(bar.reflow.len()))
                       .map_err(|_| NO_MEMORY)?;
                }
                let resource = state.menu_id;
                // Bar::new prepares the first order slot; later growth is
                // reserved above. Publishing this payload cannot leave an
                // untracked menu after an allocation refusal.
                let id = self.objects
                             .insert_with(owner, |id| {
                                 Menu { id,
                                        state }
                             })
                             .map_err(pool_status)?;
                let Some(bar) = self.bar_mut()
                else
                {
                    // An invalidate-first walk owns any already-dead ticket.
                    // Ordinary rollback may remove only our still-live value.
                    drop(self.objects.remove(id));
                    return Err(SERVICE_BUSY);
                };
                bar.order.push(id);
                call.handle = id;
                call.menu_id = resource;
                Ok(true)
            }
            CLEAR => Ok(self.clear()),
            LAYOUT =>
            {
                let menu = self.objects
                               .get(self.menu_id(call.menu_id).ok_or(NOT_FOUND)?)
                               .ok_or(NOT_FOUND)?;
                call.title_bounds = menu.state.title_bounds;
                call.popup_bounds = menu.state.popup_bounds;
                Ok(false)
            }
            SET_ITEM | TRUNCATE =>
            {
                let id = self.menu_id(call.menu_id).ok_or(NOT_FOUND)?;
                let current = &self.objects.get(id).ok_or(NOT_FOUND)?.state;
                if call.operation == TRUNCATE
                {
                    if call.value > current.item_count
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    if call.value == current.item_count
                    {
                        return Ok(false);
                    }
                }
                else
                {
                    let item = call.replacement;
                    if call.item == 0
                       || call.item as usize > MAX_ITEMS
                       || u32::from(call.item) > current.item_count + 1
                       || item.text_length as usize > item.text.len()
                       || item.enabled > 1
                       || item.checked > 1
                       || item.separator > 1
                       || item.icon.present != 0
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    core::str::from_utf8(&item.text[..item.text_length as usize]).map_err(|_| {
                                                                                     BAD_ARGUMENT
                                                                                 })?;
                    let slot = &current.items[call.item as usize - 1];
                    if slot.text == item.text
                       && slot.text_length == item.text_length
                       && slot.command == item.command
                       && slot.enabled == item.enabled
                       && slot.checked == item.checked
                       && slot.key == item.key
                       && slot.mark == item.mark
                       && slot.style == item.style
                       && slot.separator == item.separator
                       && u32::from(call.item) <= current.item_count
                    {
                        return Ok(false);
                    }
                }
                // No-change exits precede both allocation and MDEF measurement.
                let mut state = new_state()?;
                state.as_mut().clone_from(current.as_ref());
                if call.operation == TRUNCATE
                {
                    state.items[call.value as usize..].fill(MenuItemState::EMPTY);
                    state.item_count = call.value;
                }
                else
                {
                    state.items[call.item as usize - 1] = call.replacement;
                    state.item_count = state.item_count.max(u32::from(call.item));
                }
                self.measure(&mut state)?;
                self.objects.get_mut(id).ok_or(NOT_FOUND)?.state = state;
                if let Some(bar) = self.bar_mut()
                {
                    bar.cancel();
                }
                Ok(true)
            }
            SET_ENABLED | SET_CHECKED | COMMAND =>
            {
                let id = self.menu_id(call.menu_id).ok_or(NOT_FOUND)?;
                let menu = self.objects.get_mut(id).ok_or(NOT_FOUND)?;
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

    pub(crate) fn clear(&mut self) -> bool
    {
        let changed = self.bar().is_some_and(|bar| !bar.order.is_empty() || bar.tracking);
        while let Some(id) = self.bar_mut().and_then(|bar| bar.order.pop())
        {
            drop(self.objects.remove(id));
        }
        if let Some(bar) = self.bar_mut()
        {
            bar.cancel();
        }
        changed
    }

    fn unpublish(&mut self, id: u64)
    {
        if let Some(bar) = self.bar_mut()
        {
            bar.order.retain(|other| *other != id);
            bar.cancel();
        }
    }

    pub(crate) fn release(&mut self, ticket: ReleasedResource) -> Result<(), CompletionError>
    {
        let Some(id) = self.objects.releasing(&ticket).map(|menu| menu.id)
        else
        {
            return Err(<CompletionError as mac_object_table::registry::CleanupFailure>::registration_mismatch(ticket));
        };
        self.unpublish(id);
        drop(self.objects.release(ticket)?);
        Ok(())
    }

    pub(crate) fn release_storage(&mut self, ticket: ReleasedResource)
                                  -> Result<(), CompletionError>
    {
        self.objects.release_storage(ticket)
    }

    pub fn is_tracking(&self) -> bool
    {
        self.bar().is_some_and(|bar| bar.tracking)
    }

    pub fn begin(&mut self, point: Point) -> bool
    {
        if !(0..20).contains(&point.v)
        {
            return false;
        }
        let Some(bar) = self.bar_mut()
        else
        {
            return false;
        };
        bar.tracking = true;
        self.hover(point);
        true
    }

    pub fn hover(&mut self, point: Point) -> bool
    {
        if !self.is_tracking()
        {
            return false;
        }
        let previous = self.bar().map(|bar| (bar.active, bar.highlighted));
        if (0..20).contains(&point.v)
        {
            let active = self.bar().and_then(|bar| {
                                       bar.order.iter().copied().find(|id| {
                                                                    self.objects
                                                                        .get(*id)
                                                                        .is_some_and(|menu| {
                                                                            menu.state.enabled != 0
                                                                            && menu.state
                                                                                   .title_bounds
                                                                                   .contains(point)
                                                                        })
                                                                })
                                   });
            if let Some(bar) = self.bar_mut()
            {
                bar.active = active;
                bar.highlighted = 0;
            }
        }
        else
        {
            let active = self.bar().and_then(|bar| bar.active);
            let highlighted = active.and_then(|id| self.objects.get_mut(id)).map_or(0, |menu| {
                                                                                invoke(self.entry,
                                                                         self.gate,
                                                                         &mut menu.state,
                                                                         HIT_ITEM,
                                                                         ((u32::from(point.v
                                                                                     as u16)
                                                                           << 16)
                                                                          | u32::from(point.h
                                                                                      as u16))
                                                                         as i64,
                                                                         core::ptr::null_mut())
                                                                  as i16
                                                                            });
            if let Some(bar) = self.bar_mut()
            {
                bar.highlighted = highlighted;
            }
        }
        previous != self.bar().map(|bar| (bar.active, bar.highlighted))
    }

    pub fn release_tracking(&mut self, point: Point) -> Option<Selection>
    {
        if !self.is_tracking()
        {
            return None;
        }
        self.hover(point);
        let selected =
            self.bar()
                .and_then(|bar| bar.active.and_then(|id| self.selection(id, bar.highlighted)));
        if let Some(bar) = self.bar_mut()
        {
            bar.cancel();
        }
        selected
    }

    fn selection(&self, id: u64, item: i16) -> Option<Selection>
    {
        let state = &self.objects.get(id)?.state;
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
        key != 0 && self.bar().is_some_and(|bar| bar.order.iter().filter_map(|id| self.objects.get(*id))
            .any(|menu| menu.state.items[..menu.state.item_count as usize].iter().any(|item| item.key != 0 && item.key.eq_ignore_ascii_case(&key))))
    }

    pub fn key(&self, key: u8) -> Option<Selection>
    {
        self.bar()?.order.iter().copied().find_map(|id| {
            let menu = self.objects.get(id)?;
            menu.state.items[..menu.state.item_count as usize].iter()
                .position(|item| item.key != 0 && item.key.eq_ignore_ascii_case(&key) && item.enabled != 0 && item.separator == 0)
                .and_then(|item| self.selection(id, item as i16 + 1))
        })
    }

    pub fn draw(&mut self, canvas: &mut Canvas)
    {
        self.draw_bar(canvas);
        self.draw_popup(canvas);
    }

    pub fn selected_menu(&self) -> Option<i16>
    {
        self.objects.get(self.bar()?.active?).map(|menu| menu.state.menu_id)
    }

    pub fn draw_bar(&mut self, canvas: &mut Canvas)
    {
        canvas.reset_port();
        let count = self.bar().map_or(0, |bar| bar.order.len());
        if count == 0
        {
            draw_empty(self.entry, self.gate, canvas);
            return;
        }
        let active = self.bar().and_then(|bar| bar.active);
        for index in 0..count
        {
            let Some(id) = self.id_at(index)
            else
            {
                continue;
            };
            let Some(menu) = self.objects.get_mut(id)
            else
            {
                continue;
            };
            menu.state.first = u32::from(index == 0);
            menu.state.bar_selected = u32::from(active == Some(id));
            let mut surface = Surface::from_canvas(canvas);
            invoke(self.entry, self.gate, &mut menu.state, DRAW_BAR, 0, &mut surface);
        }
    }

    pub fn draw_popup(&mut self, canvas: &mut Canvas)
    {
        canvas.reset_port();
        let Some(bar) = self.bar()
        else
        {
            return;
        };
        let (active, highlighted) = (bar.active, bar.highlighted);
        if let Some(menu) = active.and_then(|id| self.objects.get_mut(id))
        {
            menu.state.highlighted = highlighted;
            let mut surface = Surface::from_canvas(canvas);
            invoke(self.entry, self.gate, &mut menu.state, DRAW_POPUP, 0, &mut surface);
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
// Keep the large ABI object off nested application/definition stack frames.
// Allocate before parsing and fill it in place; allocation failure is a status.
#[inline(never)]
pub(in crate::toolbox) fn new_state() -> Result<Box<MenuDefinitionState>, Status>
{
    Box::try_new(MenuDefinitionState::default()).map_err(|_| NO_MEMORY)
}
fn parse_menu(bytes: &[u8], state: &mut MenuDefinitionState) -> Result<(), Status>
{
    if bytes.len() < 13
    {
        return Err(BAD_ARGUMENT);
    }
    if bytes[6..8] != [0, 0]
    {
        return Err(NOT_SUPPORTED);
    }
    state.menu_id = i16::from_be_bytes(bytes[..2].try_into().unwrap());
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
    Ok(())
}
fn parse_fmnu(bytes: &[u8], state: &mut MenuDefinitionState) -> Result<(), Status>
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
    state.menu_id = i16::try_from(id).map_err(|_| BAD_ARGUMENT)?;
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
    Ok(())
}

// Empty bars need the definition's full ABI record too. Keep that rare temporary
// out of the compositor and pointer-poll stack frames.
#[inline(never)]
fn draw_empty(entry: DefinitionEntry, gate: TrapGate, canvas: &mut Canvas)
{
    let mut state = MenuDefinitionState { first: 1,
                                          bar_bounds: Rect::new(0, 0, canvas.width as i16, 20),
                                          ..Default::default() };
    let mut surface = Surface::from_canvas(canvas);
    invoke(entry, gate, &mut state, DRAW_BAR, 0, &mut surface);
}
fn invoke(entry: DefinitionEntry, gate: TrapGate, state: &mut MenuDefinitionState, message: i16,
          param: i64, surface: *mut Surface)
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
                                    gate };
    crate::toolbox::resources::definitions::invoke(entry, &mut call)
}
