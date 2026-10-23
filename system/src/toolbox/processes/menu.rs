//! System-owned application-menu policy. MDEF owns drawing and hit geometry.
#![forbid(unsafe_code)]

use alloc::boxed::Box;

use mac_abi::{menus::*, *};

use crate::toolbox::{quickdraw::{Canvas, SurfaceExt},
                     resources::manager::Manager as Resources};

pub const RESOURCE_ID: i16 = 32760;
pub const MENU_ID: i16 = -16489;
const FIXED_ROWS: usize = 4;
//64px commands +21*18px applications, top20 and the popup shadow fit480px.
pub const MAX_APPLICATIONS: usize = 21;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action
{
    Activate(u32),
    HideCurrent,
    HideOthers,
    ShowAll
}

/// A borrowed process snapshot; names are copied before returning to the caller.
#[derive(Clone, Copy)]
pub struct Entry<'a>
{
    pub id:     u32,
    pub name:   &'a str,
    pub hidden: bool,
    pub icon:   MenuIcon
}

pub struct ApplicationMenu
{
    state:       Box<MenuDefinitionState>,
    actions:     [Option<Action>; MAX_ITEMS],
    labels:      [[u8; INLINE_STRING_BYTES]; 3],
    lengths:     [usize; 3],
    definition:  DefinitionEntry,
    gate:        TrapGate,
    tracking:    bool,
    highlighted: i16
}

impl ApplicationMenu
{
    /// Labels are resident System resources, independent of the foreground app.
    /// STR# 32760: Hide prefix, Hide Others, Show All.
    pub fn new(definition: DefinitionEntry, gate: TrapGate, resources: Resources<'_>)
               -> Result<Self, Status>
    {
        let mut labels = [[0; INLINE_STRING_BYTES]; 3];
        let mut lengths = [0; 3];
        for index in 0..3
        {
            let mut call = resources::ResourceCall { operation: resources::STRING,
                                                     scope: resources::SYSTEM,
                                                     key: ResourceKey { kind: *b"STR#",
                                                                        id:   RESOURCE_ID },
                                                     index: index as u32,
                                                     ..Default::default() };
            resources.request(&mut call, "", &mut labels[index])?;
            lengths[index] = call.transferred;
        }
        Ok(Self { state: crate::toolbox::menus::new_state()?,
                  actions: [None; MAX_ITEMS],
                  labels,
                  lengths,
                  definition,
                  gate,
                  tracking: false,
                  highlighted: 0 })
    }

    fn invoke(&self, state: &mut MenuDefinitionState, message: i16, param: i64,
              surface: *mut Surface)
              -> i64
    {
        invoke(self.definition, self.gate, state, message, param, surface)
    }

    /// Publish a complete snapshot, preserving old state if input or sizing fails.
    /// Applications appear alphabetically, as in the original Process Manager.
    /// A refresh cancels tracking so a release cannot select a stale row.
    pub fn refresh(&mut self, entries: &[Entry<'_>], foreground: u32) -> Result<(), Status>
    {
        if entries.len() > MAX_APPLICATIONS
        {
            return Err(NO_MEMORY);
        }
        if entries.iter().enumerate().any(|(index, entry)| {
                                         entry.id == 0
                                         || entry.name.is_empty()
                                         || entries[..index].iter()
                                                            .any(|previous| previous.id == entry.id)
                                     })
        {
            return Err(BAD_ARGUMENT);
        }
        let active = entries.iter().find(|entry| entry.id == foreground).ok_or(NOT_FOUND)?;
        let mut state = crate::toolbox::menus::new_state()?;
        state.menu_id = MENU_ID;
        state.profile = APPLICATION_PROFILE;
        state.title_icon = icon(active.icon);
        state.item_count = (FIXED_ROWS + entries.len()) as u32;
        copy_text(&mut state.title, &mut state.title_length, active.name.as_bytes())?;
        let mut actions = [None; MAX_ITEMS];
        for (index, action) in
            [Action::HideCurrent, Action::HideOthers, Action::ShowAll].into_iter().enumerate()
        {
            copy_text(&mut state.items[index].text,
                      &mut state.items[index].text_length,
                      &self.labels[index][..self.lengths[index]])?;
            actions[index] = Some(action);
            state.items[index].command = match action
            {
                Action::HideCurrent => u32::from_be_bytes(*b"hcur"),
                Action::HideOthers => u32::from_be_bytes(*b"hoth"),
                Action::ShowAll => u32::from_be_bytes(*b"sall"),
                Action::Activate(id) => id
            };
        }
        let prefix = self.lengths[0];
        let length = prefix.checked_add(active.name.len()).ok_or(BAD_ARGUMENT)?;
        if length > INLINE_STRING_BYTES
        {
            return Err(BAD_ARGUMENT);
        }
        state.items[0].text[prefix..length].copy_from_slice(active.name.as_bytes());
        state.items[0].text_length = length as u32;
        let other_visible = entries.iter().any(|entry| entry.id != foreground && !entry.hidden);
        state.items[0].enabled = u32::from(other_visible);
        state.items[1].enabled = u32::from(other_visible);
        state.items[2].enabled = u32::from(entries.iter().any(|entry| entry.hidden));
        state.items[3].separator = 1;
        state.items[3].enabled = 0;
        let mut order = [0; MAX_APPLICATIONS];
        for (index, slot) in order[..entries.len()].iter_mut().enumerate()
        {
            *slot = index;
        }
        order[..entries.len()].sort_unstable_by(|left, right| {
                                  entries[*left].name
                                                .chars()
                                                .flat_map(char::to_lowercase)
                                                .cmp(entries[*right].name
                                                                    .chars()
                                                                    .flat_map(char::to_lowercase))
                                                .then_with(|| {
                                                    entries[*left].id.cmp(&entries[*right].id)
                                                })
                              });
        for (index, entry) in
            order[..entries.len()].iter().map(|index| &entries[*index]).enumerate()
        {
            let row = index + FIXED_ROWS;
            copy_text(&mut state.items[row].text,
                      &mut state.items[row].text_length,
                      entry.name.as_bytes())?;
            state.items[row].checked = u32::from(entry.id == foreground);
            state.items[row].icon = icon(entry.icon);
            // Original hidden apps retain normal Chicago text and dim the icon.
            state.items[row].icon.flags |= u16::from(entry.hidden);
            actions[row] = Some(Action::Activate(entry.id));
            state.items[row].command = entry.id;
        }
        let mut pixel = [0];
        let mut canvas = Canvas::new(&mut pixel, 1, 1);
        let mut surface = Surface::from_canvas(&mut canvas);
        let status = self.invoke(&mut state, MEASURE, 0, &mut surface);
        if status < 0
        {
            return Err(status as Status);
        }
        self.state = state;
        self.actions = actions;
        self.cancel();
        Ok(())
    }

    pub fn title_bounds(&self) -> Rect
    {
        self.state.title_bounds
    }

    pub fn popup_bounds(&self) -> Rect
    {
        self.state.popup_bounds
    }

    /// System-menu state is observable through the normal Menu Manager SDK.
    /// Applications cannot mutate this menu or replace its actions/resources.
    pub fn query(&self, call: &mut MenuCall) -> Result<(), Status>
    {
        if call.menu_id != MENU_ID || self.state.item_count == 0
        {
            return Err(NOT_FOUND);
        }
        match call.operation
        {
            LAYOUT =>
            {
                call.title_bounds = self.state.title_bounds;
                call.popup_bounds = self.state.popup_bounds;
            }
            COMMAND if call.item == 0 =>
            {
                call.value = self.state.item_count;
                call.command = 0;
            }
            COMMAND =>
            {
                let item = self.state
                               .items
                               .get(call.item as usize - 1)
                               .filter(|_| u32::from(call.item) <= self.state.item_count)
                               .ok_or(NOT_FOUND)?;
                call.command = item.command;
                call.value = u32::from(item.enabled != 0 && item.separator == 0)
                             | (u32::from(item.checked != 0) << 1);
            }
            _ => return Err(NOT_SUPPORTED)
        }
        Ok(())
    }

    pub fn is_tracking(&self) -> bool
    {
        self.tracking
    }

    pub fn begin(&mut self, point: Point) -> bool
    {
        if self.state.item_count == 0 || !self.state.title_bounds.contains(point)
        {
            return false;
        }
        self.tracking = true;
        self.highlighted = 0;
        true
    }

    pub fn hover(&mut self, point: Point)
    {
        if !self.tracking
        {
            return;
        }
        self.highlighted = invoke(self.definition,
                                  self.gate,
                                  &mut self.state,
                                  HIT_ITEM,
                                  (((point.v as u16 as u32) << 16) | point.h as u16 as u32) as i64,
                                  core::ptr::null_mut()) as i16;
    }

    pub fn release(&mut self, point: Point) -> Option<Action>
    {
        if !self.tracking
        {
            return None;
        }
        self.hover(point);
        let selected =
            usize::try_from(self.highlighted).ok()
                                             .and_then(|row| row.checked_sub(1))
                                             .filter(|row| *row < self.state.item_count as usize)
                                             .filter(|row| self.state.items[*row].enabled != 0)
                                             .and_then(|row| self.actions[row]);
        self.cancel();
        selected
    }

    pub fn cancel(&mut self)
    {
        self.tracking = false;
        self.highlighted = 0;
    }

    pub fn draw_bar(&mut self, canvas: &mut Canvas<'_>)
    {
        self.state.first = 0;
        self.state.bar_selected = u32::from(self.tracking);
        canvas.reset_port();
        let mut surface = Surface::from_canvas(canvas);
        invoke(self.definition, self.gate, &mut self.state, DRAW_BAR, 0, &mut surface);
    }

    pub fn draw_popup(&mut self, canvas: &mut Canvas<'_>)
    {
        if !self.tracking
        {
            return;
        }
        self.state.highlighted = self.highlighted;
        canvas.reset_port();
        let mut surface = Surface::from_canvas(canvas);
        invoke(self.definition, self.gate, &mut self.state, DRAW_POPUP, 0, &mut surface);
    }
}

fn copy_text(output: &mut [u8], length: &mut u32, text: &[u8]) -> Result<(), Status>
{
    if text.len() > output.len() || core::str::from_utf8(text).is_err()
    {
        return Err(BAD_ARGUMENT);
    }
    output[..text.len()].copy_from_slice(text);
    *length = text.len() as u32;
    Ok(())
}

fn icon(icon: MenuIcon) -> MenuIcon
{
    if icon.present == 0
    {
        // The standard System application icon remains valid if an app has no
        // registered icon family yet, including during initial process startup.
        MenuIcon::new(resources::SYSTEM, -3996)
    }
    else
    {
        icon
    }
}

fn invoke(definition: DefinitionEntry, gate: TrapGate, state: &mut MenuDefinitionState,
          message: i16, param: i64, surface: *mut Surface)
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
    crate::toolbox::resources::definitions::invoke(definition, &mut call)
}
