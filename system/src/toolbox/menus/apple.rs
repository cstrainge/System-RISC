//! Desktop Apple menu. App rows retain their generation-checked command owner.
use super::*;
pub(super) const MAX_APP_ITEMS: usize = MAX_ITEMS - 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action
{
    AboutSystem,
    Application
    {
        handle:    u64,
        selection: Selection
    }
}

pub(crate) struct SystemBar
{
    template:    Box<MenuDefinitionState>,
    state:       Box<MenuDefinitionState>,
    scratch:     Box<MenuDefinitionState>,
    rows:        [Option<Action>; MAX_ITEMS],
    owner:       u64,
    source:      Option<(u64, u64)>,
    entry:       DefinitionEntry,
    gate:        TrapGate,
    tracking:    bool,
    popup:       bool,
    highlighted: i16
}
impl SystemBar
{
    pub(crate) fn new(entry: DefinitionEntry, gate: TrapGate, resources: Resources<'_>)
                      -> Result<Self, Status>
    {
        use mac_object_table::shared::InvocationGate as _;
        crate::toolbox::handles::InvocationGate::with_system_allocation(|| {
            let mut template = new_state()?;
            let (_, resource) = resources.get(resources::SYSTEM,
                                              ResourceKey { kind: *b"MENU",
                                                            id:   SYSTEM_APPLE_MENU_ID })?;
            parse_menu(&resource.data, &mut template)?;
            let (_, commands) = resources.get(resources::SYSTEM,
                                              ResourceKey { kind: *b"MCMD",
                                                            id:   SYSTEM_APPLE_MENU_ID })?;
            if commands.data.as_slice() != b"asys"
            {
                return Err(BAD_ARGUMENT);
            }
            template.items[0].command = u32::from_be_bytes(*b"asys");
            if !is_apple(&template)
               || template.item_count != 1
               || template.items[0].command != u32::from_be_bytes(*b"asys")
            {
                return Err(BAD_ARGUMENT);
            }
            template.title_bounds.left = 10;
            Manager::measure_with(entry, gate, &mut template)?;
            let mut state = new_state()?;
            state.as_mut().clone_from(template.as_ref());
            Ok(Self { template,
                      state,
                      scratch: new_state()?,
                      rows: [None; MAX_ITEMS],
                      owner: 0,
                      source: None,
                      entry,
                      gate,
                      tracking: false,
                      popup: true,
                      highlighted: 0 })
        })
    }

    pub(crate) fn title_bounds(&self) -> Rect
    {
        self.template.title_bounds
    }

    pub(crate) fn is_tracking(&self) -> bool
    {
        self.tracking
    }

    pub(crate) fn bar_selected(&self) -> bool
    {
        self.tracking && (self.popup || self.highlighted != 0)
    }

    pub(crate) fn cancel(&mut self)
    {
        self.tracking = false;
        self.highlighted = 0;
        self.popup = true;
    }

    pub(crate) fn reset(&mut self, owner: u64)
    {
        self.cancel();
        self.state.as_mut().clone_from(self.template.as_ref());
        self.rows.fill(None);
        self.rows[0] = Some(Action::AboutSystem);
        self.owner = owner;
        self.source = None;
    }

    pub(super) fn refresh(&mut self, owner: u64, menu: Option<&Menu>) -> Result<bool, Status>
    {
        let source = menu.map(|menu| (menu.id, menu.revision));
        if self.owner == owner && self.source == source
        {
            return Ok(false);
        }
        if menu.is_none()
        {
            self.reset(owner);
            return Ok(true);
        }
        let state = &mut *self.scratch;
        state.clone_from(self.template.as_ref());
        state.items.fill(MenuItemState::EMPTY);
        state.item_count = 0;
        let mut rows = [None; MAX_ITEMS];
        if let Some(menu) = menu
        {
            if menu.state.item_count as usize > MAX_APP_ITEMS
            {
                return Err(NO_MEMORY);
            }
            let items = &menu.state.items[..menu.state.item_count as usize];
            let first = items.iter().position(|item| item.separator == 0);
            let last = items.iter().rposition(|item| item.separator == 0);
            if let (Some(first), Some(last)) = (first, last)
            {
                for (index, item) in items.iter().enumerate().take(last + 1).skip(first)
                {
                    if item.separator != 0
                       && state.item_count != 0
                       && state.items[state.item_count as usize - 1].separator != 0
                    {
                        continue;
                    }
                    let at = state.item_count as usize;
                    state.items[at] = *item;
                    state.items[at].enabled &= menu.state.enabled;
                    if item.separator == 0
                    {
                        rows[at] = Some(Action::Application { handle:    menu.id,
                                                              selection:
                                                                  Selection { menu_id: menu.state
                                                                                           .menu_id,
                                                                              item:    index as u16
                                                                                       + 1,
                                                                              command: item.command } });
                    }
                    state.item_count += 1;
                }
                state.items[state.item_count as usize].separator = 1;
                state.items[state.item_count as usize].enabled = 0;
                state.item_count += 1;
            }
        }
        let at = state.item_count as usize;
        state.items[at] = self.template.items[0];
        rows[at] = Some(Action::AboutSystem);
        state.item_count += 1;
        Manager::measure_with(self.entry, self.gate, state)?;
        core::mem::swap(&mut self.state, &mut self.scratch);
        self.rows = rows;
        self.owner = owner;
        self.source = source;
        self.cancel();
        Ok(true)
    }

    pub(super) fn apply_layout(&self, app: &mut MenuDefinitionState)
    {
        app.title_bounds = self.state.title_bounds;
        app.popup_bounds = self.state.popup_bounds;
        for (row, action) in self.state.items.iter().zip(self.rows)
        {
            if let Some(Action::Application { selection,
                                              .. }) = action
            {
                app.items[selection.item as usize - 1].bounds = row.bounds;
            }
        }
    }

    pub(crate) fn query(&self, call: &mut MenuCall) -> Result<(), Status>
    {
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
                let at = usize::from(call.item) - 1;
                let row = self.state
                              .items
                              .get(at)
                              .filter(|_| at < self.state.item_count as usize)
                              .ok_or(NOT_FOUND)?;
                call.command = row.command;
                call.value = u32::from(row.enabled != 0 && row.separator == 0)
                             | (u32::from(row.checked != 0) << 1);
            }
            _ => return Err(ACCESS_DENIED)
        }
        Ok(())
    }

    pub(crate) fn begin(&mut self, point: Point) -> bool
    {
        if !self.state.title_bounds.contains(point)
        {
            return false;
        }
        self.tracking = true;
        self.popup = true;
        self.highlighted = 0;
        true
    }

    pub(crate) fn hover(&mut self, point: Point) -> bool
    {
        let previous = self.highlighted;
        self.highlighted = invoke(self.entry,
                                  self.gate,
                                  &mut self.state,
                                  HIT_ITEM,
                                  ((u32::from(point.v as u16) << 16) | u32::from(point.h as u16))
                                  as i64,
                                  core::ptr::null_mut()) as i16;
        self.highlighted != previous
    }

    pub(crate) fn release(&mut self, point: Point) -> Option<(i16, Action)>
    {
        self.hover(point);
        let selected = self.selected();
        if selected.is_none()
        {
            self.cancel();
        }
        selected
    }

    fn selected(&self) -> Option<(i16, Action)>
    {
        let at = usize::try_from(self.highlighted).ok()?.checked_sub(1)?;
        let row = self.state.items.get(at)?;
        if at >= self.state.item_count as usize || row.enabled == 0 || row.separator != 0
        {
            return None;
        }
        Some((self.highlighted, self.rows[at]?))
    }

    pub(crate) fn begin_key(&mut self, selection: Selection) -> Option<(i16, Action)>
    {
        let (at, action) = self.rows.iter().enumerate().find_map(|(at, action)| {
                                                            match action
                                             {
                                                 Some(action
                                                      @ Action::Application { selection:
                                                                                current,
                                                                            .. })
                                                     if *current == selection =>
                                                 {
                                                     Some((at, *action))
                                                 }
                                                 _ => None
                                             }
                                                        })?;
        self.tracking = true;
        self.popup = false;
        self.highlighted = 0;
        Some((at as i16 + 1, action))
    }

    pub(crate) fn feedback_bounds(&self, item: i16, keyboard: bool) -> Option<Rect>
    {
        if keyboard
        {
            let r = self.state.title_bounds;
            return Some(Rect::new(r.left - 1, 1, r.right + 4, 19));
        }
        self.state.items.get(usize::try_from(item).ok()?.checked_sub(1)?).map(|row| row.bounds)
    }

    pub(crate) fn feedback_valid(&self, owner: u64, item: i16, action: Action) -> bool
    {
        let Some(at) = usize::try_from(item).ok().and_then(|n| n.checked_sub(1))
        else
        {
            return false;
        };
        self.owner == owner
        && self.tracking
        && self.rows.get(at) == Some(&Some(action))
        && self.state.items[at].enabled != 0
        && self.state.items[at].separator == 0
    }

    pub(crate) fn highlight(&mut self, item: i16)
    {
        self.highlighted = item;
    }

    pub(crate) fn paint_feedback(&mut self, item: i16, keyboard: bool, canvas: &mut Canvas<'_>)
                                 -> Result<(), Status>
    {
        let clip = self.feedback_bounds(item, keyboard).ok_or(BAD_ARGUMENT)?;
        self.state.highlighted = self.highlighted;
        self.state.first = 0;
        self.state.bar_selected = u32::from(self.bar_selected());
        let mut surface = Surface::from_canvas(canvas);
        surface.clip = clip;
        let status = invoke(self.entry,
                            self.gate,
                            &mut self.state,
                            if keyboard { DRAW_BAR } else { DRAW_ITEM },
                            i64::from(item),
                            &mut surface);
        if status == 0 { Ok(()) } else { Err(status as Status) }
    }

    pub(crate) fn draw_bar(&mut self, canvas: &mut Canvas<'_>) -> Result<(), PaintError>
    {
        self.state.first = 1;
        self.state.bar_selected = u32::from(self.bar_selected());
        canvas.reset_port();
        let mut surface = Surface::from_canvas(canvas);
        painted(SYSTEM_APPLE_MENU_ID,
                invoke(self.entry, self.gate, &mut self.state, DRAW_BAR, 0, &mut surface))
    }

    pub(crate) fn draw_popup(&mut self, canvas: &mut Canvas<'_>) -> Result<(), PaintError>
    {
        if !self.tracking || !self.popup
        {
            return Ok(());
        }
        self.state.highlighted = self.highlighted;
        canvas.reset_port();
        let mut surface = Surface::from_canvas(canvas);
        painted(SYSTEM_APPLE_MENU_ID,
                invoke(self.entry, self.gate, &mut self.state, DRAW_POPUP, 0, &mut surface))
    }
}
