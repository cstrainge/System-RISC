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
    Activate(u64),
    HideCurrent,
    HideOthers,
    ShowAll
}

/// A borrowed process snapshot; names are copied before returning to the caller.
#[derive(Clone, Copy)]
pub struct Entry<'a>
{
    pub id:     u64,
    pub name:   &'a str,
    pub hidden: bool,
    pub icon:   MenuIcon
}

pub struct ApplicationMenu
{
    state:       Box<MenuDefinitionState>,
    scratch:     Box<MenuDefinitionState>,
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
                  scratch: crate::toolbox::menus::new_state()?,
                  actions: [None; MAX_ITEMS],
                  labels,
                  lengths,
                  definition,
                  gate,
                  tracking: false,
                  highlighted: 0 })
    }

    /// Publish a complete snapshot, preserving old state if input or sizing fails.
    /// Applications appear alphabetically, as in the original Process Manager.
    /// A refresh cancels tracking so a release cannot select a stale row.
    pub fn refresh(&mut self, entries: &[Entry<'_>], foreground: u64) -> Result<(), Status>
    {
        self.cancel();
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
        let state = &mut *self.scratch;
        *state = MenuDefinitionState::default();
        state.menu_id = MENU_ID;
        state.profile = APPLICATION_PROFILE;
        state.bar_bounds = Rect::new(0, 0, crate::display::WIDTH as i16, 20);
        state.title_icon = icon(active.icon);
        state.item_count = (FIXED_ROWS + entries.len()) as u32;
        copy_text(&mut state.title, &mut state.title_length, active.name.as_bytes())?;
        let mut actions = [None; MAX_ITEMS];
        for (index, (action, command)) in [(Action::HideCurrent, *b"hcur"),
                                           (Action::HideOthers, *b"hoth"),
                                           (Action::ShowAll, *b"sall")].into_iter()
                                                                       .enumerate()
        {
            copy_text(&mut state.items[index].text,
                      &mut state.items[index].text_length,
                      &self.labels[index][..self.lengths[index]])?;
            actions[index] = Some(action);
            state.items[index].command = u32::from_be_bytes(command);
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
            // MDEF reports a selected row; the resident action retains the full
            // owner incarnation. A menu's u32 command field is not an app ID.
            state.items[row].command = (row + 1) as u32;
        }
        let mut pixel = [0];
        let mut canvas = Canvas::new(&mut pixel, 1, 1);
        let mut surface = Surface::from_canvas(&mut canvas);
        let status = invoke(self.definition, self.gate, state, MEASURE, 0, &mut surface);
        if status < 0
        {
            return Err(status as Status);
        }
        core::mem::swap(&mut self.state, &mut self.scratch);
        self.actions = actions;
        Ok(())
    }

    /// The foreground saved context exists, but its prepared thread has not
    /// been published. Remove prior-owner actions rather than retaining them.
    pub fn suspend(&mut self)
    {
        self.cancel();
        self.actions.fill(None);
        self.state.item_count = 0;
        self.state.title_length = 0;
        self.state.title_icon = Default::default();
        self.state.title_bounds = Rect::ZERO;
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

    pub fn hover(&mut self, point: Point) -> bool
    {
        if !self.tracking
        {
            return false;
        }
        let previous = self.highlighted;
        self.highlighted = invoke(self.definition,
                                  self.gate,
                                  &mut self.state,
                                  HIT_ITEM,
                                  (((point.v as u16 as u32) << 16) | point.h as u16 as u32) as i64,
                                  core::ptr::null_mut()) as i16;
        previous != self.highlighted
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
        if selected.is_none()
        {
            self.cancel();
        }
        selected
    }

    pub(crate) fn feedback_target(&self) -> Option<(i16, Rect)>
    {
        let index = usize::try_from(self.highlighted).ok()?.checked_sub(1)?;
        Some((self.highlighted, self.state.items.get(index)?.bounds))
    }

    pub(crate) fn feedback_valid(&self, item: i16, action: Action) -> bool
    {
        let Some(index) = usize::try_from(item).ok().and_then(|row| row.checked_sub(1))
        else
        {
            return false;
        };
        self.tracking
        && index < self.state.item_count as usize
        && self.state.items[index].enabled != 0
        && self.actions[index] == Some(action)
    }

    pub(crate) fn feedback_highlight(&mut self, item: i16)
    {
        self.highlighted = item;
    }

    pub(crate) fn paint_feedback(&mut self, item: i16, canvas: &mut Canvas<'_>)
                                 -> Result<(), Status>
    {
        let index =
            usize::try_from(item).ok().and_then(|row| row.checked_sub(1)).ok_or(BAD_ARGUMENT)?;
        self.state.highlighted = self.highlighted;
        let mut surface = Surface::from_canvas(canvas);
        surface.clip = self.state.items.get(index).ok_or(BAD_ARGUMENT)?.bounds;
        let status = invoke(self.definition,
                            self.gate,
                            &mut self.state,
                            DRAW_ITEM,
                            i64::from(item),
                            &mut surface);
        if status == 0 { Ok(()) } else { Err(status as Status) }
    }

    pub fn cancel(&mut self)
    {
        self.tracking = false;
        self.highlighted = 0;
    }

    pub(crate) fn draw_bar(&mut self, canvas: &mut Canvas<'_>)
                           -> Result<(), crate::toolbox::menus::PaintError>
    {
        self.state.first = 0;
        self.state.bar_selected = u32::from(self.tracking);
        canvas.reset_port();
        let mut surface = Surface::from_canvas(canvas);
        crate::toolbox::menus::painted(MENU_ID,
                                       invoke(self.definition, self.gate, &mut self.state,
                                              DRAW_BAR, 0, &mut surface))
    }

    pub(crate) fn draw_popup(&mut self, canvas: &mut Canvas<'_>)
                             -> Result<(), crate::toolbox::menus::PaintError>
    {
        if !self.tracking
        {
            return Ok(());
        }
        self.state.highlighted = self.highlighted;
        canvas.reset_port();
        let mut surface = Surface::from_canvas(canvas);
        crate::toolbox::menus::painted(MENU_ID,
                                       invoke(self.definition, self.gate, &mut self.state,
                                              DRAW_POPUP, 0, &mut surface))
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
    let mut call = DefinitionCall { reflow: 0,
                                    header: Header::for_type::<DefinitionCall>(),
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

#[cfg(all(test, feature = "test-support", not(target_arch = "riscv64")))]
#[path = "menu/tests.rs"]
mod tests;
